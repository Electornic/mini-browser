// The browser thread (docs/concept.md "프로세스·스레드 배치"). `AgentBrowser`
// holds `Rc`s (DOM, JS runtime) and can't leave the thread it was built on,
// so the MCP server (tokio, other thread) sends it `Job`s over a channel and
// awaits each reply on a oneshot. Jobs run strictly one at a time.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use mb_agent::{
    AgentBrowser, AgentError, ApproveMode, Observation, ObserveOptions, PendingWait, Scope,
    Screenshot, WaitCondition,
};
use mb_runtime::dom::NodeId;
use tokio::sync::oneshot;

use crate::cli::Options;
use crate::session_log::{SessionLog, StepRecord, Target, resolved_by, result_record, target_of};

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

/// A parked call returns `still_waiting` after this long and the agent
/// calls again to keep waiting: HTTP clients give up on a tool call at
/// about 60 s (Claude Code), people take longer than that to type a
/// password.
const PARK_REPLY_AFTER: Duration = Duration::from_secs(45);

/// The browser plus everything that survives across requests: repeat
/// tracking, the timeline, and how to rebuild after a panic. Both the
/// headless loop (`run`) and the observation window drive it.
pub struct Worker {
    options: Options,
    browser: AgentBrowser,
    repeats: RepeatTracker,
    timeline: Vec<TimelineEntry>,
    parked: Option<Parked>,
    // Window mode: a `wait_for` the frames are checking (it must not block
    // the window), and jobs that arrived meanwhile, in order.
    page_wait: Option<(Job, PendingWait, StepContext)>,
    deferred: VecDeque<Job>,
    // The person answered while no call was attached: the reply waits for
    // the agent's next identical call.
    unclaimed: Option<(Request, Reply)>,
    // How long a call stays parked before it returns `still_waiting`.
    park_reply_after: Duration,
    // `--log`: one line per finished call (G5a).
    log: Option<SessionLog>,
    // The person paused the agent from the window's ⋮ menu: no agent call
    // runs until they resume. The one call holding meanwhile (parked like
    // an approval: `paused` after `park_reply_after`, re-attach by
    // repeating it).
    paused: bool,
    held: Option<Held>,
}

/// An agent call that arrived while the agent was paused.
struct Held {
    request: Request,
    // None once it answered `paused` (or its client gave up).
    reply: Option<oneshot::Sender<Reply>>,
    since: std::time::Instant,
}

/// Trusted note on a result that waited for the person to resume.
const RESUMED_NOTE: &str =
    "the person paused you from the window; this call ran after they resumed";

/// What a session-log line needs from before the action ran.
#[derive(Debug, Clone, Default)]
struct StepContext {
    target: Option<Target>,
    person_moved: bool,
    approve: ApproveMode,
}

impl Worker {
    pub fn new(options: Options) -> Self {
        let browser = new_browser(&options);
        let log = options.log.as_ref().map(|path| {
            SessionLog::create(path, &options).unwrap_or_else(|error| {
                eprintln!("mb-mcp: --log {}: {error}", path.display());
                std::process::exit(2);
            })
        });
        Self {
            options,
            browser,
            repeats: RepeatTracker::default(),
            timeline: Vec::new(),
            parked: None,
            page_wait: None,
            deferred: VecDeque::new(),
            unclaimed: None,
            park_reply_after: PARK_REPLY_AFTER,
            log,
            paused: false,
            held: None,
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
        let context = self.begin_step(request);
        let result = self.execute_guarded(request, |browser| execute(browser, request));
        let reply = self.conclude(request, result, None);
        self.log_step(request, &context, None, &reply);
        reply
    }

    /// Window mode: like `process`, but an action that needs the person
    /// watching — approval in Ask mode, or a password handoff — is parked
    /// with its reply channel until `answer`. Only one can wait at a time.
    pub fn submit(&mut self, job: Job) {
        // The person answered while the agent was away: this call gets it
        // if it repeats the request; any other call means it moved on.
        if let Some((request, reply)) = self.unclaimed.take()
            && request == job.request
        {
            let _ = job.reply.send(reply);
            return;
        }
        let job = match self.reattach(job) {
            Some(job) => job,
            None => return,
        };
        if self.paused {
            self.hold(job);
            return;
        }
        self.run_now(job, None);
    }

    // The same call as the parked one, after it answered `still_waiting`
    // (or its client gave up): wait on the question again. Hands any
    // other job back.
    fn reattach(&mut self, job: Job) -> Option<Job> {
        match &mut self.parked {
            Some(parked)
                if parked.request == job.request
                    && parked.reply.as_ref().is_none_or(|r| r.is_closed()) =>
            {
                parked.reply = Some(job.reply);
                parked.since = std::time::Instant::now();
                None
            }
            _ => Some(job),
        }
    }

    // Paused: the call waits for the person to resume. One call holds at
    // a time — the same call again re-attaches, a different one replaces
    // a call nobody waits on any more (the agent moved on), and a second
    // call while one is still attached is told `paused` right away.
    fn hold(&mut self, job: Job) {
        let now = std::time::Instant::now();
        let attached = |held: &Held| held.reply.as_ref().is_some_and(|r| !r.is_closed());
        match &mut self.held {
            Some(held) if attached(held) => {
                let reply = self.conclude(&job.request, Err(ToolError::Paused), None);
                let _ = job.reply.send(reply);
            }
            Some(held) if held.request == job.request => {
                held.reply = Some(job.reply);
                held.since = now;
            }
            _ => {
                self.held = Some(Held {
                    request: job.request,
                    reply: Some(job.reply),
                    since: now,
                });
            }
        }
    }

    // Run the call now (or start its wait, or park it for the person).
    // `note`: a trusted note for its result, when it comes right away.
    fn run_now(&mut self, job: Job, note: Option<&'static str>) {
        if self.page_wait.is_some() {
            // Runs once the wait ends (`tick`).
            self.deferred.push_back(job);
            return;
        }
        if let Request::WaitFor {
            text,
            id,
            timeout_ms,
        } = &job.request
            && self.parked.is_none()
        {
            let timeout = Duration::from_millis(*timeout_ms);
            let context = self.begin_step(&job.request);
            let begun = wait_condition(text, id).and_then(|condition| {
                self.browser
                    .begin_wait(condition, timeout)
                    .map_err(ToolError::from)
            });
            match begun {
                Ok(wait) => self.page_wait = Some((job, wait, context)),
                Err(error) => {
                    let reply = self.conclude(&job.request, Err(error), None);
                    self.log_step(&job.request, &context, None, &reply);
                    let _ = job.reply.send(reply);
                }
            }
            return;
        }
        if let Some(parked) = &self.parked {
            let busy = ToolError::Busy(parked.waiting.clone());
            let reply = self.conclude(&job.request, Err(busy), None);
            let _ = job.reply.send(reply);
            return;
        }
        let context = self.begin_step(&job.request);
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
                self.browser.begin_handoff(*id);
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
                    reply: Some(job.reply),
                    waiting,
                    since: std::time::Instant::now(),
                    context,
                });
            }
            None => {
                let mut reply = self.conclude(&job.request, result, None);
                if let Some(note) = note {
                    add_note(&mut reply, note);
                }
                self.log_step(&job.request, &context, None, &reply);
                let _ = job.reply.send(reply);
            }
        }
    }

    /// The person paused the agent (⋮ menu). Person-only: no MCP tool
    /// reaches this. Calls from now on wait for `resume`; a `wait_for` in
    /// progress ends with `paused` (its result would be the person's
    /// page). A question card already on screen stays answerable.
    pub fn pause(&mut self) {
        if self.paused {
            return;
        }
        self.paused = true;
        if let Some((job, _, _)) = self.page_wait.take() {
            let reply = self.conclude(&job.request, Err(ToolError::Paused), None);
            let _ = job.reply.send(reply);
        }
        // Calls queued behind that wait now hold (or are told `paused`).
        self.run_deferred();
    }

    /// The person resumed the agent. The held call runs now, against the
    /// page as it is (ids are checked as usual), if its caller is still
    /// waiting; a call that already answered `paused` and wasn't repeated
    /// is dropped — the agent gets on with its next call.
    pub fn resume(&mut self) {
        if !self.paused {
            return;
        }
        self.paused = false;
        if let Some(held) = self.held.take() {
            match held.reply {
                Some(reply) if !reply.is_closed() => self.run_now(
                    Job {
                        request: held.request,
                        reply,
                    },
                    Some(RESUMED_NOTE),
                ),
                _ => self.push_timeline(
                    format!("{} (not run: paused)", held.request.label()),
                    Some("paused".into()),
                ),
            }
        }
        self.run_deferred();
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// The call waiting for the person to resume, for the panel.
    pub fn held_call(&self) -> Option<String> {
        self.held.as_ref().map(|held| held.request.label())
    }

    /// The approval mode the next agent action runs under.
    pub fn approve_mode(&self) -> ApproveMode {
        self.options.agent.approve
    }

    /// The person switched the approval mode (⋮ menu). Person-only: no
    /// MCP tool reaches this. Applies from the next agent action; an
    /// approval card already on screen keeps waiting for the person's
    /// answer (the mode change doesn't answer it). Kept in `options` too,
    /// so a browser rebuilt after a panic gets it.
    pub fn set_approve_mode(&mut self, mode: ApproveMode) {
        self.options.agent.approve = mode;
        self.browser.set_approve_mode(mode);
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
                        .map(|mut observation| {
                            // Instant or after minutes, it was the person.
                            observation
                                .notes
                                .push("the person approved this action in the window".into());
                            observation_reply(observation)
                        })
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
        self.log_step(&parked.request, &parked.context, Some(outcome), &reply);
        match parked.reply {
            Some(sender) if !sender.is_closed() => {
                let _ = sender.send(reply);
            }
            _ => self.unclaimed = Some((parked.request, reply)),
        }
        self.run_deferred();
    }

    /// Window mode, once per frame (after the person's input was applied):
    /// finish the pending `wait_for` if its condition holds or time is up.
    pub fn tick(&mut self) {
        self.release_long_parks();
        let Some((job, wait, context)) = self.page_wait.take() else {
            return;
        };
        let result = match self.execute_guarded(&job.request, |b| Ok(b.poll_wait(&wait))) {
            Ok(None) => {
                self.page_wait = Some((job, wait, context));
                return;
            }
            Ok(Some(result)) => result.map(observation_reply).map_err(ToolError::from),
            Err(error) => Err(error),
        };
        let reply = self.conclude(&job.request, result, None);
        self.log_step(&job.request, &context, None, &reply);
        let _ = job.reply.send(reply);
        self.run_deferred();
    }

    /// When the attached parked call is due its `still_waiting` reply (the
    /// window schedules a frame for then).
    pub fn park_deadline(&self) -> Option<std::time::Instant> {
        let parked = self
            .parked
            .as_ref()
            .filter(|p| p.reply.is_some())
            .map(|p| p.since);
        let held = self
            .held
            .as_ref()
            .filter(|h| h.reply.is_some())
            .map(|h| h.since);
        parked
            .into_iter()
            .chain(held)
            .min()
            .map(|since| since + self.park_reply_after)
    }

    /// The agent's pending `wait_for`, for the panel: what and time left.
    pub fn page_wait(&self) -> Option<(&WaitCondition, Duration)> {
        self.page_wait
            .as_ref()
            .map(|(_, wait, _)| (wait.condition(), wait.remaining()))
    }

    // Before an action: what the session log needs to know about it.
    fn begin_step(&mut self, request: &Request) -> StepContext {
        if self.log.is_none() {
            return StepContext::default();
        }
        let id = match request {
            Request::Click { id } | Request::Type { id, .. } => Some(id),
            Request::WaitFor { id: Some(id), .. } => Some(id),
            _ => None,
        };
        StepContext {
            target: id
                .and_then(|id| parse_id(id).ok())
                .and_then(|id| target_of(&mut self.browser, id)),
            person_moved: self.browser.changed_by_person(),
            approve: self.options.agent.approve,
        }
    }

    // After a call's final result (not `still_waiting` or a busy reply).
    fn log_step(
        &mut self,
        request: &Request,
        context: &StepContext,
        outcome: Option<&str>,
        reply: &Reply,
    ) {
        if self.log.is_none() {
            return;
        }
        let result = result_record(&mut self.browser, reply);
        if let Some(log) = &mut self.log {
            log.step(
                request,
                StepRecord {
                    target: context.target.as_ref(),
                    person_moved: context.person_moved,
                    resolved_by: outcome.and_then(resolved_by),
                    approve: context.approve,
                },
                result,
            );
        }
    }

    // A parked call answers `still_waiting` after `park_reply_after` (or
    // detaches once its client gave up) while the question stays on
    // screen; the agent's identical next call re-attaches (`submit`).
    fn release_long_parks(&mut self) {
        if let Some(parked) = &mut self.parked
            && let Some(sender) = parked
                .reply
                .take_if(|r| r.is_closed() || parked.since.elapsed() >= self.park_reply_after)
            && !sender.is_closed()
        {
            let waiting = ToolError::StillWaiting(parked.waiting.clone());
            let _ = sender.send(error_reply(&waiting, 1));
        }
        if let Some(held) = &mut self.held
            && let Some(sender) = held
                .reply
                .take_if(|r| r.is_closed() || held.since.elapsed() >= self.park_reply_after)
            && !sender.is_closed()
        {
            let _ = sender.send(error_reply(&ToolError::Paused, 1));
        }
        if self
            .page_wait
            .as_ref()
            .is_some_and(|(job, _, _)| job.reply.is_closed())
        {
            self.page_wait = None;
        }
        self.deferred.retain(|job| !job.reply.is_closed());
    }

    // Jobs that queued behind a wait, until one of them waits again.
    fn run_deferred(&mut self) {
        while self.page_wait.is_none()
            && self.parked.is_none()
            && let Some(job) = self.deferred.pop_front()
        {
            self.submit(job);
        }
    }

    /// The window closed: everything still waiting gets `browser_closed`.
    pub fn close(&mut self) {
        let parked = self.parked.take().and_then(|p| p.reply);
        let held = self.held.take().and_then(|h| h.reply);
        let waiting = self.page_wait.take().map(|(job, _, _)| job.reply);
        for reply in parked
            .into_iter()
            .chain(held)
            .chain(waiting)
            .chain(self.deferred.drain(..).map(|job| job.reply))
        {
            let _ = reply.send(browser_closed());
        }
    }

    // Run `op` on the browser, surviving engine panics: a panic throws the
    // state away and starts over on the same URL (concept.md "엔진 패닉").
    fn execute_guarded<T>(
        &mut self,
        request: &Request,
        op: impl FnOnce(&mut AgentBrowser) -> Result<T, ToolError>,
    ) -> Result<T, ToolError> {
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
                error_reply(&error, repeat_count)
            }
        };
        let label = match outcome {
            Some(outcome) => format!("{} ({outcome})", request.label()),
            None => request.label(),
        };
        let error = match &reply {
            Reply::Error(report) => Some(report.code.clone()),
            _ => None,
        };
        self.push_timeline(label, error);
        reply
    }

    fn push_timeline(&mut self, label: String, error: Option<String>) {
        self.timeline.push(TimelineEntry { label, error });
        if self.timeline.len() > TIMELINE_LEN {
            self.timeline.remove(0);
        }
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
    // None while no call is attached (it returned `still_waiting`).
    reply: Option<oneshot::Sender<Reply>>,
    waiting: Waiting,
    since: std::time::Instant,
    context: StepContext,
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
    // No agent saw the start page: the first result must not call the
    // person's moves since then "changes since your last result".
    browser.forget_reported();
    browser
}

/// Parse a client error before it reaches the browser.
#[derive(Debug)]
pub(crate) enum ToolError {
    Agent(AgentError),
    InvalidId(String),
    InvalidArguments(&'static str),
    /// The person chose to do the held action themselves.
    TakenOver((String, String)),
    /// The person declined to fill in the field handed to them.
    HandoffDeclined,
    /// The engine panicked; the browser was rebuilt.
    Crashed,
    /// Another call is waiting for the person (approval / password).
    Busy(Waiting),
    /// This call waited a while; the question is still on screen.
    StillWaiting(Waiting),
    /// The person paused the agent from the window.
    Paused,
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
            ToolError::Busy(Waiting::Approval { .. }) => "approval_pending",
            ToolError::Busy(Waiting::Handoff { .. }) => "handoff_pending",
            ToolError::StillWaiting(_) => "still_waiting",
            ToolError::Paused => "paused",
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
            ToolError::Paused => {
                "The person watching paused the agent from the browser window; nothing was done."
                    .into()
            }
            ToolError::Busy(waiting) | ToolError::StillWaiting(waiting) => {
                let prefix = if matches!(self, ToolError::Busy(_)) {
                    "Another call is waiting for the person: "
                } else {
                    "The person hasn't answered yet: "
                };
                let what = match waiting {
                    Waiting::Approval { action } => format!(
                        "an approval card for \"{action}\" is shown in the browser window (Approve / Deny / Do it myself)."
                    ),
                    Waiting::Handoff { id } => format!(
                        "they are typing the password into field {id} in the browser window and press Done when finished."
                    ),
                };
                format!("{prefix}{what}")
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
            ToolError::Busy(_) => {
                "Repeat the call that is waiting (same tool, same arguments) to keep waiting for the answer, or tell the user what they need to do in the window."
            }
            ToolError::StillWaiting(_) => {
                "Call the same tool with the same arguments again to keep waiting (nothing was done yet), or tell the user what they need to do in the window."
            }
            ToolError::Paused => {
                "Call the same tool with the same arguments again to wait until they resume (it runs then), or tell the user the browser is paused."
            }
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

impl ToolError {
    // Waiting for the person or the page: repeating the call is the
    // right move, so the repeat warning doesn't apply.
    fn is_waiting(&self) -> bool {
        matches!(
            self.code(),
            "approval_pending" | "handoff_pending" | "still_waiting" | "wait_timeout" | "paused"
        )
    }

    fn person_changed_page(&self) -> bool {
        matches!(self, ToolError::Agent(e) if e.observation().is_some_and(|o| o.changed_by_person))
    }
}

pub(crate) fn error_reply(error: &ToolError, repeat_count: u32) -> Reply {
    let hint = if repeat_count >= REPEAT_LIMIT && !error.is_waiting() {
        STOP_REPEATING.to_string()
    } else {
        error.hint().to_string()
    };
    let mut message = error.message();
    if error.person_changed_page() {
        // Errors are read by code first; don't let this hide in the
        // observation block.
        message.push_str(
            " Meanwhile the person changed the page in the window: read the observation below (earlier ids are invalid).",
        );
    }
    Reply::Error(ErrorReport {
        code: error.code().to_string(),
        message,
        hint,
        repeat_count,
        observation: error.observation_blocks(),
    })
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

// A browser-written note at the end of the trusted block (never page text).
fn add_note(reply: &mut Reply, note: &'static str) {
    let browser = match reply {
        Reply::Observation { browser, .. } => browser,
        Reply::Error(ErrorReport {
            observation: Some((browser, _)),
            ..
        }) => browser,
        _ => return,
    };
    if !browser.ends_with('\n') {
        browser.push('\n');
    }
    browser.push_str(&format!("note: {note}\n"));
}

pub(crate) fn observation_reply(observation: Observation) -> Reply {
    let (browser, page) = observation.to_blocks();
    Reply::Observation { browser, page }
}

pub(crate) fn execute(browser: &mut AgentBrowser, request: &Request) -> Result<Reply, ToolError> {
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
            let condition = wait_condition(text, id)?;
            browser.wait_for(condition, Duration::from_millis(*timeout_ms))?
        }
    };
    Ok(observation_reply(observation))
}

fn wait_condition(text: &Option<String>, id: &Option<String>) -> Result<WaitCondition, ToolError> {
    match (text, id) {
        (Some(text), None) if !text.is_empty() => Ok(WaitCondition::Text(text.clone())),
        (None, Some(id)) => Ok(WaitCondition::Element(parse_id(id)?)),
        _ => Err(ToolError::InvalidArguments(
            "wait_for needs exactly one of `text` or `id`.",
        )),
    }
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
                let mut request_line = String::new();
                let _ = reader.read_line(&mut request_line);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                        break;
                    }
                }
                let body = if request_line.contains(" /x ") {
                    "<html><body><p>Arrived</p></body></html>".to_string()
                } else {
                    format!(
                        "<html><body><label for=\"pw\">Password</label><input id=\"pw\" type=\"password\"><a href=\"{other}/x\">Elsewhere</a></body></html>"
                    )
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

    fn worker_on(base: &str, approve: mb_agent::ApproveMode) -> (Worker, String, String) {
        worker_logging(base, approve, None)
    }

    fn worker_logging(
        base: &str,
        approve: mb_agent::ApproveMode,
        log: Option<std::path::PathBuf>,
    ) -> (Worker, String, String) {
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
            preview_menu: false,
            preview_pause: false,
            http_port: None,
            attach: None,
            log,
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

    // The person clicks element `id` in the window; returns where.
    fn person_click(worker: &mut Worker, id: &str) -> (f32, f32) {
        let rect = worker
            .browser()
            .element_boxes()
            .into_iter()
            .find(|e| format!("e{}", e.id.raw()) == id)
            .unwrap()
            .rect;
        let at = (
            rect.x + rect.width / 2.0,
            rect.y + rect.height / 2.0 + mb_runtime::chrome::CHROME_HEIGHT,
        );
        worker.browser().human_frame(&WindowInput {
            mouse_position: Some(at),
            left_mouse_pressed: true,
            left_mouse_held: true,
            ..WindowInput::default()
        });
        worker.browser().human_frame(&WindowInput {
            mouse_position: Some(at),
            ..WindowInput::default()
        });
        at
    }

    // Window frames until `rx` has a reply (the loop `window_mode` runs).
    fn frames_until_reply(
        worker: &mut Worker,
        rx: &mut tokio::sync::oneshot::Receiver<Reply>,
    ) -> Reply {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            worker.browser().human_frame(&WindowInput::default());
            worker.tick();
            if let Ok(reply) = rx.try_recv() {
                return reply;
            }
            assert!(std::time::Instant::now() < deadline, "no reply");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn window_wait_for_keeps_the_window_live_and_notes_the_person() {
        let base = serve_page();
        let (mut worker, _, link) = worker_on(&base, mb_agent::ApproveMode::Deny);
        let mut rx = submit(
            &mut worker,
            Request::WaitFor {
                text: Some("Arrived".into()),
                id: None,
                timeout_ms: 30_000,
            },
        );
        worker.tick();
        assert!(rx.try_recv().is_err(), "submit returns at once");
        assert!(worker.page_wait().is_some());
        // Another call meanwhile queues behind the wait (not refused).
        let mut queued = submit(
            &mut worker,
            Request::Observe {
                full: false,
                cursor: 0,
            },
        );
        assert!(queued.try_recv().is_err());

        // The person follows the link while the agent waits.
        person_click(&mut worker, &link);
        let Reply::Observation { browser, page } = frames_until_reply(&mut worker, &mut rx) else {
            panic!("expected the observation")
        };
        assert!(page.contains("Arrived"), "{page}");
        assert!(browser.contains("the person used the window"), "{browser}");
        // What happened during the wait, like any action's result.
        let changed = browser.lines().find(|l| l.starts_with("changed:")).unwrap();
        assert!(changed.contains("new document"), "{changed}");
        assert!(worker.page_wait().is_none());
        // Then the queued call ran, without repeating the note.
        let Ok(Reply::Observation { browser, .. }) = queued.try_recv() else {
            panic!("queued observe should have run")
        };
        assert!(!browser.contains("the person used the window"));
    }

    #[test]
    fn a_long_park_returns_still_waiting_and_the_same_call_reattaches() {
        let base = serve_page();
        let (mut worker, _, link) = worker_on(&base, mb_agent::ApproveMode::Ask);
        worker.park_reply_after = Duration::ZERO;
        let click = Request::Click { id: link.clone() };
        let mut first = submit(&mut worker, click.clone());
        worker.tick();
        let Ok(Reply::Error(report)) = first.try_recv() else {
            panic!("expected still_waiting")
        };
        assert_eq!(report.code, "still_waiting");
        assert!(report.hint.contains("same arguments"), "{}", report.hint);
        // The card stays up; nothing was decided.
        assert!(matches!(worker.waiting(), Some(Waiting::Approval { .. })));

        // Another call is told what is pending, and repeating that is fine
        // (no "stop repeating" escalation).
        for _ in 0..4 {
            let mut other = submit(&mut worker, Request::Scroll { dy: 10.0 });
            let Ok(Reply::Error(busy)) = other.try_recv() else {
                panic!("expected approval_pending")
            };
            assert_eq!(busy.code, "approval_pending");
            assert!(!busy.hint.starts_with("Stop repeating"), "{}", busy.hint);
        }

        // The same call again waits on the same card.
        worker.park_reply_after = Duration::from_secs(60);
        let mut again = submit(&mut worker, click);
        assert!(again.try_recv().is_err());
        worker.answer(Answer::Deny);
        let Ok(Reply::Error(report)) = again.try_recv() else {
            panic!("expected approval_denied")
        };
        assert_eq!(report.code, "approval_denied");
    }

    #[test]
    fn an_answer_given_while_the_agent_was_away_waits_for_its_next_call() {
        let base = serve_page();
        let (mut worker, _, link) = worker_on(&base, mb_agent::ApproveMode::Ask);
        let click = Request::Click { id: link };
        let rx = submit(&mut worker, click.clone());
        drop(rx); // the client timed out
        worker.tick();
        assert!(worker.waiting().is_some(), "the card stays up");
        worker.answer(Answer::TakeOver);
        assert!(worker.waiting().is_none());

        // The agent repeats its call and gets the person's answer.
        let mut again = submit(&mut worker, click.clone());
        let Ok(Reply::Error(report)) = again.try_recv() else {
            panic!("expected taken_over")
        };
        assert_eq!(report.code, "taken_over");
        // Only once: the next identical call runs normally (and is refused
        // from memory, since "do it myself" counts as a refusal).
        let mut third = submit(&mut worker, click);
        let Ok(Reply::Error(report)) = third.try_recv() else {
            panic!("expected approval_denied")
        };
        assert_eq!(report.code, "approval_denied");
    }

    #[test]
    fn an_error_says_when_the_person_changed_the_page() {
        let base = serve_page();
        let (mut worker, _, link) = worker_on(&base, mb_agent::ApproveMode::Deny);
        let mut rx = submit(
            &mut worker,
            Request::WaitFor {
                text: Some("never there".into()),
                id: None,
                timeout_ms: 1_500,
            },
        );
        person_click(&mut worker, &link);
        let Reply::Error(report) = frames_until_reply(&mut worker, &mut rx) else {
            panic!("expected wait_timeout")
        };
        assert_eq!(report.code, "wait_timeout");
        assert!(
            report.message.contains("the person changed the page"),
            "{}",
            report.message
        );
    }

    #[test]
    fn done_without_typing_is_reported() {
        let base = serve_page();
        let (mut worker, pw, _) = worker_on(&base, mb_agent::ApproveMode::Deny);
        let mut rx = submit(
            &mut worker,
            Request::Type {
                id: pw.clone(),
                text: "x".into(),
                clear: false,
                submit: false,
            },
        );
        worker.answer(Answer::HandoffDone);
        let Ok(Reply::Observation { browser, .. }) = rx.try_recv() else {
            panic!("expected the observation")
        };
        assert!(
            browser.contains(&format!(
                "note: the person pressed Done but field {pw} did not change"
            )),
            "{browser}"
        );
    }

    #[test]
    fn an_approved_action_says_the_person_approved_it() {
        let base = serve_page();
        let (mut worker, _, link) = worker_on(&base, mb_agent::ApproveMode::Ask);
        let mut rx = submit(&mut worker, Request::Click { id: link });
        worker.answer(Answer::Approve);
        let Ok(Reply::Observation { browser, page }) = rx.try_recv() else {
            panic!("expected the observation")
        };
        assert!(page.contains("Arrived"), "{page}");
        assert!(
            browser.contains("note: the person approved this action in the window"),
            "{browser}"
        );
    }

    #[test]
    fn the_start_page_is_not_mistaken_for_the_agents_last_result() {
        let base = serve_page();
        let mut agent = mb_agent::AgentConfig {
            viewport_width: 800,
            viewport_height: 600,
            ..mb_agent::AgentConfig::default()
        };
        agent.system_fonts = false;
        let mut worker = Worker::new(Options {
            agent,
            start_url: Some(format!("{base}/")),
            window: true,
            preview: None,
            preview_clicks: Vec::new(),
            preview_scale: 1.0,
            preview_menu: false,
            preview_pause: false,
            http_port: None,
            attach: None,
            log: None,
        });
        // The person follows the link before any agent call.
        let link = worker
            .browser()
            .element_boxes()
            .into_iter()
            .find(|e| e.name == "Elsewhere")
            .map(|e| format!("e{}", e.id.raw()))
            .unwrap();
        person_click(&mut worker, &link);
        for _ in 0..200 {
            worker.browser().human_frame(&WindowInput::default());
            if worker
                .browser()
                .state()
                .current_url
                .as_ref()
                .is_some_and(|u| u.path == "/x")
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let Reply::Observation { browser, page } = worker.process(&Request::Observe {
            full: false,
            cursor: 0,
        }) else {
            panic!("expected the observation")
        };
        assert!(page.contains("Arrived"), "{page}");
        assert!(
            !browser.contains("note:"),
            "first result has nothing to compare: {browser}"
        );
    }

    #[test]
    fn a_window_session_with_the_person_replays_with_the_secret() {
        let base = serve_page();
        let path = std::env::temp_dir().join(format!("mb-person-{}.jsonl", std::process::id()));
        let (mut worker, pw, link) =
            worker_logging(&base, mb_agent::ApproveMode::Ask, Some(path.clone()));
        // The person types the password the agent handed over…
        let mut typed = submit(
            &mut worker,
            Request::Type {
                id: pw.clone(),
                text: "x".into(),
                clear: false,
                submit: false,
            },
        );
        let at = person_click(&mut worker, &pw);
        worker.browser().human_frame(&WindowInput {
            typed: "hunter2".into(),
            mouse_position: Some(at),
            ..WindowInput::default()
        });
        worker.answer(Answer::HandoffDone);
        assert!(matches!(typed.try_recv(), Ok(Reply::Observation { .. })));
        // …and approves the move to the other origin.
        let mut moved = submit(&mut worker, Request::Click { id: link });
        worker.answer(Answer::Approve);
        assert!(matches!(moved.try_recv(), Ok(Reply::Observation { .. })));
        drop(worker);

        let log = std::fs::read_to_string(&path).unwrap();
        assert!(log.contains(r#""resolved_by":"person_filled""#), "{log}");
        assert!(log.contains(r#""resolved_by":"person_approved""#), "{log}");
        assert!(log.contains("<7 chars>"), "{log}");
        assert!(!log.contains("hunter2"), "no password in the log");

        let mut options = crate::cli::ReplayOptions {
            file: path.clone(),
            secrets: Vec::new(),
            ignore_text: false,
            window: false,
            preview: None,
            preview_steps: 0,
            preview_scale: 1.0,
        };
        let stop = crate::replay::replay(&options).unwrap_err();
        assert_eq!(stop.code, 3, "{}", stop.message);
        assert!(
            stop.message
                .contains("--secret \"Password\" with MB_SECRET_PASSWORD set"),
            "{}",
            stop.message
        );
        options.secrets.push(("Password".into(), "hunter2".into()));
        assert_eq!(crate::replay::replay(&options).unwrap(), 3);
        let _ = std::fs::remove_file(&path);
    }

    fn observe() -> Request {
        Request::Observe {
            full: false,
            cursor: 0,
        }
    }

    #[test]
    fn a_paused_agent_waits_and_its_call_runs_on_resume() {
        let base = serve_page();
        let (mut worker, _, link) = worker_on(&base, mb_agent::ApproveMode::Auto);
        worker.pause();
        let mut held = submit(&mut worker, Request::Click { id: link });
        worker.tick();
        assert!(held.try_recv().is_err(), "held, not run");
        assert!(worker.held_call().unwrap().starts_with("click e"));
        assert!(
            !worker
                .browser()
                .state()
                .current_url
                .as_ref()
                .unwrap()
                .path
                .contains("/x"),
            "nothing ran while paused"
        );
        // A second call while the first is still waiting is told at once,
        // and repeating that never escalates to "stop repeating".
        for _ in 0..4 {
            let mut other = submit(&mut worker, Request::Scroll { dy: 10.0 });
            let Ok(Reply::Error(report)) = other.try_recv() else {
                panic!("expected paused")
            };
            assert_eq!(report.code, "paused");
            assert!(report.hint.contains("same arguments"), "{}", report.hint);
            assert!(!report.hint.starts_with("Stop repeating"));
        }
        assert_eq!(
            worker.timeline().last().unwrap().error.as_deref(),
            Some("paused")
        );

        worker.resume();
        let Ok(Reply::Observation { browser, page }) = held.try_recv() else {
            panic!("the held click runs on resume")
        };
        assert!(page.contains("Arrived"), "{page}");
        assert!(browser.contains("note: the person paused you"), "{browser}");
        assert!(worker.held_call().is_none());
        // Back to normal: the next call runs right away, without the note.
        let mut next = submit(&mut worker, observe());
        let Ok(Reply::Observation { browser, .. }) = next.try_recv() else {
            panic!("runs at once")
        };
        assert!(!browser.contains("paused"), "{browser}");
    }

    #[test]
    fn a_held_call_answers_paused_reattaches_and_is_dropped_if_not_repeated() {
        let base = serve_page();
        let (mut worker, _, link) = worker_on(&base, mb_agent::ApproveMode::Auto);
        worker.park_reply_after = Duration::ZERO;
        worker.pause();
        let click = Request::Click { id: link };
        let mut first = submit(&mut worker, click.clone());
        // The window schedules a frame for the deadline.
        assert!(worker.park_deadline().is_some());
        worker.tick();
        let Ok(Reply::Error(report)) = first.try_recv() else {
            panic!("expected paused after the park time")
        };
        assert_eq!(report.code, "paused");
        assert!(
            report.message.contains("nothing was done"),
            "{}",
            report.message
        );
        assert!(worker.park_deadline().is_none(), "nobody attached");

        // The same call re-attaches and runs on resume.
        worker.park_reply_after = Duration::from_secs(60);
        let mut again = submit(&mut worker, click.clone());
        worker.tick();
        assert!(again.try_recv().is_err());
        worker.resume();
        assert!(matches!(again.try_recv(), Ok(Reply::Observation { .. })));

        // Paused again: a call that answered `paused` and wasn't repeated
        // is dropped on resume — the agent moved on.
        worker.process(&Request::Back);
        worker.park_reply_after = Duration::ZERO;
        worker.pause();
        let mut gone = submit(&mut worker, click);
        worker.tick();
        assert!(matches!(gone.try_recv(), Ok(Reply::Error(_))));
        worker.resume();
        let last = worker.timeline().last().unwrap();
        assert!(last.label.ends_with("(not run: paused)"), "{}", last.label);
        assert!(
            !worker
                .browser()
                .state()
                .current_url
                .as_ref()
                .unwrap()
                .path
                .contains("/x"),
            "the dropped click never ran"
        );
    }

    #[test]
    fn pausing_ends_a_wait_and_queued_calls_hold() {
        let base = serve_page();
        let (mut worker, _, _) = worker_on(&base, mb_agent::ApproveMode::Deny);
        let mut wait = submit(
            &mut worker,
            Request::WaitFor {
                text: Some("never there".into()),
                id: None,
                timeout_ms: 30_000,
            },
        );
        let mut queued = submit(&mut worker, observe());
        worker.pause();
        let Ok(Reply::Error(report)) = wait.try_recv() else {
            panic!("the wait ends with paused")
        };
        assert_eq!(report.code, "paused");
        assert!(worker.page_wait().is_none());
        assert!(queued.try_recv().is_err(), "the queued call now holds");
        assert_eq!(worker.held_call().as_deref(), Some("observe"));
        worker.resume();
        assert!(matches!(queued.try_recv(), Ok(Reply::Observation { .. })));
    }

    #[test]
    fn a_question_card_stays_answerable_while_paused() {
        let base = serve_page();
        let (mut worker, pw, _) = worker_on(&base, mb_agent::ApproveMode::Deny);
        let mut typed = submit(
            &mut worker,
            Request::Type {
                id: pw.clone(),
                text: "x".into(),
                clear: false,
                submit: false,
            },
        );
        worker.pause();
        // Other calls hold for the resume, not for the card.
        let mut other = submit(&mut worker, observe());
        assert!(other.try_recv().is_err());
        assert!(worker.waiting().is_some());
        worker.answer(Answer::HandoffDone);
        assert!(matches!(typed.try_recv(), Ok(Reply::Observation { .. })));
        assert!(other.try_recv().is_err(), "still paused");
        worker.resume();
        assert!(matches!(other.try_recv(), Ok(Reply::Observation { .. })));
    }

    #[test]
    fn the_approval_mode_switch_applies_to_the_next_action() {
        let base = serve_page();
        let (mut worker, _, link) = worker_on(&base, mb_agent::ApproveMode::Ask);
        let click = Request::Click { id: link.clone() };
        // A card already up isn't answered by switching the mode.
        let mut asked = submit(&mut worker, click.clone());
        worker.set_approve_mode(mb_agent::ApproveMode::Auto);
        assert!(asked.try_recv().is_err());
        assert!(matches!(worker.waiting(), Some(Waiting::Approval { .. })));
        worker.answer(Answer::Approve);
        assert!(matches!(asked.try_recv(), Ok(Reply::Observation { .. })));
        worker.process(&Request::Back);
        worker.process(&observe());

        // Refuse: the next move is refused without asking…
        worker.set_approve_mode(mb_agent::ApproveMode::Deny);
        let Reply::Error(report) = worker.process(&click) else {
            panic!("expected approval_denied")
        };
        assert_eq!(report.code, "approval_denied");
        // …and back to Ask, the person is asked again (the refusal was the
        // mode's, not theirs).
        worker.set_approve_mode(mb_agent::ApproveMode::Ask);
        let mut asked = submit(&mut worker, click.clone());
        assert!(matches!(worker.waiting(), Some(Waiting::Approval { .. })));
        worker.answer(Answer::Deny);
        assert!(matches!(asked.try_recv(), Ok(Reply::Error(_))));
        // A refusal by the person outlives switching to Allow.
        worker.set_approve_mode(mb_agent::ApproveMode::Auto);
        let Reply::Error(report) = worker.process(&click) else {
            panic!("expected the remembered refusal")
        };
        assert_eq!(report.code, "approval_denied");
        assert_eq!(worker.approve_mode(), mb_agent::ApproveMode::Auto);
        assert_eq!(worker.options().agent.approve, mb_agent::ApproveMode::Auto);
    }

    #[test]
    fn a_mode_switch_is_logged_per_step_and_replays() {
        let base = serve_page();
        let path = std::env::temp_dir().join(format!("mb-mode-{}.jsonl", std::process::id()));
        let (mut worker, _, link) =
            worker_logging(&base, mb_agent::ApproveMode::Deny, Some(path.clone()));
        worker.process(&observe());
        worker.set_approve_mode(mb_agent::ApproveMode::Auto);
        let Reply::Observation { page, .. } = worker.process(&Request::Click { id: link }) else {
            panic!("allowed after the switch")
        };
        assert!(page.contains("Arrived"));
        drop(worker);
        let log = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert!(lines[0].contains(r#""approve":"deny""#), "{log}");
        assert!(!lines[2].contains(r#""approve""#), "header mode: no field");
        assert!(lines[3].contains(r#""approve":"auto""#), "{log}");
        let options = crate::cli::ReplayOptions {
            file: path.clone(),
            secrets: Vec::new(),
            ignore_text: false,
            window: false,
            preview: None,
            preview_steps: 0,
            preview_scale: 1.0,
        };
        assert_eq!(crate::replay::replay(&options).unwrap(), 3);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn window_wait_for_times_out_without_blocking() {
        let base = serve_page();
        let (mut worker, _, _) = worker_on(&base, mb_agent::ApproveMode::Deny);
        let mut rx = submit(
            &mut worker,
            Request::WaitFor {
                text: Some("never there".into()),
                id: None,
                timeout_ms: 30,
            },
        );
        let Reply::Error(report) = frames_until_reply(&mut worker, &mut rx) else {
            panic!("expected wait_timeout")
        };
        assert_eq!(report.code, "wait_timeout");
        assert!(report.observation.is_some());
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
        assert_eq!(busy.code, "handoff_pending");
        assert!(busy.message.contains("press Done"), "{}", busy.message);

        // The person clicks the field in the window and types.
        let at = person_click(&mut worker, &pw);
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
        assert!(
            browser.contains(&format!("note: the person typed into field {pw}")),
            "{browser}"
        );
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
