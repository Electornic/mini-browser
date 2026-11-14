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
    --approve MODE          auto | deny  — risky actions (POST, cross-origin)
                                                               [default: deny]
    --clock MODE            virtual | system                   [default: virtual]
    --seed N                Math.random seed                   [default: 1]
    --settle-timeout-ms N   Wall-clock cap per settle          [default: 5000]
    -h, --help              Print this help (to stderr)

stdout carries the MCP protocol only; logs go to stderr.
";

#[derive(Debug, Clone)]
pub struct Options {
    pub agent: AgentConfig,
    pub start_url: Option<String>,
}

pub enum Parsed {
    Run(Options),
    Help,
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Parsed, String> {
    let mut agent = AgentConfig::default();
    let mut start_url = None;
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
                    "ask" => {
                        return Err(
                            "--approve ask needs the observation window (G3/G4); use auto or deny"
                                .into(),
                        );
                    }
                    other => return Err(format!("--approve expects auto|deny, got {other:?}")),
                }
            }
            "--clock" => {
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
            "--window" => {
                return Err("--window (observation window) arrives in G3".into());
            }
            other => return Err(format!("unknown option {other:?} (see --help)")),
        }
    }
    Ok(Parsed::Run(Options { agent, start_url }))
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
    fn rejects_bad_input() {
        for bad in [
            vec!["--viewport", "big"],
            vec!["--approve", "maybe"],
            vec!["--approve", "ask"],
            vec!["--allow-origin", "ftp://x"],
            vec!["--seed"],
            vec!["--bogus"],
        ] {
            assert!(parse(bad.iter().map(|s| s.to_string())).is_err(), "{bad:?}");
        }
    }
}
