// Session log (`--log PATH`, docs/concept.md "기록·재생 (G5a 설계)"): one
// JSON line per finished tool call, read back by `mb-mcp replay`.
//
// What goes in is chosen so a run recorded in the window (system clock,
// system fonts) can be compared with a headless replay: element roles,
// names and values in document order — never rects, scroll or timing.
// Password values never appear (length only); text typed into a password
// field is "<redacted>".

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use mb_agent::{AgentBrowser, ElementObservation, ElementValue, Observation};
use mb_runtime::dom::NodeId;
use serde_json::{Map, Value, json};

use crate::cli::Options;
use crate::worker::{Reply, Request};

pub const VERSION: u64 = 1;
pub const REDACTED: &str = "<redacted>";

/// The element an id argument pointed at, as replay can find it again
/// when NodeIds differ.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub role: String,
    pub name: String,
    /// Index among elements with the same role and name, document order.
    pub nth: usize,
    pub password: bool,
}

impl Target {
    pub fn to_json(&self) -> Value {
        let mut value = json!({"role": self.role, "name": self.name, "nth": self.nth});
        if self.password {
            value["password"] = json!(true);
        }
        value
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        Some(Target {
            role: value.get("role")?.as_str()?.to_string(),
            name: value.get("name")?.as_str()?.to_string(),
            nth: value.get("nth")?.as_u64()? as usize,
            password: value.get("password").and_then(Value::as_bool) == Some(true),
        })
    }

    pub fn describe(&self) -> String {
        format!("{} {:?}", self.role, self.name)
    }
}

/// How a step that waited for the person ended.
pub fn resolved_by(outcome: &str) -> Option<&'static str> {
    Some(match outcome {
        "approved" => "person_approved",
        "refused" => "person_denied",
        "person took over" => "person_took_over",
        "filled in by the person" => "person_filled",
        "declined" => "person_declined",
        _ => return None,
    })
}

pub struct SessionLog {
    path: std::path::PathBuf,
    header: Value,
    // Opened on the first step: a server that never got a tool call (a
    // health check, an unused registration) leaves no empty run behind
    // for `mb-mcp eval` to count as a failure.
    out: Option<BufWriter<File>>,
    steps: u64,
}

impl SessionLog {
    pub fn create(path: &Path, options: &Options) -> std::io::Result<Self> {
        // Fail at startup, not at the first step, if the place is unusable.
        let is_dir = path.as_os_str().to_string_lossy().ends_with('/') || path.is_dir();
        let parent = if is_dir {
            Some(path)
        } else {
            path.parent().filter(|p| !p.as_os_str().is_empty())
        };
        if let Some(dir) = parent {
            std::fs::create_dir_all(dir)?;
        }
        let agent = &options.agent;
        let header = json!({
            "type": "session",
            "version": VERSION,
            "mb_mcp": env!("CARGO_PKG_VERSION"),
            "prompts": crate::prompts::fingerprint(),
            "viewport": [agent.viewport_width, agent.viewport_height],
            "approve": format!("{:?}", agent.approve).to_lowercase(),
            "allow_origins": agent.allow_origins,
            "seed": agent.seed,
            "clock": format!("{:?}", agent.clock).to_lowercase(),
            "window": options.window,
            "start_url": options.start_url,
        });
        Ok(SessionLog {
            path: path.to_path_buf(),
            header,
            out: None,
            steps: 0,
        })
    }

    pub fn step(
        &mut self,
        request: &Request,
        target: Option<&Target>,
        person_moved: bool,
        resolved_by: Option<&str>,
        result: Value,
    ) {
        self.steps += 1;
        let (tool, args) = request_json(request, target);
        let mut line = Map::new();
        line.insert("type".into(), json!("step"));
        line.insert("n".into(), json!(self.steps));
        line.insert("tool".into(), json!(tool));
        line.insert("args".into(), args);
        if let Some(target) = target {
            line.insert("target".into(), target.to_json());
        }
        if let Some(by) = resolved_by {
            line.insert("resolved_by".into(), json!(by));
        }
        if person_moved {
            line.insert("person_moved".into(), json!(true));
        }
        line.insert("result".into(), result);
        if let Err(error) = self.write(&Value::Object(line)) {
            eprintln!("[mb-mcp] session log: {error}");
        }
    }

    fn write(&mut self, value: &Value) -> std::io::Result<()> {
        if self.out.is_none() {
            let mut out = BufWriter::new(create_private(&run_path(&self.path)?)?);
            serde_json::to_writer(&mut out, &self.header)?;
            out.write_all(b"\n")?;
            self.out = Some(out);
        }
        let out = self.out.as_mut().expect("opened above");
        serde_json::to_writer(&mut *out, value)?;
        out.write_all(b"\n")?;
        // A crash should still leave every finished step on disk.
        out.flush()
    }
}

/// `--log DIR/` (ends with `/` or is a directory): a new
/// `run-<pid>-<n>.jsonl` in it per process, so several runs sharing one
/// MCP registration (G5c) don't overwrite each other. A file path is used
/// as is.
pub fn run_path(path: &Path) -> std::io::Result<std::path::PathBuf> {
    let is_dir = path.as_os_str().to_string_lossy().ends_with('/') || path.is_dir();
    if !is_dir {
        return Ok(path.to_path_buf());
    }
    std::fs::create_dir_all(path)?;
    let pid = std::process::id();
    let mut n = 1;
    loop {
        let candidate = path.join(format!("run-{pid}-{n}.jsonl"));
        if !candidate.exists() {
            return Ok(candidate);
        }
        n += 1;
    }
}

// The log holds what the agent typed and saw: owner-only on Unix.
fn create_private(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    // `mode` only applies to a new file; an old log keeps its bits.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// The element `id` points at right now (no ids issued).
pub fn target_of(browser: &mut AgentBrowser, id: NodeId) -> Option<Target> {
    let snapshot = browser.full_snapshot();
    let element = snapshot.elements.iter().find(|e| e.id == id)?;
    let nth = snapshot
        .elements
        .iter()
        .take_while(|e| e.id != id)
        .filter(|e| e.role == element.role && e.name == element.name)
        .count();
    Some(Target {
        role: element.role.as_str().to_string(),
        name: element.name.clone(),
        nth,
        password: matches!(element.value, Some(ElementValue::Hidden { .. })),
    })
}

/// Replay: the element in `snapshot` that `target` describes.
pub fn find_target(snapshot: &Observation, target: &Target) -> Option<NodeId> {
    snapshot
        .elements
        .iter()
        .filter(|e| e.role.as_str() == target.role && e.name == target.name)
        .nth(target.nth)
        .map(|e| e.id)
}

/// `textbox "Email" = "dev@example.test"`, `checkbox "Done" [x]`,
/// `textbox "Password" = <6 chars>`.
pub fn element_line(element: &ElementObservation) -> String {
    let mut line = format!("{} {:?}", element.role.as_str(), element.name);
    match &element.value {
        Some(ElementValue::Text(value)) => line.push_str(&format!(" = {value:?}")),
        Some(ElementValue::Hidden { chars }) => line.push_str(&format!(" = <{chars} chars>")),
        None => {}
    }
    match element.checked {
        Some(true) => line.push_str(" [x]"),
        Some(false) => line.push_str(" [ ]"),
        None => {}
    }
    line
}

/// What a step left behind: outcome, URL, title and every element.
pub fn result_record(browser: &mut AgentBrowser, reply: &Reply) -> Value {
    let snapshot = browser.full_snapshot();
    let mut record = Map::new();
    match reply {
        Reply::Error(report) => {
            record.insert("ok".into(), json!(false));
            record.insert("error".into(), json!(report.code));
        }
        _ => {
            record.insert("ok".into(), json!(true));
        }
    }
    record.insert("url".into(), json!(snapshot.url));
    record.insert("title".into(), json!(snapshot.title));
    // Visible text too: a regression can be only in the words ("1 of 3
    // done"). Text that changes by itself (clocks) can be skipped at
    // replay with --ignore-text.
    record.insert("text".into(), json!(snapshot.text));
    record.insert(
        "elements".into(),
        json!(
            snapshot
                .elements
                .iter()
                .map(element_line)
                .collect::<Vec<_>>()
        ),
    );
    Value::Object(record)
}

pub fn request_json(request: &Request, target: Option<&Target>) -> (&'static str, Value) {
    match request {
        Request::Navigate { url } => ("navigate", json!({"url": url})),
        Request::Observe { full, cursor } => ("observe", json!({"full": full, "cursor": cursor})),
        Request::Click { id } => ("click", json!({"id": id})),
        Request::Type {
            id,
            text,
            clear,
            submit,
        } => {
            // Fail closed: if we couldn't tell which field it was, it might
            // have been a password field.
            let text = if target.is_none_or(|t| t.password) {
                REDACTED
            } else {
                text.as_str()
            };
            (
                "type",
                json!({"id": id, "text": text, "clear": clear, "submit": submit}),
            )
        }
        Request::Press { key } => ("press", json!({"key": key})),
        Request::Scroll { dy } => ("scroll", json!({"dy": dy})),
        Request::Back => ("back", json!({})),
        Request::Forward => ("forward", json!({})),
        Request::Screenshot => ("screenshot", json!({})),
        Request::WaitFor {
            text,
            id,
            timeout_ms,
        } => (
            "wait_for",
            json!({"text": text, "id": id, "timeout_ms": timeout_ms}),
        ),
    }
}

pub fn request_from_json(tool: &str, args: &Value) -> Result<Request, String> {
    let string = |name: &str| -> Result<String, String> {
        args.get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("{tool}: missing string `{name}`"))
    };
    let optional = |name: &str| args.get(name).and_then(Value::as_str).map(str::to_string);
    let flag = |name: &str| args.get(name).and_then(Value::as_bool).unwrap_or(false);
    Ok(match tool {
        "navigate" => Request::Navigate {
            url: string("url")?,
        },
        "observe" => Request::Observe {
            full: flag("full"),
            cursor: args.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize,
        },
        "click" => Request::Click { id: string("id")? },
        "type" => Request::Type {
            id: string("id")?,
            text: string("text")?,
            clear: flag("clear"),
            submit: flag("submit"),
        },
        "press" => Request::Press {
            key: string("key")?,
        },
        "scroll" => Request::Scroll {
            dy: args
                .get("dy")
                .and_then(Value::as_f64)
                .ok_or("scroll: missing `dy`")?,
        },
        "back" => Request::Back,
        "forward" => Request::Forward,
        "screenshot" => Request::Screenshot,
        "wait_for" => Request::WaitFor {
            text: optional("text"),
            id: optional("id"),
            timeout_ms: args
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(5_000),
        },
        other => return Err(format!("unknown tool {other:?}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_through_json() {
        let requests = [
            Request::Navigate {
                url: "http://localhost:3000/".into(),
            },
            Request::Observe {
                full: true,
                cursor: 40,
            },
            Request::Click { id: "e21".into() },
            Request::Type {
                id: "e3".into(),
                text: "buy milk".into(),
                clear: true,
                submit: true,
            },
            Request::Press {
                key: "Enter".into(),
            },
            Request::Scroll { dy: -600.0 },
            Request::Back,
            Request::Forward,
            Request::Screenshot,
            Request::WaitFor {
                text: Some("Saved".into()),
                id: None,
                timeout_ms: 3_000,
            },
        ];
        let plain = Target {
            role: "textbox".into(),
            name: "New item".into(),
            nth: 0,
            password: false,
        };
        for request in requests {
            let (tool, args) = request_json(&request, Some(&plain));
            assert_eq!(request_from_json(tool, &args).unwrap(), request);
        }
    }

    #[test]
    fn text_typed_into_a_password_field_is_redacted() {
        let target = Target {
            role: "textbox".into(),
            name: "Password".into(),
            nth: 0,
            password: true,
        };
        let (_, args) = request_json(
            &Request::Type {
                id: "e4".into(),
                text: "hunter2".into(),
                clear: false,
                submit: false,
            },
            Some(&target),
        );
        assert_eq!(args["text"], REDACTED);
        assert!(!args.to_string().contains("hunter2"));
        // Unknown field: assume it could be a password.
        let (_, args) = request_json(
            &Request::Type {
                id: "e999".into(),
                text: "hunter2".into(),
                clear: false,
                submit: false,
            },
            None,
        );
        assert_eq!(args["text"], REDACTED);
        assert_eq!(Target::from_json(&target.to_json()), Some(target));
    }

    #[test]
    fn a_directory_log_path_gets_one_file_per_run() {
        let dir = std::env::temp_dir().join(format!("mb-logdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let as_dir = std::path::PathBuf::from(format!("{}/", dir.display()));
        let first = run_path(&as_dir).unwrap();
        std::fs::write(&first, "").unwrap();
        let second = run_path(&dir).unwrap();
        assert_ne!(first, second);
        assert_eq!(first.parent(), Some(dir.as_path()));
        assert!(
            second
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("run-")
        );
        let file = dir.join("x.jsonl");
        assert_eq!(run_path(&file).unwrap(), file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_server_that_did_nothing_leaves_no_log() {
        let dir = std::env::temp_dir().join(format!("mb-lazy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let options = crate::cli::parse(["--log".to_string(), format!("{}/", dir.display())])
            .map(|p| match p {
                crate::cli::Parsed::Run(options) => options,
                _ => unreachable!(),
            })
            .unwrap();
        let log = SessionLog::create(options.log.as_ref().unwrap(), &options).unwrap();
        drop(log);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "no empty run file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
