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

/// One line of the observation window's action timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineEntry {
    /// e.g. "click e24", "type e22 (8 chars)". Never the typed text.
    pub label: String,
    /// `None` = succeeded, `Some(code)` = failed with that error code.
    pub error: Option<String>,
}

const TIMELINE_LEN: usize = 50;

/// The browser plus everything that survives across requests: repeat
/// tracking, the timeline, and how to rebuild after a panic. Both the
/// headless loop (`run`) and the observation window drive it.
pub struct Worker {
    options: Options,
    browser: AgentBrowser,
    repeats: RepeatTracker,
    timeline: Vec<TimelineEntry>,
    parked: Option<Parked>,
}

impl Worker {
    pub fn new(options: Options) -> Self {
        let browser = new_browser(&options);
        Self {
            options,
            browser,
            repeats: RepeatTracker::default(),
            timeline: Vec::new(),
            parked: None,
        }
    }

    pub fn browser(&mut self) -> &mut AgentBrowser {
        &mut self.browser
    }

    pub fn options(&self) -> &Options {
        &self.options
    }

    pub fn timeline(&self) -> &[TimelineEntry] {
        &self.timeline
    }

    /// Headless: run one request and reply. Nothing ever waits for a
    /// person here (`--approve ask` requires the window).
    pub fn process(&mut self, request: &Request) -> Reply {
        let result = self.execute_guarded(request, |browser| execute(browser, request));
        self.conclude(request, result, None)
    }

    /// Window mode: like `process`, but an action that needs the person
    /// watching — approval in Ask mode, or a password handoff — is parked
    /// with its reply channel until `answer`. Only one can wait at a time.
    pub fn submit(&mut self, job: Job) {
        if let Some(parked) = &self.parked {
            let busy = ToolError::Agent(AgentError::ApprovalPending {
                action: parked.waiting.describe(),
            });
            let reply = self.conclude(&job.request, Err(busy), None);
            let _ = job.reply.send(reply);
            return;
        }
        let result = self.execute_guarded(&job.request, |browser| execute(browser, &job.request));
        let waiting = match &result {
            Err(ToolError::Agent(AgentError::ApprovalPending { action })) => {
                Some(Waiting::Approval {
                    action: action.clone(),
                })
            }
            Err(ToolError::Agent(AgentError::HandoffRequired { id, .. }))
                if matches!(job.request, Request::Type { .. }) =>
            {
                self.browser.highlight(*id);
                self.browser.begin_handoff();
                Some(Waiting::Handoff {
                    id: format!("e{}", id.raw()),
                })
            }
            _ => None,
        };
        match waiting {
            Some(waiting) => {
                self.parked = Some(Parked {
                    request: job.request,
                    reply: job.reply,
                    waiting,
                });
            }
            None => {
                let reply = self.conclude(&job.request, result, None);
                let _ = job.reply.send(reply);
            }
        }
    }

    /// What the person is being asked, if anything.
    pub fn waiting(&self) -> Option<&Waiting> {
        self.parked.as_ref().map(|p| &p.waiting)
    }

    /// The person's answer to the parked request; sends its reply.
    pub fn answer(&mut self, answer: Answer) {
        let Some(parked) = self.parked.take() else {
            return;
        };
        let (result, outcome) = match (&parked.waiting, answer) {
            (Waiting::Approval { .. }, Answer::Approve) => (
                self.execute_guarded(&parked.request, |b| {
                    b.resolve_approval(true)
                        .map(observation_reply)
                        .map_err(ToolError::from)
                }),
                "approved",
            ),
            (Waiting::Approval { .. }, Answer::Deny) => (
                self.execute_guarded(&parked.request, |b| {
                    b.resolve_approval(false)
                        .map(observation_reply)
                        .map_err(ToolError::from)
                }),
                "refused",
            ),
            (Waiting::Approval { .. }, Answer::TakeOver) => (
                self.execute_guarded(&parked.request, |b| match b.resolve_approval(false) {
                    Err(AgentError::ApprovalDenied { observation, .. }) => {
                        Err(ToolError::TakenOver(observation.to_blocks()))
                    }
                    other => other.map(observation_reply).map_err(ToolError::from),
                }),
                "person took over",
            ),
            (Waiting::Handoff { .. }, Answer::HandoffDone) => (
                self.execute_guarded(&parked.request, |b| {
                    Ok(observation_reply(b.finish_handoff()))
                }),
                "filled in by the person",
            ),
            (Waiting::Handoff { .. }, _) => (Err(ToolError::HandoffDeclined), "declined"),
            (Waiting::Approval { .. }, _) => (Err(ToolError::HandoffDeclined), "declined"),
        };
        let reply = self.conclude(&parked.request, result, Some(outcome));
        let _ = parked.reply.send(reply);
    }

    // Run `op` on the browser, surviving engine panics: a panic throws the
    // state away and starts over on the same URL (concept.md "엔진 패닉").
    fn execute_guarded(
        &mut self,
        request: &Request,
        op: impl FnOnce(&mut AgentBrowser) -> Result<Reply, ToolError>,
    ) -> Result<Reply, ToolError> {
        match catch_unwind(AssertUnwindSafe(|| op(&mut self.browser))) {
            Ok(result) => result,
            Err(_) => {
                let last_url = self
                    .browser
                    .state()
                    .current_url
                    .as_ref()
                    .map(|u| u.to_string());
                eprintln!("[mb-mcp] engine panicked handling {request:?}; rebuilding");
                self.browser = new_browser(&self.options);
                if let Some(url) = last_url {
                    let _ = self.browser.navigate(&url);
                }
                self.parked = None;
                Err(ToolError::Crashed)
            }
        }
    }

    // Turn a result into the reply: error reports with repeat counting,
    // and a timeline entry either way.
    fn conclude(
        &mut self,
        request: &Request,
        result: Result<Reply, ToolError>,
        outcome: Option<&str>,
    ) -> Reply {
        let reply = match result {
            Ok(reply) => {
                self.repeats.success();
                reply
            }
            Err(error) => {
                let repeat_count = self.repeats.failure(request, error.code());
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
                    observation: error.observation_blocks(),
                })
            }
        };
        let label = match outcome {
            Some(outcome) => format!("{} ({outcome})", request.label()),
            None => request.label(),
        };
        self.timeline.push(TimelineEntry {
            label,
            error: match &reply {
                Reply::Error(report) => Some(report.code.clone()),
                _ => None,
            },
        });
        if self.timeline.len() > TIMELINE_LEN {
            self.timeline.remove(0);
        }
        reply
    }
}

/// What a parked request is waiting for.
#[derive(Debug, Clone, PartialEq)]
pub enum Waiting {
    /// Ask mode: approve this risky action ("POST http://…/signup").
    Approval { action: String },
    /// A password field the person has to fill in themselves.
    Handoff { id: String },
}

impl Waiting {
    fn describe(&self) -> String {
        match self {
            Waiting::Approval { action } => action.clone(),
            Waiting::Handoff { id } => format!("Typing into password field {id}"),
        }
    }
}

/// The person's answer from the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Approve,
    Deny,
    TakeOver,
    HandoffDone,
    HandoffCancel,
}

struct Parked {
    request: Request,
    reply: oneshot::Sender<Reply>,
    waiting: Waiting,
}

impl Request {
    /// Short, log-safe description (typed text reduced to its length).
    pub fn label(&self) -> String {
        match self {
            Request::Navigate { url } => format!("navigate {url}"),
            Request::Observe { full, .. } => {
                if *full {
                    "observe (full)".into()
                } else {
                    "observe".into()
                }
            }
            Request::Click { id } => format!("click {id}"),
            Request::Type {
                id, text, submit, ..
            } => format!(
                "type {id} ({} chars){}",
                text.chars().count(),
                if *submit { " + Enter" } else { "" }
            ),
            Request::Press { key } => format!("press {key}"),
            Request::Scroll { dy } => format!("scroll {dy:+}"),
            Request::Back => "back".into(),
            Request::Forward => "forward".into(),
            Request::Screenshot => "screenshot".into(),
            Request::WaitFor { text, id, .. } => match (text, id) {
                (Some(_), _) => "wait_for text".into(),
                (_, Some(id)) => format!("wait_for {id}"),
                _ => "wait_for".into(),
            },
        }
    }
}

/// The reply every tool call gets once the observation window was closed.
pub fn browser_closed() -> Reply {
    Reply::Error(ErrorReport {
        code: "browser_closed".into(),
        message: "The person watching closed the browser window.".into(),
        hint:
            "Tell the user the browser was closed; it can't be used until the MCP server restarts."
                .into(),
        repeat_count: 1,
        observation: None,
    })
}

/// Headless: serve jobs until every sender is gone (the MCP server shut down).
pub fn run(options: Options, jobs: Receiver<Job>) {
    let mut worker = Worker::new(options);
    while let Ok(job) = jobs.recv() {
        let reply = worker.process(&job.request);
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

/// Parse a client error before it reaches the browser.
#[derive(Debug)]
enum ToolError {
    Agent(AgentError),
    InvalidId(String),
    InvalidArguments(&'static str),
    /// The person chose to do the held action themselves.
    TakenOver((String, String)),
    /// The person declined to fill in the field handed to them.
    HandoffDeclined,
    /// The engine panicked; the browser was rebuilt.
    Crashed,
}

impl ToolError {
    fn code(&self) -> &'static str {
        match self {
            ToolError::Agent(e) => e.code(),
            ToolError::InvalidId(_) => "invalid_id",
            ToolError::InvalidArguments(_) => "invalid_arguments",
            ToolError::TakenOver(_) => "taken_over",
            ToolError::HandoffDeclined => "handoff_declined",
            ToolError::Crashed => "engine_crashed",
        }
    }

    fn message(&self) -> String {
        match self {
            ToolError::Agent(e) => e.message(),
            ToolError::InvalidId(raw) => format!("{raw:?} is not an element id."),
            ToolError::InvalidArguments(why) => (*why).to_string(),
            ToolError::TakenOver(_) => {
                "The person watching chose to do this step themselves.".into()
            }
            ToolError::HandoffDeclined => {
                "The person watching declined to fill in that field.".into()
            }
            ToolError::Crashed => {
                "The browser engine crashed and was restarted on the same URL.".into()
            }
        }
    }

    fn hint(&self) -> &'static str {
        match self {
            ToolError::Agent(e) => e.hint(),
            ToolError::InvalidId(_) => {
                "Element ids look like \"e21\"; copy one from the latest observation."
            }
            ToolError::InvalidArguments(_) => "Check the tool's parameters.",
            ToolError::TakenOver(_) => {
                "Don't repeat it. Call `observe` later to see what they did, or ask them."
            }
            ToolError::HandoffDeclined => "Ask the user how they want to proceed.",
            ToolError::Crashed => "Call `observe` to see the reloaded page; form input was lost.",
        }
    }

    fn observation_blocks(&self) -> Option<(String, String)> {
        match self {
            ToolError::Agent(e) => e.observation().map(Observation::to_blocks),
            ToolError::TakenOver(blocks) => Some(blocks.clone()),
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
    encode_rgb_png(&shot.pixels, shot.width, shot.height)
}

/// `0x00RRGGBB` pixels → PNG bytes.
pub fn encode_rgb_png(pixels: &[u32], width: usize, height: usize) -> Vec<u8> {
    use image::ImageEncoder;
    let mut rgb = Vec::with_capacity(pixels.len() * 3);
    for pixel in pixels {
        rgb.push((pixel >> 16) as u8);
        rgb.push((pixel >> 8) as u8);
        rgb.push(*pixel as u8);
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            &rgb,
            width as u32,
            height as u32,
            image::ExtendedColorType::Rgb8,
        )
        .expect("encoding an in-memory RGB buffer as PNG can't fail");
    png
}

#[cfg(test)]
mod tests {
    use super::*;
    use mb_runtime::input::WindowInput;

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

    // A tiny page server for the parking tests: a password field and a link
    // to another origin (same listener under its `localhost` name).
    fn serve_page() -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let other = base.replace("127.0.0.1", "localhost");
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                        break;
                    }
                }
                let body = format!(
                    "<html><body><label for=\"pw\">Password</label><input id=\"pw\" type=\"password\"><a href=\"{other}/x\">Elsewhere</a></body></html>"
                );
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        base
    }

    fn worker_on(base: &str, approve: mb_agent::ApproveMode) -> (Worker, String, String) {
        let mut agent = mb_agent::AgentConfig {
            viewport_width: 800,
            viewport_height: 600,
            approve,
            ..mb_agent::AgentConfig::default()
        };
        agent.system_fonts = false;
        let mut worker = Worker::new(Options {
            agent,
            start_url: None,
            window: true,
            preview: None,
            preview_clicks: Vec::new(),
            preview_scale: 1.0,
        });
        let Reply::Observation { page, .. } = worker.process(&Request::Navigate {
            url: format!("{base}/"),
        }) else {
            panic!("navigate failed");
        };
        let id = |name: &str| {
            let line = page.lines().find(|l| l.contains(name)).unwrap();
            line[1..line.find(']').unwrap()].to_string()
        };
        let (pw, link) = (id("name=\"Password\""), id("name=\"Elsewhere\""));
        (worker, pw, link)
    }

    fn submit(worker: &mut Worker, request: Request) -> oneshot::Receiver<Reply> {
        let (reply, rx) = oneshot::channel();
        worker.submit(Job { request, reply });
        rx
    }

    #[test]
    fn password_typing_parks_until_the_person_is_done() {
        let base = serve_page();
        let (mut worker, pw, _) = worker_on(&base, mb_agent::ApproveMode::Deny);
        let mut rx = submit(
            &mut worker,
            Request::Type {
                id: pw.clone(),
                text: "secret".into(),
                clear: false,
                submit: false,
            },
        );
        assert_eq!(worker.waiting(), Some(&Waiting::Handoff { id: pw.clone() }));
        assert!(rx.try_recv().is_err(), "no reply while the person types");
        // Another request meanwhile is refused, not queued.
        let mut other = submit(&mut worker, Request::Scroll { dy: 10.0 });
        let Ok(Reply::Error(busy)) = other.try_recv() else {
            panic!("expected busy error")
        };
        assert_eq!(busy.code, "approval_pending");

        // The person clicks the field in the window and types.
        let field = worker
            .browser()
            .element_boxes()
            .into_iter()
            .find(|e| format!("e{}", e.id.raw()) == pw)
            .unwrap()
            .rect;
        let at = (
            field.x + field.width / 2.0,
            field.y + field.height / 2.0 + mb_runtime::chrome::CHROME_HEIGHT,
        );
        let click = WindowInput {
            mouse_position: Some(at),
            left_mouse_pressed: true,
            left_mouse_held: true,
            ..WindowInput::default()
        };
        worker.browser().human_frame(&click);
        worker.browser().human_frame(&WindowInput {
            mouse_position: Some(at),
            ..WindowInput::default()
        });
        worker.browser().human_frame(&WindowInput {
            typed: "hunter2".into(),
            mouse_position: Some(at),
            ..WindowInput::default()
        });

        worker.answer(Answer::HandoffDone);
        let Ok(Reply::Observation { browser, page }) = rx.try_recv() else {
            panic!("expected an observation")
        };
        // What the person changed is summarised; the value never shows.
        let changed = browser.lines().find(|l| l.starts_with("changed:")).unwrap();
        assert!(changed.contains(&format!("values: {pw}")), "{changed}");
        assert!(!browser.contains("hunter2") && !page.contains("hunter2"));
        assert!(!browser.contains("the person used the window"));
        assert!(worker.waiting().is_none());
        assert!(
            worker
                .timeline()
                .last()
                .unwrap()
                .label
                .ends_with("(filled in by the person)")
        );
    }

    #[test]
    fn held_navigation_answers_deny_and_take_over() {
        let base = serve_page();
        let (mut worker, _, link) = worker_on(&base, mb_agent::ApproveMode::Ask);

        let mut rx = submit(&mut worker, Request::Click { id: link.clone() });
        assert!(matches!(worker.waiting(), Some(Waiting::Approval { .. })));
        worker.answer(Answer::TakeOver);
        let Ok(Reply::Error(report)) = rx.try_recv() else {
            panic!("expected an error reply")
        };
        assert_eq!(report.code, "taken_over");
        assert!(
            report.observation.is_some(),
            "the page comes back with the error"
        );

        // Taking over counts as a refusal for the agent: asking again for
        // the same origin is refused without a new card.
        let mut again = submit(&mut worker, Request::Click { id: link });
        assert!(worker.waiting().is_none());
        let Ok(Reply::Error(report)) = again.try_recv() else {
            panic!("expected a refusal")
        };
        assert_eq!(report.code, "approval_denied");
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
