// The browser thread (docs/concept.md "프로세스·스레드 배치"). `AgentBrowser`
// holds `Rc`s (DOM, JS runtime) and can't leave the thread it was built on,
// so the MCP server (tokio, other thread) sends it `Job`s over a channel and
// awaits each reply on a oneshot. Jobs run strictly one at a time.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use mb_agent::{
    AgentBrowser, AgentError, Observation, ObserveOptions, Scope, Screenshot, WaitCondition,
};
use mb_runtime::dom::NodeId;
use tokio::sync::oneshot;

use crate::cli::Options;

// Debug is hand-written below: `Type.text` may be a (test) password, and
// requests end up in logs and in the repeat-tracker key.
#[derive(Clone, PartialEq)]
pub enum Request {
    Navigate {
        url: String,
    },
    Observe {
        full: bool,
        cursor: usize,
    },
    Click {
        id: String,
    },
    Type {
        id: String,
        text: String,
        clear: bool,
        submit: bool,
    },
    Press {
        key: String,
    },
    Scroll {
        dy: f64,
    },
    Back,
    Forward,
    Screenshot,
    WaitFor {
        text: Option<String>,
        id: Option<String>,
        timeout_ms: u64,
    },
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Request::Navigate { url } => f.debug_struct("Navigate").field("url", url).finish(),
            Request::Observe { full, cursor } => f
                .debug_struct("Observe")
                .field("full", full)
                .field("cursor", cursor)
                .finish(),
            Request::Click { id } => f.debug_struct("Click").field("id", id).finish(),
            Request::Type {
                id,
                text,
                clear,
                submit,
            } => f
                .debug_struct("Type")
                .field("id", id)
                .field(
                    "text",
                    &format_args!("<redacted, {} chars>", text.chars().count()),
                )
                .field("clear", clear)
                .field("submit", submit)
                .finish(),
            Request::Press { key } => f.debug_struct("Press").field("key", key).finish(),
            Request::Scroll { dy } => f.debug_struct("Scroll").field("dy", dy).finish(),
            Request::Back => f.write_str("Back"),
            Request::Forward => f.write_str("Forward"),
            Request::Screenshot => f.write_str("Screenshot"),
            Request::WaitFor {
                text,
                id,
                timeout_ms,
            } => f
                .debug_struct("WaitFor")
                .field("text", text)
                .field("id", id)
                .field("timeout_ms", timeout_ms)
                .finish(),
        }
    }
}

#[derive(Debug)]
pub enum Reply {
    /// (browser block, page block) — sent as two content items.
    Observation {
        browser: String,
        page: String,
    },
    Screenshot {
        png: Vec<u8>,
        width: usize,
        height: usize,
        /// Origin of the page shown, for the untrusted label.
        origin: Option<String>,
    },
    Error(ErrorReport),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ErrorReport {
    pub code: String,
    pub message: String,
    pub hint: String,
    pub repeat_count: u32,
    /// The page after the failure, when the error carries one.
    pub observation: Option<(String, String)>,
}

pub struct Job {
    pub request: Request,
    pub reply: oneshot::Sender<Reply>,
}

/// From the third identical failure in a row the hint stops helping and
/// starts telling the model to stop (docs/concept.md "에러 응답 형식").
const REPEAT_LIMIT: u32 = 3;
const STOP_REPEATING: &str =
    "Stop repeating this action. Try a different approach or ask the user.";

#[derive(Default)]
struct RepeatTracker {
    last: Option<(String, String)>,
    count: u32,
}

impl RepeatTracker {
    fn failure(&mut self, request: &Request, code: &str) -> u32 {
        let key = (format!("{request:?}"), code.to_string());
        if self.last.as_ref() == Some(&key) {
            self.count += 1;
        } else {
            self.last = Some(key);
            self.count = 1;
        }
        self.count
    }

    fn success(&mut self) {
        self.last = None;
        self.count = 0;
    }
}

/// Serve jobs until every sender is gone (the MCP server shut down).
pub fn run(options: Options, jobs: Receiver<Job>) {
    let mut browser = new_browser(&options);
    let mut repeats = RepeatTracker::default();
    while let Ok(job) = jobs.recv() {
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            handle(&mut browser, &mut repeats, &job.request)
        }));
        let reply = match outcome {
            Ok(reply) => reply,
            Err(_) => {
                // The state may be broken mid-update: throw it away and start
                // over on the page we were on (docs/concept.md "엔진 패닉").
                let last_url = browser.state().current_url.as_ref().map(|u| u.to_string());
                eprintln!(
                    "[mb-mcp] engine panicked handling {:?}; rebuilding",
                    job.request
                );
                browser = new_browser(&options);
                if let Some(url) = last_url {
                    let _ = browser.navigate(&url);
                }
                Reply::Error(ErrorReport {
                    code: "engine_crashed".into(),
                    message: "The browser engine crashed and was restarted on the same URL.".into(),
                    hint: "Call `observe` to see the reloaded page; form input was lost.".into(),
                    repeat_count: repeats.failure(&job.request, "engine_crashed"),
                    observation: None,
                })
            }
        };
        // The server side may have given up (client cancelled): fine.
        let _ = job.reply.send(reply);
    }
}

fn new_browser(options: &Options) -> AgentBrowser {
    let mut browser = AgentBrowser::new(options.agent.clone());
    if let Some(url) = &options.start_url
        && let Err(error) = browser.navigate(url)
    {
        eprintln!("[mb-mcp] --start-url {url}: {error}");
    }
    browser
}

fn handle(browser: &mut AgentBrowser, repeats: &mut RepeatTracker, request: &Request) -> Reply {
    let result = execute(browser, request);
    match result {
        Ok(reply) => {
            repeats.success();
            reply
        }
        Err(error) => {
            let repeat_count = repeats.failure(request, error.code());
            let hint = if repeat_count >= REPEAT_LIMIT {
                STOP_REPEATING.to_string()
            } else {
                error.hint().to_string()
            };
            Reply::Error(ErrorReport {
                code: error.code().to_string(),
                message: error.message(),
                hint,
                repeat_count,
                observation: error.observation().map(Observation::to_blocks),
            })
        }
    }
}

/// Parse a client error before it reaches the browser.
#[derive(Debug)]
enum ToolError {
    Agent(AgentError),
    InvalidId(String),
    InvalidArguments(&'static str),
}

impl ToolError {
    fn code(&self) -> &'static str {
        match self {
            ToolError::Agent(e) => e.code(),
            ToolError::InvalidId(_) => "invalid_id",
            ToolError::InvalidArguments(_) => "invalid_arguments",
        }
    }

    fn message(&self) -> String {
        match self {
            ToolError::Agent(e) => e.message(),
            ToolError::InvalidId(raw) => format!("{raw:?} is not an element id."),
            ToolError::InvalidArguments(why) => (*why).to_string(),
        }
    }

    fn hint(&self) -> &'static str {
        match self {
            ToolError::Agent(e) => e.hint(),
            ToolError::InvalidId(_) => {
                "Element ids look like \"e21\"; copy one from the latest observation."
            }
            ToolError::InvalidArguments(_) => "Check the tool's parameters.",
        }
    }

    fn observation(&self) -> Option<&Observation> {
        match self {
            ToolError::Agent(e) => e.observation(),
            _ => None,
        }
    }
}

impl From<AgentError> for ToolError {
    fn from(error: AgentError) -> Self {
        ToolError::Agent(error)
    }
}

fn parse_id(raw: &str) -> Result<NodeId, ToolError> {
    let digits = raw.trim().strip_prefix(['e', 'E']).unwrap_or(raw.trim());
    digits
        .parse::<u32>()
        .map(NodeId::from_raw)
        .map_err(|_| ToolError::InvalidId(raw.to_string()))
}

fn observation_reply(observation: Observation) -> Reply {
    let (browser, page) = observation.to_blocks();
    Reply::Observation { browser, page }
}

fn execute(browser: &mut AgentBrowser, request: &Request) -> Result<Reply, ToolError> {
    let observation = match request {
        Request::Navigate { url } => browser.navigate(url)?,
        Request::Observe { full, cursor } => browser.observe(&ObserveOptions {
            scope: if *full { Scope::Full } else { Scope::Viewport },
            cursor: *cursor,
            ..ObserveOptions::default()
        }),
        Request::Click { id } => browser.click(parse_id(id)?)?,
        Request::Type {
            id,
            text,
            clear,
            submit,
        } => browser.type_text(parse_id(id)?, text, *clear, *submit)?,
        Request::Press { key } => browser.press(key)?,
        Request::Scroll { dy } => browser.scroll(*dy as f32)?,
        Request::Back => browser.back()?,
        Request::Forward => browser.forward()?,
        Request::Screenshot => {
            let shot = browser.screenshot();
            return Ok(Reply::Screenshot {
                png: encode_png(&shot),
                width: shot.width,
                height: shot.height,
                origin: browser
                    .state()
                    .current_url
                    .as_ref()
                    .map(mb_runtime::state::policy::origin_of),
            });
        }
        Request::WaitFor {
            text,
            id,
            timeout_ms,
        } => {
            let condition = match (text, id) {
                (Some(text), None) if !text.is_empty() => WaitCondition::Text(text.clone()),
                (None, Some(id)) => WaitCondition::Element(parse_id(id)?),
                _ => {
                    return Err(ToolError::InvalidArguments(
                        "wait_for needs exactly one of `text` or `id`.",
                    ));
                }
            };
            browser.wait_for(condition, Duration::from_millis(*timeout_ms))?
        }
    };
    Ok(observation_reply(observation))
}

fn encode_png(shot: &Screenshot) -> Vec<u8> {
    use image::ImageEncoder;
    let mut rgb = Vec::with_capacity(shot.pixels.len() * 3);
    for pixel in &shot.pixels {
        rgb.push((pixel >> 16) as u8);
        rgb.push((pixel >> 8) as u8);
        rgb.push(*pixel as u8);
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            &rgb,
            shot.width as u32,
            shot.height as u32,
            image::ExtendedColorType::Rgb8,
        )
        .expect("encoding an in-memory RGB buffer as PNG can't fail");
    png
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ids_with_or_without_prefix() {
        assert_eq!(parse_id("e21").unwrap(), NodeId::from_raw(21));
        assert_eq!(parse_id(" 7 ").unwrap(), NodeId::from_raw(7));
        assert!(parse_id("button").is_err());
        assert!(parse_id("e-1").is_err());
    }

    #[test]
    fn typed_text_never_shows_up_in_debug_output() {
        let request = Request::Type {
            id: "e3".into(),
            text: "hunter22".into(),
            clear: false,
            submit: true,
        };
        let shown = format!("{request:?}");
        assert!(!shown.contains("hunter22"), "{shown}");
        assert!(shown.contains("<redacted, 8 chars>"), "{shown}");
    }

    #[test]
    fn third_identical_failure_switches_the_hint() {
        let mut repeats = RepeatTracker::default();
        let click = Request::Click { id: "e1".into() };
        assert_eq!(repeats.failure(&click, "stale_element"), 1);
        assert_eq!(repeats.failure(&click, "stale_element"), 2);
        assert_eq!(repeats.failure(&click, "stale_element"), 3);
        // A different error (or action) starts a new streak.
        assert_eq!(repeats.failure(&click, "not_interactable"), 1);
        repeats.success();
        assert_eq!(repeats.failure(&click, "not_interactable"), 1);
    }
}
