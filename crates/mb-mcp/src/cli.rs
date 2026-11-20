// `mb-mcp` command line (docs/concept.md "`mb-mcp` CLI"). Hand-rolled: the
// flag set is small and a parser dependency would be the biggest thing in
// the binary.

use std::time::Duration;

use mb_agent::{AgentConfig, ApproveMode, ClockMode};

pub const USAGE: &str = "\
mb-mcp — mini-browser as an MCP server (stdio, or HTTP with --http)

USAGE:
    mb-mcp [OPTIONS]
    mb-mcp replay FILE [--secret NAME=VALUE]...
    mb-mcp eval SCENARIO LOG_OR_DIR... [--json PATH] [--min-success RATE]

OPTIONS:
    --viewport WxH          Page viewport in CSS px            [default: 1280x800]
    --start-url URL         Page to open before the first tool call
    --allow-origin ORIGIN   Only allow navigation to this origin (repeatable),
                            e.g. http://localhost:3000
    --approve MODE          auto | deny | ask — risky actions (POST, cross-
                            origin). ask (needs --window) holds the action
                            and asks the person watching   [default: deny]
    --clock MODE            virtual | system                   [default: virtual]
    --seed N                Math.random seed                   [default: 1]
    --settle-timeout-ms N   Wall-clock cap per settle          [default: 5000]
    --window                Open an observation window: the page with the
                            agent's element ids, its last target, and an
                            action timeline (clock defaults to system)
    --http                  Serve MCP over HTTP at http://127.0.0.1:PORT/mcp
                            instead of stdio: start the browser yourself
                            (usually with --window) and let an agent attach.
                            Needs `Authorization: Bearer TOKEN`; the token is
                            random per launch unless MB_MCP_TOKEN is set
    --port N                Port for --http (0 = any free)     [default: 7333]
    --log PATH              Write a session log (JSONL): one line per tool
                            call, for `mb-mcp replay`. Typed text is kept
                            except in password fields
    --preview PATH          Render one observation-window frame (after
                            --start-url) to a PNG and exit — no window, no MCP
    --preview-click ID      Click this element before --preview (repeatable)
    --preview-scale N       Device pixel ratio for --preview, e.g. 2 for Retina
    -h, --help              Print this help (to stderr)

REPLAY (a log runs like a script — replay only logs you trust):
    mb-mcp replay FILE      Re-run a session log without an LLM (headless,
                            virtual clock) and report the first step whose
                            result differs. Exit 0 = same, 1 = differs,
                            2 = bad log/arguments, 3 = needs the person
    --secret NAME[=VALUE]   Password for the field with accessible name NAME
                            (from MB_SECRET_<NAME> if no VALUE: keeps it out
                            of ps / shell history). Typed only into password
                            fields on local dev hosts (localhost, 127.0.0.1,
                            *.localhost, *.test)
    --ignore-text           Compare elements only, not the page text (for
                            pages that show clocks or random values)
    --window                Step through it in a window: Next / Back /
                            Run all / Restart, the next target outlined,
                            recorded vs now for each step
    --preview PATH          Render that window after --preview-steps N
                            steps to a PNG (no window)

EVAL (judge and aggregate session logs; it runs no agent):
    mb-mcp eval SCENARIO LOG_OR_DIR...
                            Judge each log against SCENARIO (JSON: task,
                            success and forbid checks), then report success
                            rate, steps, error codes, where failed runs
                            ended and safety violations, per prompts
                            version. Directories expand to their *.jsonl
    --json PATH             Also write the report as JSON
    --min-success RATE      Exit 1 if the success rate is below RATE (0..1)

With stdio, stdout carries the MCP protocol only; logs always go to stderr.
";

pub const DEFAULT_PORT: u16 = 7333;

#[derive(Debug, Clone)]
pub struct Options {
    pub agent: AgentConfig,
    pub start_url: Option<String>,
    /// Open the observation window (G3).
    pub window: bool,
    /// `--preview PATH`: render one observation-window frame and exit.
    pub preview: Option<std::path::PathBuf>,
    /// `--preview-click ID` (repeatable): clicks to perform before it.
    pub preview_clicks: Vec<String>,
    /// `--preview-scale N`: device pixel ratio for the preview (default 1).
    pub preview_scale: f32,
    /// `--http`: serve MCP over HTTP on this loopback port (G3b).
    pub http_port: Option<u16>,
    /// Filled in by `main` once the HTTP listener is bound.
    pub attach: Option<crate::http::Attach>,
    /// `--log PATH`: session log for `mb-mcp replay` (G5a).
    pub log: Option<std::path::PathBuf>,
}

/// `mb-mcp replay FILE ...`
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayOptions {
    pub file: std::path::PathBuf,
    /// (accessible name, value) for fields the person filled in.
    pub secrets: Vec<(String, String)>,
    /// Don't compare page text (pages that show clocks, random ids…).
    pub ignore_text: bool,
    /// `--window`: step through the replay in a window (G5b).
    pub window: bool,
    /// `--preview PATH`: render the replay window after `preview_steps`
    /// steps to a PNG instead of opening it.
    pub preview: Option<std::path::PathBuf>,
    pub preview_steps: usize,
    pub preview_scale: f32,
}

pub enum Parsed {
    Run(Options),
    Replay(ReplayOptions),
    Eval(EvalOptions),
    Help,
}

/// `mb-mcp eval SCENARIO LOG_OR_DIR...`
#[derive(Debug, Clone, PartialEq)]
pub struct EvalOptions {
    pub scenario: std::path::PathBuf,
    pub logs: Vec<std::path::PathBuf>,
    pub json: Option<std::path::PathBuf>,
    pub min_success: Option<f64>,
}

fn parse_eval(mut args: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut paths = Vec::new();
    let mut json = None;
    let mut min_success = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--json" => {
                json = Some(std::path::PathBuf::from(
                    args.next().ok_or("--json needs a PATH")?,
                ));
            }
            "--min-success" => {
                let raw = args.next().ok_or("--min-success needs RATE")?;
                min_success = Some(
                    raw.parse::<f64>()
                        .ok()
                        .filter(|r| (0.0..=1.0).contains(r))
                        .ok_or_else(|| format!("--min-success expects 0..1, got {raw:?}"))?,
                );
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown eval option {other:?} (see --help)"));
            }
            path => paths.push(std::path::PathBuf::from(path)),
        }
    }
    if paths.len() < 2 {
        return Err("eval needs a SCENARIO and at least one session log or directory".into());
    }
    let scenario = paths.remove(0);
    Ok(Parsed::Eval(EvalOptions {
        scenario,
        logs: paths,
        json,
        min_success,
    }))
}

/// `--secret Password` reads `MB_SECRET_PASSWORD`.
pub fn secret_env_var(name: &str) -> String {
    let upper: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("MB_SECRET_{upper}")
}

fn parse_replay(mut args: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut file = None;
    let mut secrets = Vec::new();
    let mut ignore_text = false;
    let mut window = false;
    let mut preview = None;
    let mut preview_steps = 0;
    let mut preview_scale = 1.0_f32;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--ignore-text" => ignore_text = true,
            "--window" => window = true,
            "--preview" => {
                preview = Some(std::path::PathBuf::from(
                    args.next().ok_or("--preview needs a PATH")?,
                ));
            }
            "--preview-steps" => {
                let raw = args.next().ok_or("--preview-steps needs N")?;
                preview_steps = raw
                    .parse()
                    .map_err(|_| format!("--preview-steps expects a number, got {raw:?}"))?;
            }
            "--preview-scale" => {
                let raw = args.next().ok_or("--preview-scale needs N")?;
                preview_scale = raw
                    .parse::<f32>()
                    .ok()
                    .filter(|s| (0.5..=4.0).contains(s))
                    .ok_or_else(|| format!("--preview-scale expects 0.5..=4, got {raw:?}"))?;
            }
            "--secret" => {
                let raw = args.next().ok_or("--secret needs NAME or NAME=VALUE")?;
                let (name, value) = match raw.split_once('=') {
                    Some((name, value)) => (name.to_string(), value.to_string()),
                    // Keeps the value out of `ps` and shell history.
                    None => {
                        let var = secret_env_var(&raw);
                        let value = std::env::var(&var)
                            .map_err(|_| format!("--secret {raw}: set {var}"))?;
                        (raw, value)
                    }
                };
                secrets.push((name, value));
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown replay option {other:?} (see --help)"));
            }
            path if file.is_none() => file = Some(std::path::PathBuf::from(path)),
            extra => return Err(format!("unexpected argument {extra:?}")),
        }
    }
    let file = file.ok_or("replay needs a session log FILE")?;
    Ok(Parsed::Replay(ReplayOptions {
        file,
        secrets,
        ignore_text,
        window,
        preview,
        preview_steps,
        preview_scale,
    }))
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Parsed, String> {
    let mut agent = AgentConfig::default();
    let mut start_url = None;
    let mut window = false;
    let mut clock_given = false;
    let mut preview = None;
    let mut preview_clicks = Vec::new();
    let mut preview_scale = 1.0_f32;
    let mut http = false;
    let mut port: Option<u16> = None;
    let mut args = args.into_iter().peekable();
    if args.peek().is_some_and(|a| a == "replay") {
        args.next();
        return parse_replay(args);
    }
    if args.peek().is_some_and(|a| a == "eval") {
        args.next();
        return parse_eval(args);
    }
    let mut log = None;
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--viewport" => {
                let raw = value("--viewport")?;
                let (w, h) = raw
                    .split_once(['x', 'X'])
                    .ok_or_else(|| format!("--viewport expects WxH, got {raw:?}"))?;
                agent.viewport_width = parse_dimension(w, &raw)?;
                agent.viewport_height = parse_dimension(h, &raw)?;
            }
            "--start-url" => start_url = Some(value("--start-url")?),
            "--allow-origin" => {
                let origin = value("--allow-origin")?;
                agent.allow_origins.push(normalize_origin(&origin)?);
            }
            "--approve" => {
                agent.approve = match value("--approve")?.as_str() {
                    "auto" => ApproveMode::Auto,
                    "deny" => ApproveMode::Deny,
                    "ask" => ApproveMode::Ask,
                    other => {
                        return Err(format!("--approve expects auto|deny|ask, got {other:?}"));
                    }
                }
            }
            "--clock" => {
                clock_given = true;
                agent.clock = match value("--clock")?.as_str() {
                    "virtual" => ClockMode::Virtual,
                    "system" => ClockMode::System,
                    other => return Err(format!("--clock expects virtual|system, got {other:?}")),
                }
            }
            "--seed" => {
                let raw = value("--seed")?;
                agent.seed = raw
                    .parse()
                    .map_err(|_| format!("--seed expects a number, got {raw:?}"))?;
            }
            "--settle-timeout-ms" => {
                let raw = value("--settle-timeout-ms")?;
                let ms: u64 = raw
                    .parse()
                    .map_err(|_| format!("--settle-timeout-ms expects a number, got {raw:?}"))?;
                agent.settle_timeout = Duration::from_millis(ms);
            }
            "--window" => window = true,
            "--log" => log = Some(std::path::PathBuf::from(value("--log")?)),
            "--http" => http = true,
            "--port" => {
                let raw = value("--port")?;
                port = Some(
                    raw.parse()
                        .map_err(|_| format!("--port expects 0..=65535, got {raw:?}"))?,
                );
            }
            "--preview" => preview = Some(std::path::PathBuf::from(value("--preview")?)),
            "--preview-click" => preview_clicks.push(value("--preview-click")?),
            "--preview-scale" => {
                let raw = value("--preview-scale")?;
                preview_scale = raw
                    .parse::<f32>()
                    .ok()
                    .filter(|s| (0.5..=4.0).contains(s))
                    .ok_or_else(|| format!("--preview-scale expects 0.5..=4, got {raw:?}"))?;
            }
            other => return Err(format!("unknown option {other:?} (see --help)")),
        }
    }
    if port.is_some() && !http {
        return Err("--port only applies with --http".into());
    }
    // Someone has to be there to answer.
    if agent.approve == ApproveMode::Ask && !window && preview.is_none() {
        return Err(
            "--approve ask needs --window: the approval card lives in the observation window"
                .into(),
        );
    }
    // A person is watching: timers should run at the speed they see.
    if window && !clock_given {
        agent.clock = ClockMode::System;
    }
    Ok(Parsed::Run(Options {
        agent,
        start_url,
        window,
        preview,
        preview_clicks,
        preview_scale,
        http_port: http.then(|| port.unwrap_or(DEFAULT_PORT)),
        attach: None,
        log,
    }))
}

fn parse_dimension(raw: &str, whole: &str) -> Result<usize, String> {
    raw.trim()
        .parse::<usize>()
        .ok()
        .filter(|&n| (100..=8000).contains(&n))
        .ok_or_else(|| format!("--viewport {whole:?}: dimensions must be 100..=8000"))
}

/// `http://localhost:3000/` → `http://localhost:3000`, matching how the
/// policy prints origins (default ports dropped).
fn normalize_origin(raw: &str) -> Result<String, String> {
    let url = mb_runtime::net::Url::parse(raw)
        .map_err(|_| format!("--allow-origin {raw:?} is not an http(s) URL"))?;
    Ok(mb_runtime::state::policy::origin_of(&url))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Options {
        match parse(args.iter().map(|s| s.to_string())).unwrap() {
            Parsed::Run(options) => options,
            Parsed::Help | Parsed::Replay(_) | Parsed::Eval(_) => panic!("expected a run"),
        }
    }

    #[test]
    fn defaults_are_safe_and_deterministic() {
        let options = run(&[]);
        assert_eq!(options.agent.approve, ApproveMode::Deny);
        assert_eq!(options.agent.clock, ClockMode::Virtual);
        assert!(options.agent.allow_origins.is_empty());
        assert!(options.start_url.is_none());
    }

    #[test]
    fn parses_every_flag() {
        let options = run(&[
            "--viewport",
            "1024x768",
            "--start-url",
            "http://localhost:3000/",
            "--allow-origin",
            "http://localhost:3000/",
            "--allow-origin",
            "https://example.test:443",
            "--approve",
            "auto",
            "--clock",
            "system",
            "--seed",
            "42",
        ]);
        assert_eq!(
            (options.agent.viewport_width, options.agent.viewport_height),
            (1024, 768)
        );
        assert_eq!(
            options.agent.allow_origins,
            vec!["http://localhost:3000", "https://example.test"]
        );
        assert_eq!(options.agent.approve, ApproveMode::Auto);
        assert_eq!(options.agent.clock, ClockMode::System);
        assert_eq!(options.agent.seed, 42);
    }

    #[test]
    fn window_mode_defaults_to_the_system_clock_unless_told_otherwise() {
        let watched = run(&["--window"]);
        assert!(watched.window);
        assert_eq!(watched.agent.clock, ClockMode::System);
        let pinned = run(&["--window", "--clock", "virtual"]);
        assert_eq!(pinned.agent.clock, ClockMode::Virtual);
    }

    #[test]
    fn ask_mode_requires_the_window() {
        assert!(parse(["--approve", "ask"].map(String::from)).is_err());
        let options = run(&["--approve", "ask", "--window"]);
        assert_eq!(options.agent.approve, ApproveMode::Ask);
    }

    #[test]
    fn http_is_opt_in_with_a_default_port() {
        assert_eq!(run(&[]).http_port, None);
        assert_eq!(run(&["--http"]).http_port, Some(DEFAULT_PORT));
        assert_eq!(run(&["--http", "--port", "0"]).http_port, Some(0));
        let both = run(&["--http", "--window", "--approve", "ask"]);
        assert!(both.window && both.http_port.is_some());
        assert!(parse(["--port", "9000"].map(String::from)).is_err());
        assert!(parse(["--http", "--port", "99999"].map(String::from)).is_err());
    }

    #[test]
    fn replay_is_a_subcommand_with_secrets() {
        let Ok(Parsed::Replay(replay)) =
            parse(["replay", "run.jsonl", "--secret", "Password=a=b"].map(String::from))
        else {
            panic!("expected replay")
        };
        assert_eq!(replay.file, std::path::PathBuf::from("run.jsonl"));
        assert_eq!(replay.secrets, vec![("Password".into(), "a=b".into())]);
        assert!(!replay.ignore_text);
        assert_eq!(secret_env_var("New password"), "MB_SECRET_NEW_PASSWORD");
        assert!(parse(["replay"].map(String::from)).is_err());
        assert!(parse(["replay", "a", "b"].map(String::from)).is_err());
        assert_eq!(
            run(&["--log", "out.jsonl"]).log,
            Some(std::path::PathBuf::from("out.jsonl"))
        );
    }

    #[test]
    fn eval_takes_a_scenario_and_logs() {
        let Ok(Parsed::Eval(eval)) = parse(
            [
                "eval",
                "s.json",
                "runs/",
                "one.jsonl",
                "--min-success",
                "0.8",
            ]
            .map(String::from),
        ) else {
            panic!("expected eval")
        };
        assert_eq!(eval.scenario, std::path::PathBuf::from("s.json"));
        assert_eq!(eval.logs.len(), 2);
        assert_eq!(eval.min_success, Some(0.8));
        assert!(parse(["eval", "s.json"].map(String::from)).is_err());
        assert!(parse(["eval", "s.json", "r", "--min-success", "80"].map(String::from)).is_err());
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            vec!["--viewport", "big"],
            vec!["--approve", "maybe"],
            vec!["--approve", "ask"],
            vec!["--approve", "maybe", "--window"],
            vec!["--clock", "fast"],
            vec!["--allow-origin", "ftp://x"],
            vec!["--seed"],
            vec!["--bogus"],
        ] {
            assert!(parse(bad.iter().map(|s| s.to_string())).is_err(), "{bad:?}");
        }
    }
}
