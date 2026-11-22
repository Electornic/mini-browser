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

use mb_agent::{AgentBrowser, ApproveMode, ElementObservation, ElementValue, Observation};
use mb_runtime::dom::NodeId;
use serde_json::{Map, Value, json};

use crate::cli::Options;
use crate::worker::{Reply, Request};

pub const VERSION: u64 = 1;
pub const REDACTED: &str = "<redacted>";
/// Where "Record this session" (⋮ menu) writes when `--log` didn't name a
/// directory: relative to the server's working directory, one
/// `run-<pid>-<n>.jsonl` per recording, as `--log DIR/`.
pub const DEFAULT_RECORD_DIR: &str = "mb-runs/";

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
        "handed back by the person" => "person_handed_back",
        // Took it over, then "I won't do it": a refusal, replayed as one.
        "taken over, not done" => "person_denied",
        "filled in by the person" => "person_filled",
        "declined" => "person_declined",
        // The person changed the allowed sites while the card was up.
        crate::worker::SITE_REMOVED => "person_changed_sites",
        _ => return None,
    })
}

/// What a step line records besides the call and its result.
pub struct StepRecord<'a> {
    pub target: Option<&'a Target>,
    pub person_moved: bool,
    pub resolved_by: Option<&'a str>,
    /// The approval mode the action started under.
    pub approve: ApproveMode,
    /// The allowed sites the action started under.
    pub allow_origins: &'a [String],
}

/// `auto` / `deny` / `ask`, as in the header and `--approve`.
pub fn approve_name(mode: ApproveMode) -> &'static str {
    match mode {
        ApproveMode::Auto => "auto",
        ApproveMode::Deny => "deny",
        ApproveMode::Ask => "ask",
    }
}

pub struct SessionLog {
    path: std::path::PathBuf,
    header: Value,
    // Opened on the first step: a server that never got a tool call (a
    // health check, an unused registration) leaves no empty run behind
    // for `mb-mcp eval` to count as a failure.
    out: Option<BufWriter<File>>,
    // The file actually opened (`run-<pid>-<n>.jsonl` for a directory).
    file: Option<std::path::PathBuf>,
    steps: u64,
}

impl SessionLog {
    pub fn create(path: &Path, options: &Options) -> std::io::Result<Self> {
        // Fail at startup, not at the first step, if the place is unusable.
        let dir = if is_dir_path(path) {
            Some(path)
        } else {
            path.parent().filter(|p| !p.as_os_str().is_empty())
        };
        if let Some(dir) = dir {
            create_private_dir(dir)?;
        }
        let agent = &options.agent;
        let header = json!({
            "type": "session",
            "version": VERSION,
            "mb_mcp": env!("CARGO_PKG_VERSION"),
            "prompts": crate::prompts::fingerprint(),
            "viewport": [agent.viewport_width, agent.viewport_height],
            "approve": approve_name(agent.approve),
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
            file: None,
            steps: 0,
        })
    }

    /// A recording started mid-session (⋮ menu): replay must open the
    /// page it started on, not `--start-url`.
    pub fn starting_at(mut self, url: Option<String>) -> Self {
        if url.is_some() {
            self.header["start_url"] = json!(url);
        }
        self
    }

    /// Steps recorded so far.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// The file being written, once the first step created it.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// What `--log` named: the file, or the directory runs go into.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn step(&mut self, request: &Request, context: StepRecord<'_>, result: Value) {
        self.steps += 1;
        let StepRecord {
            target,
            person_moved,
            resolved_by,
            approve,
            allow_origins,
        } = context;
        let (tool, args) = request_json(request, target);
        let mut line = Map::new();
        line.insert("type".into(), json!("step"));
        line.insert("n".into(), json!(self.steps));
        line.insert("tool".into(), json!(tool));
        line.insert("args".into(), args);
        if let Some(target) = target {
            line.insert("target".into(), target.to_json());
        }
        // The person switched the approval mode from the window: the step
        // ran under that mode, and replay must too. Absent = the header's.
        let approve = approve_name(approve);
        if self.header["approve"] != approve {
            line.insert("approve".into(), json!(approve));
        }
        // Same for the allowed sites ("Allowed sites" dialog).
        if self.header["allow_origins"] != json!(allow_origins) {
            line.insert("allow_origins".into(), json!(allow_origins));
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
            let (file, path) = open_run_log(&self.path)?;
            let mut out = BufWriter::new(file);
            serde_json::to_writer(&mut out, &self.header)?;
            out.write_all(b"\n")?;
            self.out = Some(out);
            self.file = Some(path);
        }
        let out = self.out.as_mut().expect("opened above");
        serde_json::to_writer(&mut *out, value)?;
        out.write_all(b"\n")?;
        // A crash should still leave every finished step on disk.
        out.flush()
    }
}

pub fn is_dir_path(path: &Path) -> bool {
    path.as_os_str().to_string_lossy().ends_with('/') || path.is_dir()
}

/// Open the log for writing. `--log DIR/` (ends with `/` or is a
/// directory): a new `run-<pid>-<n>.jsonl` in it per process, so several
/// runs sharing one MCP registration (G5c) don't overwrite each other —
/// created atomically (`create_new`), retrying on a taken name, never
/// "check then create". A file path is used as is.
pub fn open_run_log(path: &Path) -> std::io::Result<(File, std::path::PathBuf)> {
    if !is_dir_path(path) {
        return Ok((open_private(path, false)?, path.to_path_buf()));
    }
    create_private_dir(path)?;
    let pid = std::process::id();
    for n in 1.. {
        let candidate = path.join(format!("run-{pid}-{n}.jsonl"));
        match open_private(&candidate, true) {
            Ok(file) => return Ok((file, candidate)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    unreachable!("ran out of run numbers")
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

// The log holds what the agent typed and saw: owner-only on Unix, and a
// symlink planted at the path is refused rather than followed.
fn open_private(path: &Path, new: bool) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
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
        let (_, first) = open_run_log(&as_dir).unwrap();
        let (_, second) = open_run_log(&dir).unwrap();
        assert_ne!(first, second);
        assert_eq!(first.parent(), Some(dir.as_path()));
        assert!(
            second
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("run-")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
            // A symlink planted where the log goes is refused, not followed.
            let target = dir.join("elsewhere.txt");
            std::fs::write(&target, "keep").unwrap();
            let link = dir.join("x.jsonl");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(open_run_log(&link).is_err());
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        }
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
