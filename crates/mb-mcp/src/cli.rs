// `mb-mcp` command line (docs/concept.md "`mb-mcp` CLI"). Hand-rolled: the
// flag set is small and a parser dependency would be the biggest thing in
// the binary.

use std::time::Duration;

use mb_agent::{AgentConfig, ApproveMode, ClockMode};

pub const USAGE: &str = "\
mb-mcp — mini-browser as an MCP server (stdio)

USAGE:
    mb-mcp [OPTIONS]

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
    --preview PATH          Render one observation-window frame (after
                            --start-url) to a PNG and exit — no window, no MCP
    --preview-click ID      Click this element before --preview (repeatable)
    --preview-scale N       Device pixel ratio for --preview, e.g. 2 for Retina
    -h, --help              Print this help (to stderr)

stdout carries the MCP protocol only; logs go to stderr.
";

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
}

pub enum Parsed {
    Run(Options),
    Help,
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Parsed, String> {
    let mut agent = AgentConfig::default();
    let mut start_url = None;
    let mut window = false;
    let mut clock_given = false;
    let mut preview = None;
    let mut preview_clicks = Vec::new();
    let mut preview_scale = 1.0_f32;
    let mut args = args.into_iter();
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
            Parsed::Help => panic!("unexpected help"),
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
