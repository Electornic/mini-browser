// `mb-mcp replay FILE` (docs/concept.md "기록·재생 (G5a 설계)"): run a
// session log again without an LLM and report the first step whose result
// differs from the recording — the regression point.
//
// Steps go through `worker::execute`, the same code an MCP tool call
// runs, so a replay does exactly what the agent did. Headless, virtual
// clock: the result is deterministic for a given page.

use std::process::ExitCode;

use mb_agent::{
    AgentBrowser, AgentConfig, AgentError, ApproveMode, ClockMode, ObserveOptions, Scope,
};
use mb_runtime::dom::NodeId;
use serde_json::Value;

use crate::cli::ReplayOptions;
use crate::session_log::{REDACTED, Target, find_target, request_from_json, result_record};
use crate::worker::{self, Reply, Request, ToolError};

/// Exit codes (also in `--help`).
const SAME: u8 = 0;
const DIFFERS: u8 = 1;
const BAD_INPUT: u8 = 2;
const NEEDS_PERSON: u8 = 3;

const DIFF_LINES: usize = 20;

pub fn run(options: &ReplayOptions) -> ExitCode {
    match replay(options) {
        Ok(steps) => {
            eprintln!("replay: {steps} steps, all results match the recording");
            ExitCode::from(SAME)
        }
        Err(Stop { code, message }) => {
            eprintln!("{message}");
            ExitCode::from(code)
        }
    }
}

#[derive(Debug)]
pub struct Stop {
    pub code: u8,
    pub message: String,
}

fn stop(code: u8, message: impl Into<String>) -> Stop {
    Stop {
        code,
        message: message.into(),
    }
}

/// Replays every step; `Ok(step count)` when all results match.
pub fn replay(options: &ReplayOptions) -> Result<usize, Stop> {
    let raw = std::fs::read_to_string(&options.file).map_err(|e| {
        stop(
            BAD_INPUT,
            format!("replay: {}: {e}", options.file.display()),
        )
    })?;
    let mut lines = raw.lines().filter(|l| !l.trim().is_empty()).enumerate();
    let header: Value = match lines.next() {
        Some((_, line)) => serde_json::from_str(line)
            .map_err(|e| stop(BAD_INPUT, format!("replay: header: {e}")))?,
        None => return Err(stop(BAD_INPUT, "replay: empty log")),
    };
    if header["type"] != "session" || header["version"] != crate::session_log::VERSION {
        return Err(stop(
            BAD_INPUT,
            "replay: not a mb-mcp session log (version 1)",
        ));
    }
    if header["prompts"] != crate::prompts::fingerprint() {
        eprintln!(
            "replay: note: recorded with different prompts (only matters for G5c comparisons)"
        );
    }
    let mut browser = AgentBrowser::new(config_from(&header)?);
    if let Some(url) = header["start_url"].as_str() {
        let _ = browser.navigate(url);
        browser.forget_reported();
    }

    let steps: Vec<(usize, &str)> = lines.collect();
    let total = steps.len();
    let mut count = 0;
    for (index, line) in steps {
        let step: Value = serde_json::from_str(line)
            .map_err(|e| stop(BAD_INPUT, format!("replay: line {}: {e}", index + 1)))?;
        if let Err(mut stopped) =
            replay_step(&mut browser, &step, &options.secrets, options.ignore_text)
        {
            let why = match stopped.code {
                DIFFERS => "a result differs",
                NEEDS_PERSON => "needs the person",
                _ => "bad log",
            };
            stopped.message.push_str(&format!(
                "\nreplay: stopped at step {} of {total} ({count} matched): {why}",
                count + 1
            ));
            return Err(stopped);
        }
        count += 1;
    }
    Ok(count)
}

fn config_from(header: &Value) -> Result<AgentConfig, Stop> {
    let mut config = AgentConfig::default();
    if let Some([w, h]) = header["viewport"].as_array().map(Vec::as_slice) {
        config.viewport_width = w.as_u64().unwrap_or(1280) as usize;
        config.viewport_height = h.as_u64().unwrap_or(800) as usize;
    }
    config.approve = match header["approve"].as_str() {
        Some("auto") => ApproveMode::Auto,
        Some("ask") => ApproveMode::Ask,
        Some("deny") | None => ApproveMode::Deny,
        Some(other) => return Err(stop(BAD_INPUT, format!("replay: approve {other:?}?"))),
    };
    config.allow_origins = header["allow_origins"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|o| o.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if let Some(seed) = header["seed"].as_u64().and_then(|s| u32::try_from(s).ok()) {
        config.seed = seed;
    }
    // Deterministic whatever the recording used.
    config.clock = ClockMode::Virtual;
    config.system_fonts = false;
    Ok(config)
}

fn replay_step(
    browser: &mut AgentBrowser,
    step: &Value,
    secrets: &[(String, String)],
    ignore_text: bool,
) -> Result<(), Stop> {
    let n = step["n"].as_u64().unwrap_or(0);
    let tool = step["tool"].as_str().unwrap_or("");
    let mut request = request_from_json(tool, &step["args"])
        .map_err(|e| stop(BAD_INPUT, format!("replay: step {n}: {e}")))?;
    let target = step.get("target").and_then(Target::from_json);
    let label = match &target {
        Some(target) => format!("{n} {tool} {}", target.describe()),
        None => format!("{n} {tool}"),
    };

    if step["person_moved"] == true {
        return Err(stop(
            NEEDS_PERSON,
            format!(
                "✗ {label}: before this step the person changed the page in the window — that isn't in the log, so it can't be replayed"
            ),
        ));
    }

    // Ids are only accepted once an observation listed them, and the
    // recorded id may belong to a different element now.
    let snapshot = browser.observe(&ObserveOptions {
        scope: Scope::Full,
        max_elements: usize::MAX,
        max_text_chars: 0,
        cursor: 0,
    });
    if let Some(target) = &target {
        let id = find_target(&snapshot, target).ok_or_else(|| {
            stop(
                DIFFERS,
                format!(
                    "✗ {label}: target {} not found on the page",
                    target.describe()
                ),
            )
        })?;
        set_id(&mut request, id);
    }

    // A password: the value isn't in the log.
    let secret = |target: &Option<Target>| -> Result<String, Stop> {
        let name = target.as_ref().map(|t| t.name.as_str()).unwrap_or("");
        secrets
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.clone())
            .ok_or_else(|| {
                stop(
                    NEEDS_PERSON,
                    format!("✗ {label}: needs the password for field {name:?}: pass --secret \"{name}=…\""),
                )
            })
    };
    let resolved_by = step["resolved_by"].as_str();
    let recorded_error = step["result"]["error"].as_str();
    // A redacted password is only needed if it was actually typed: by the
    // agent (the step succeeded) — the person's case is handled below.
    // A step that ended in `handoff_required` runs as is and must end
    // there again.
    if let Request::Type { text, .. } = &mut request
        && text == REDACTED
        && recorded_error.is_none()
        && resolved_by.is_none()
    {
        *text = secret(&target)?;
    }

    // `stale_element` says what the agent had *observed*, which replay
    // doesn't reproduce (it observes the whole page every step): compare
    // the page without re-running the step.
    if recorded_error == Some("stale_element") {
        let reply = Reply::Error(worker::ErrorReport {
            code: "stale_element".into(),
            message: String::new(),
            hint: String::new(),
            repeat_count: 1,
            observation: None,
        });
        return compare(
            browser,
            step,
            &reply,
            &format!("{label} (not re-run)"),
            ignore_text,
        );
    }

    let reply = match worker::execute(browser, &request) {
        Ok(reply) => reply,
        Err(ToolError::Agent(AgentError::ApprovalPending { action })) => match resolved_by {
            Some("person_approved") => outcome(browser.resolve_approval(true)),
            Some("person_denied") => outcome(browser.resolve_approval(false)),
            Some("person_took_over") => match browser.resolve_approval(false) {
                Err(AgentError::ApprovalDenied { observation, .. }) => {
                    worker::error_reply(&ToolError::TakenOver(observation.to_blocks()), 1)
                }
                other => outcome(other),
            },
            _ => {
                return Err(stop(
                    DIFFERS,
                    format!(
                        "✗ {label}: now needs approval ({action}), which the recording didn't ask for"
                    ),
                ));
            }
        },
        Err(ToolError::Agent(AgentError::HandoffRequired { id, .. }))
            if resolved_by == Some("person_filled") =>
        {
            let value = secret(&target)?;
            browser.begin_handoff(id);
            browser
                .fill_as_person(id, &value)
                .map_err(|e| stop(DIFFERS, format!("✗ {label}: {}", e.message())))?;
            worker::observation_reply(browser.finish_handoff())
        }
        Err(ToolError::Agent(AgentError::HandoffRequired { .. }))
            if resolved_by == Some("person_declined") =>
        {
            worker::error_reply(&ToolError::HandoffDeclined, 1)
        }
        Err(error) => worker::error_reply(&error, 1),
    };

    compare(browser, step, &reply, &label, ignore_text)
}

fn compare(
    browser: &mut AgentBrowser,
    step: &Value,
    reply: &Reply,
    label: &str,
    ignore_text: bool,
) -> Result<(), Stop> {
    let got = result_record(browser, reply);
    let mut recorded = step["result"].clone();
    let mut got = got;
    // Logs from before text was recorded have none to compare.
    if ignore_text || recorded.get("text").is_none() {
        recorded.as_object_mut().map(|o| o.remove("text"));
        got.as_object_mut().map(|o| o.remove("text"));
    }
    // An error that was recorded is the expected result: say so.
    let expected_error = match recorded["error"].as_str() {
        Some(code) => format!(" ({code}, as recorded)"),
        None => String::new(),
    };
    match difference(&recorded, &got) {
        None => {
            eprintln!("✓ {label}{expected_error}");
            Ok(())
        }
        Some(diff) => Err(stop(
            DIFFERS,
            format!("✗ {label}: result differs from the recording (regression point)\n{diff}"),
        )),
    }
}

fn outcome(result: Result<mb_agent::Observation, AgentError>) -> Reply {
    match result {
        Ok(observation) => worker::observation_reply(observation),
        Err(error) => worker::error_reply(&ToolError::Agent(error), 1),
    }
}

fn set_id(request: &mut Request, id: NodeId) {
    let new = format!("e{}", id.raw());
    match request {
        Request::Click { id } | Request::Type { id, .. } => *id = new,
        Request::WaitFor { id: Some(id), .. } => *id = new,
        _ => {}
    }
}

/// `None` when the recorded and replayed results agree; otherwise a
/// readable difference.
pub fn difference(recorded: &Value, got: &Value) -> Option<String> {
    let mut out = String::new();
    for field in ["ok", "error", "url", "title"] {
        if recorded.get(field) != got.get(field) {
            out.push_str(&format!(
                "  {field}: recorded {} · now {}\n",
                recorded.get(field).unwrap_or(&Value::Null),
                got.get(field).unwrap_or(&Value::Null)
            ));
        }
    }
    let list = |v: &Value, key: &str| -> Vec<String> {
        v[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| e.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    for (key, prefix) in [("elements", ""), ("text", "text ")] {
        let (before, after) = (list(recorded, key), list(got, key));
        if before != after {
            let mut remaining = after.clone();
            let mut lines = Vec::new();
            for line in &before {
                match remaining.iter().position(|l| l == line) {
                    Some(i) => {
                        remaining.remove(i);
                    }
                    None => lines.push(format!("  - {prefix}{line}")),
                }
            }
            lines.extend(remaining.iter().map(|line| format!("  + {prefix}{line}")));
            if lines.is_empty() {
                lines.push(format!("  (same {key}, different order)"));
            }
            let extra = lines.len().saturating_sub(DIFF_LINES);
            for line in lines.into_iter().take(DIFF_LINES) {
                out.push_str(&line);
                out.push('\n');
            }
            if extra > 0 {
                out.push_str(&format!("  … {extra} more\n"));
            }
        }
    }
    (!out.is_empty()).then(|| out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::cli::Options;
    use crate::worker::Worker;

    // A fixture server whose home page can be swapped between recording
    // and replay (to simulate a regression).
    fn server(home: Arc<Mutex<String>>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                let _ = reader.read_line(&mut request_line);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                        break;
                    }
                }
                let body = if request_line.contains(" /next ") {
                    "<html><head><title>Next</title></head><body><p>Second</p></body></html>"
                        .to_string()
                } else {
                    home.lock().unwrap().clone()
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        base
    }

    fn home(button: &str) -> String {
        home_with(button, "2 items")
    }

    fn home_with(button: &str, note: &str) -> String {
        format!(
            "<html><head><title>Home</title></head><body><p>{note}</p><label for=\"n\">Name</label><input id=\"n\"><label for=\"p\">Password</label><input id=\"p\" type=\"password\"><button onclick=\"var c = document.createElement('input'); c.setAttribute('type', 'checkbox'); document.body.appendChild(c);\">{button}</button><a href=\"/next\">Next</a></body></html>"
        )
    }

    fn log_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("mb-replay-{}-{name}.jsonl", std::process::id()))
    }

    fn headless_with_log(path: &std::path::Path) -> Worker {
        let mut agent = AgentConfig {
            viewport_width: 800,
            viewport_height: 600,
            ..AgentConfig::default()
        };
        agent.system_fonts = false;
        Worker::new(Options {
            agent,
            start_url: None,
            window: false,
            preview: None,
            preview_clicks: Vec::new(),
            preview_scale: 1.0,
            http_port: None,
            attach: None,
            log: Some(path.to_path_buf()),
        })
    }

    fn id_in(reply: &Reply, needle: &str) -> String {
        let Reply::Observation { page, .. } = reply else {
            panic!("expected an observation: {reply:?}")
        };
        let line = page.lines().find(|l| l.contains(needle)).unwrap();
        line[1..line.find(']').unwrap()].to_string()
    }

    // navigate → type → click (adds an element) → click link
    fn record(base: &str, path: &std::path::Path) {
        let mut worker = headless_with_log(path);
        let page = worker.process(&Request::Navigate {
            url: format!("{base}/"),
        });
        let (name, password, add, next) = (
            id_in(&page, "name=\"Name\""),
            id_in(&page, "name=\"Password\""),
            id_in(&page, "button"),
            id_in(&page, "name=\"Next\""),
        );
        let steps = [
            (
                Request::Type {
                    id: name,
                    text: "hello".into(),
                    clear: false,
                    submit: false,
                },
                None,
            ),
            // Headless: the password is handed to a person (who isn't there).
            (
                Request::Type {
                    id: password,
                    text: "secret-probe".into(),
                    clear: false,
                    submit: false,
                },
                Some("handoff_required"),
            ),
            (Request::Click { id: add }, None),
            // A made-up id: the agent's mistake is part of the run.
            (Request::Click { id: "e999".into() }, Some("stale_element")),
            (Request::Click { id: next }, None),
        ];
        for (request, error) in steps {
            match (worker.process(&request), error) {
                (Reply::Observation { .. }, None) => {}
                (Reply::Error(report), Some(code)) => assert_eq!(report.code, code),
                (reply, _) => panic!("{request:?}: unexpected {reply:?}"),
            }
        }
    }

    fn options(path: &std::path::Path) -> ReplayOptions {
        ReplayOptions {
            file: path.to_path_buf(),
            secrets: Vec::new(),
            ignore_text: false,
        }
    }

    #[test]
    fn a_recorded_session_replays_and_matches() {
        let page = Arc::new(Mutex::new(home("Add")));
        let base = server(page);
        let path = log_path("match");
        record(&base, &path);
        let log = std::fs::read_to_string(&path).unwrap();
        assert_eq!(log.lines().count(), 7, "header + 6 steps:\n{log}");
        assert!(!log.contains("secret-probe"), "{log}");
        assert!(
            log.contains(r#""target":{"name":"Add","nth":0,"role":"button"}"#),
            "{log}"
        );
        assert!(log.contains(r#""text":"hello""#));
        // No --secret needed: the password was never typed.
        assert_eq!(replay(&options(&path)).unwrap(), 6);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_changed_page_is_reported_as_the_regression_point() {
        let page = Arc::new(Mutex::new(home("Add")));
        let base = server(page.clone());
        let path = log_path("regress");
        record(&base, &path);
        *page.lock().unwrap() = home("Add item");
        let stop = replay(&options(&path)).unwrap_err();
        assert_eq!(stop.code, DIFFERS);
        // Step 1 (navigate) already differs: the button's name changed.
        assert!(stop.message.starts_with("✗ 1 navigate"), "{}", stop.message);
        assert!(
            stop.message.contains("- button \"Add\""),
            "{}",
            stop.message
        );
        assert!(
            stop.message.contains("+ button \"Add item\""),
            "{}",
            stop.message
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_text_only_change_is_caught_unless_ignored() {
        let page = Arc::new(Mutex::new(home("Add")));
        let base = server(page.clone());
        let path = log_path("text");
        record(&base, &path);
        *page.lock().unwrap() = home_with("Add", "3 items");
        let stop = replay(&options(&path)).unwrap_err();
        assert!(stop.message.contains("- text 2 items"), "{}", stop.message);
        assert!(stop.message.contains("+ text 3 items"), "{}", stop.message);
        assert!(
            stop.message
                .contains("stopped at step 1 of 6 (0 matched): a result differs"),
            "{}",
            stop.message
        );
        let mut ignoring = options(&path);
        ignoring.ignore_text = true;
        assert_eq!(replay(&ignoring).unwrap(), 6);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_same_run_logs_the_same_bytes() {
        let base = server(Arc::new(Mutex::new(home("Add"))));
        let (a, b) = (log_path("det-a"), log_path("det-b"));
        record(&base, &a);
        record(&base, &b);
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            std::fs::read_to_string(&b).unwrap()
        );
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }

    #[test]
    fn differences_list_lost_and_new_elements() {
        let recorded = serde_json::json!({"ok": true, "url": "u", "title": "t",
            "elements": ["button \"A\"", "link \"B\""]});
        let got = serde_json::json!({"ok": false, "error": "stale_element", "url": "u", "title": "t",
            "elements": ["link \"B\"", "checkbox \"C\" [ ]"]});
        let diff = difference(&recorded, &got).unwrap();
        assert!(diff.contains("ok: recorded true · now false"), "{diff}");
        assert!(diff.contains("- button \"A\""));
        assert!(diff.contains("+ checkbox \"C\" [ ]"));
        assert!(difference(&recorded, &recorded).is_none());
    }
}
