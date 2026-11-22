// `mb-mcp --window`: the observation window (docs/concept.md G3).
//
// winit must own the main thread (macOS), so here the browser lives inside
// the window loop instead of `worker::run`. Each frame:
//   1. run every agent job that arrived (the MCP thread wakes the loop
//      after queueing one),
//   2. render the page for the person at the window (their input is
//      ordinary human input — the policy gate never judges it),
//   3. paint the overlay (id badges, target outline), the agent panel and
//      the ⋮ menu when open.
// The ⋮ menu and the panel are where the person runs the agent — pause /
// resume, approval mode, recording the session. They are window input
// only: no MCP tool reaches them (docs/concept.md "⋮ 메뉴"). The menu also
// opens "What the agent sees", a read-only viewer of the observation text
// (nothing in it goes to the agent).
// When the window closes, remaining and future tool calls get
// `browser_closed`. When the MCP client goes away, the process exits.
// With `--http` (G3b) the person owns the process instead: clients come
// and go, and closing the window ends it.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, OnceLock};

use std::time::Duration;

use mb_runtime::chrome::{CHROME_HEIGHT, ChromeExtras, menu_button_rect};
use mb_runtime::input::WindowInput;
use mb_runtime::layout::Rect;
use mb_runtime::palette;
use mb_runtime::render::{DisplayCommand, rasterize, scale_display_list};

use crate::cli::Options;
use crate::overlay;
use crate::worker::{self, Job, Worker};

/// Where the MCP thread finds the wake hook once the window exists.
pub type WakeSlot = Arc<OnceLock<Arc<dyn Fn() + Send + Sync>>>;

pub fn run(options: Options, jobs: Receiver<Job>, wake_slot: WakeSlot) {
    let (width, height) = (
        options.agent.viewport_width + overlay::PANEL_WIDTH,
        options.agent.viewport_height + CHROME_HEIGHT as usize,
    );
    let mut worker = Worker::new(options);
    let mut wake_registered = false;
    let mut client_gone = false;
    // Card buttons painted last frame, for hit-testing this frame's click.
    let mut buttons: Vec<overlay::Button> = Vec::new();
    // The ⋮ menu: open or not, and its items / bounds as painted last frame.
    let mut menu_open = false;
    let mut menu: Option<(Vec<overlay::Button>, Rect)> = None;
    let mut copied = false;
    let mut record_failed = false;
    // "What the agent sees", while open.
    let mut viewer: Option<Viewer> = None;
    // The parked-call deadline a wake-up is already scheduled for.
    let mut wake_scheduled: Option<std::time::Instant> = None;
    // The next REC-clock second a wake-up is scheduled for.
    let mut tick_scheduled: Option<std::time::Instant> = None;
    let attached = worker.options().attach.is_some();

    // Escape closes the viewer / menu; it must not close the agent's browser.
    let result = mb_shell::window::run_with(
        "mini-browser — agent view",
        width,
        height,
        false,
        |frame, input, wake| {
            let (window_width, window_height) = (frame.width, frame.height);
            if !wake_registered {
                let hook = wake.as_arc();
                let _ = wake_slot.set(hook.clone());
                worker.browser().state_mut().set_navigation_wake(hook);
                wake_registered = true;
            }
            // Agent actions lay out against the same page area the person
            // sees (compose() sets it again before painting).
            let (page_width, _) = overlay::layout(window_width);
            let page_height = window_height.saturating_sub(CHROME_HEIGHT as usize);
            worker.browser().set_viewport(page_width, page_height);

            // 1. The person's answer to a question card, if they clicked one.
            //    Clicks in the panel never reach the page.
            let mut page_input = input.clone();
            // "What the agent sees" is modal: while it is open nothing
            // reaches the page, the panel or the menu.
            if let Some(open) = &mut viewer {
                let routed = route_viewer_input(open.painted.as_ref(), input);
                open.scroll = open.scroll.saturating_add_signed(routed.scroll);
                match routed.click {
                    ViewerClick::None | ViewerClick::Swallow => {}
                    ViewerClick::Close => viewer = None,
                    ViewerClick::Act(action) => match action {
                        overlay::WindowAction::Scope(scope) if scope != open.scope => {
                            open.scope = scope;
                            open.scroll = 0;
                            open.copied = false;
                        }
                        overlay::WindowAction::CopyAgentView => {
                            open.copied = open
                                .shown
                                .as_ref()
                                .is_some_and(|(b, p)| copy_to_clipboard(&format!("{b}\n{p}")));
                        }
                        overlay::WindowAction::CloseAgentView => viewer = None,
                        _ => {}
                    },
                }
                page_input = WindowInput::default();
            }
            // The ⋮ menu takes clicks first: the button toggles it; while
            // it is open, a click picks an item or closes it, and never
            // reaches the page (or the panel) below.
            let routed = if viewer.is_some() {
                RoutedInput {
                    click: MenuClick::None,
                    over_menu: false,
                }
            } else {
                route_menu_input(menu_open, menu.as_ref(), page_width as f32, input)
            };
            let consumed = viewer.is_some() || routed.click != MenuClick::None;
            match routed.click {
                MenuClick::None | MenuClick::Swallow => {}
                MenuClick::Toggle => menu_open = !menu_open,
                MenuClick::Close | MenuClick::Escape => menu_open = false,
                MenuClick::Pick(overlay::PanelAction::Control(control)) => {
                    let ok = apply(&mut worker, control);
                    record_failed = matches!(control, overlay::Control::StartRecording) && !ok;
                    // The mode switch stays open to show the new
                    // selection (and a failed Record, its error); pause /
                    // resume / record are commands.
                    menu_open = matches!(control, overlay::Control::Approve(_)) || record_failed;
                }
                MenuClick::Pick(overlay::PanelAction::Window(
                    overlay::WindowAction::ShowAgentView,
                )) => {
                    menu_open = false;
                    viewer = Some(Viewer::default());
                }
                MenuClick::Pick(_) => {}
            }
            if routed.click == MenuClick::Escape {
                page_input.escape_pressed = false;
            }
            if consumed {
                page_input.left_mouse_pressed = false;
                page_input.left_mouse_held = false;
            }
            if routed.over_menu {
                page_input.mouse_position = None;
                page_input.left_mouse_held = false;
            }
            if let Some((x, y)) = input.mouse_position
                && x >= page_width as f32
                && !consumed
            {
                if input.left_mouse_pressed
                    && let Some(button) = buttons.iter().find(|b| b.contains(x, y))
                {
                    match button.action {
                        overlay::PanelAction::Answer(answer) => worker.answer(answer),
                        overlay::PanelAction::Replay(_) | overlay::PanelAction::Window(_) => {}
                        overlay::PanelAction::Control(control) => {
                            apply(&mut worker, control);
                        }
                        overlay::PanelAction::CopySetup => {
                            if let Some(attach) = &worker.options().attach {
                                copied = copy_to_clipboard(&attach.setup_command());
                            }
                        }
                    }
                }
                page_input.mouse_position = None;
                page_input.left_mouse_pressed = false;
                page_input.left_mouse_held = false;
            }

            // 2. Agent jobs. Each runs to completion (action + settle), or
            //    parks if it needs the person, before the frame is painted.
            loop {
                match jobs.try_recv() {
                    Ok(job) => worker.submit(job),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        client_gone = true;
                        break;
                    }
                }
            }
            if client_gone {
                // stdin closed: the client is done with us (concept.md
                // "종료·오류 처리"). Nothing else needs flushing.
                eprintln!("[mb-mcp] client disconnected; closing the window");
                std::process::exit(0);
            }

            // 3. The page as the person drives it, with what the agent sees
            //    drawn over it.
            let composed = compose(
                &mut worker,
                &page_input,
                window_width,
                window_height,
                &mut View {
                    copied,
                    menu_open,
                    record_failed,
                    hover: input.mouse_position,
                    viewer: viewer.as_mut(),
                },
            );
            buttons = composed.buttons;
            menu = composed.menu;
            let commands = composed.commands;
            frame.present(&commands);
            // A pending `wait_for` is checked after every frame, so keep
            // frames coming until it ends (the window never blocks on it).
            worker.tick();
            // Nobody may touch the window while a call is parked: make sure
            // a frame runs when it is due its `still_waiting` reply.
            if let Some(deadline) = worker.park_deadline()
                && wake_scheduled != Some(deadline)
            {
                wake_scheduled = Some(deadline);
                let wake = wake.as_arc();
                std::thread::spawn(move || {
                    std::thread::sleep(
                        deadline.saturating_duration_since(std::time::Instant::now()),
                    );
                    wake();
                });
            }
            // The REC clock: one frame per second while recording.
            if let Some(recording) = worker.recording() {
                let next =
                    recording.since + Duration::from_secs(recording.since.elapsed().as_secs() + 1);
                if tick_scheduled != Some(next) {
                    tick_scheduled = Some(next);
                    let wake = wake.as_arc();
                    std::thread::spawn(move || {
                        std::thread::sleep(
                            next.saturating_duration_since(std::time::Instant::now()),
                        );
                        wake();
                    });
                }
            }
            worker.browser().state().wants_continuous_redraw() || worker.page_wait().is_some()
        },
    );
    if let Err(error) = result {
        eprintln!("[mb-mcp] window error: {error}");
    }

    worker.close();
    if attached {
        // `--http`: the person closed their browser; that's the end.
        eprintln!("[mb-mcp] window closed; exiting");
        return;
    }
    // The person closed the window. Keep answering so the client gets a
    // clear error instead of a dead pipe, until it disconnects.
    eprintln!("[mb-mcp] window closed; answering browser_closed");
    for job in jobs.iter() {
        let _ = job.reply.send(worker::browser_closed());
    }
}

/// What a frame's mouse input means to the ⋮ menu.
#[derive(Debug, Clone, Copy, PartialEq)]
enum MenuClick {
    /// Not the menu's: the page or panel gets it.
    None,
    Toggle,
    Close,
    /// On the menu but not on an item (padding, a greyed-out row).
    Swallow,
    /// Escape while open: closes it, and the page doesn't get the key.
    Escape,
    Pick(overlay::PanelAction),
}

struct RoutedInput {
    click: MenuClick,
    /// The mouse is over the open menu: the page mustn't hover under it.
    over_menu: bool,
}

// `menu` is the open menu as painted last frame (items, bounds).
fn route_menu_input(
    open: bool,
    menu: Option<&(Vec<overlay::Button>, Rect)>,
    page_width: f32,
    input: &WindowInput,
) -> RoutedInput {
    if open && input.escape_pressed {
        return RoutedInput {
            click: MenuClick::Escape,
            over_menu: false,
        };
    }
    let Some(point) = input.mouse_position else {
        return RoutedInput {
            click: MenuClick::None,
            over_menu: false,
        };
    };
    let menu = menu.filter(|_| open);
    let over_menu = menu.is_some_and(|(_, rect)| contains(rect, point));
    let click = if !input.left_mouse_pressed {
        MenuClick::None
    } else if contains(&menu_button_rect(page_width), point) {
        MenuClick::Toggle
    } else if let Some((items, _)) = menu {
        match items.iter().find(|b| b.contains(point.0, point.1)) {
            Some(button) => MenuClick::Pick(button.action),
            // A click outside closes the menu; either way it stops here.
            _ if over_menu => MenuClick::Swallow,
            _ => MenuClick::Close,
        }
    } else {
        MenuClick::None
    };
    RoutedInput { click, over_menu }
}

fn contains(rect: &Rect, (x, y): (f32, f32)) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

// The person's controls (⋮ menu, panel). Logged so a session's stderr
// shows when the person took the wheel. False if it couldn't be done
// (recording: the directory couldn't be created).
fn apply(worker: &mut Worker, control: overlay::Control) -> bool {
    match control {
        overlay::Control::Pause => {
            eprintln!("[mb-mcp] the person paused the agent");
            worker.pause();
        }
        overlay::Control::Resume => {
            eprintln!("[mb-mcp] the person resumed the agent");
            worker.resume();
        }
        overlay::Control::Approve(mode) => {
            if worker.approve_mode() != mode {
                eprintln!(
                    "[mb-mcp] the person set approval to {}",
                    crate::session_log::approve_name(mode)
                );
                worker.set_approve_mode(mode);
            }
        }
        overlay::Control::StartRecording => match worker.start_recording() {
            Ok(()) => {
                if let Some(recording) = worker.recording() {
                    eprintln!(
                        "[mb-mcp] the person started recording into {}",
                        recording.dir.display()
                    );
                }
            }
            Err(error) => {
                eprintln!("[mb-mcp] couldn't start recording: {error}");
                return false;
            }
        },
        overlay::Control::StopRecording => {
            if let Some(recording) = worker.recording() {
                let file = recording
                    .file
                    .map_or("no file (no agent call)".into(), |f| {
                        f.display().to_string()
                    });
                eprintln!(
                    "[mb-mcp] the person stopped recording: {file}, {} steps",
                    recording.steps
                );
            }
            worker.stop_recording();
        }
    }
    true
}

/// "What the agent sees" while open (board 8).
#[derive(Default)]
struct Viewer {
    scope: overlay::ViewScope,
    scroll: usize,
    copied: bool,
    // The blocks on screen now (what Copy copies).
    shown: Option<(String, String)>,
    // Whole page: the snapshot, and the page / agent result it was taken
    // for — retaken when either changes.
    whole: Option<((String, String), WholeKey)>,
    // Buttons, dialog rect and page size as painted last frame.
    painted: Option<ViewerPainted>,
}

type WholeKey = (u64, Option<worker::Seen>);

struct ViewerPainted {
    buttons: Vec<overlay::Button>,
    rect: Rect,
    page_rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ViewerClick {
    None,
    /// Inside the dialog, not on a button.
    Swallow,
    /// Escape, or a click outside the dialog.
    Close,
    Act(overlay::WindowAction),
}

struct ViewerInput {
    click: ViewerClick,
    /// Rows to scroll (wheel, arrows, page up / down).
    scroll: isize,
}

// The viewer is modal: every click and key is its own. `painted` is the
// viewer as painted last frame (none on the frame it opened).
fn route_viewer_input(painted: Option<&ViewerPainted>, input: &WindowInput) -> ViewerInput {
    let page_rows = painted.map_or(10, |p| p.page_rows) as isize;
    let mut scroll = 0;
    // Wheel: `scroll_y` is in lines of 24 px, positive = up.
    scroll -= (input.scroll_y * 24.0 / 18.0).round() as isize;
    scroll += input.move_down as isize - input.move_up as isize;
    scroll += (input.page_down_pressed as isize - input.page_up_pressed as isize) * page_rows;
    let click = if input.escape_pressed {
        ViewerClick::Close
    } else if let (true, Some((x, y)), Some(painted)) =
        (input.left_mouse_pressed, input.mouse_position, painted)
    {
        match painted.buttons.iter().find(|b| b.contains(x, y)) {
            Some(overlay::Button {
                action: overlay::PanelAction::Window(action),
                ..
            }) => ViewerClick::Act(*action),
            _ if contains(&painted.rect, (x, y)) => ViewerClick::Swallow,
            _ => ViewerClick::Close,
        }
    } else {
        ViewerClick::None
    };
    ViewerInput { click, scroll }
}

/// Window state that isn't the worker's: what the overlay needs.
struct View<'a> {
    copied: bool,
    menu_open: bool,
    record_failed: bool,
    hover: Option<(f32, f32)>,
    viewer: Option<&'a mut Viewer>,
}

struct Composed {
    commands: Vec<DisplayCommand>,
    /// Panel buttons (question cards, Resume, Copy setup).
    buttons: Vec<overlay::Button>,
    /// The open ⋮ menu's items and bounds.
    menu: Option<(Vec<overlay::Button>, Rect)>,
}

/// One observation-window frame: the page for `input` (human input), the
/// id badges and target outline, the agent panel and the ⋮ menu.
fn compose(
    worker: &mut Worker,
    input: &WindowInput,
    window_width: usize,
    window_height: usize,
    view: &mut View<'_>,
) -> Composed {
    let (page_width, panel_width) = overlay::layout(window_width);
    let page_height = window_height.saturating_sub(CHROME_HEIGHT as usize);
    worker.browser().set_viewport(page_width, page_height);
    // The chrome of an agent window (board 2): a pill saying who drives,
    // the agent dot on the tab, ⋮ pressed while its menu is open.
    let pill = agent_pill(worker);
    let recording = worker.recording();
    let mut pills = vec![pill];
    if let Some(recording) = &recording {
        pills.push(overlay::Pill::Recording(
            recording.since.elapsed().as_secs(),
        ));
    }
    worker.browser().state_mut().chrome_extras = ChromeExtras {
        accessory_width: overlay::pills_width(&pills),
        tab_marker: (pill != overlay::Pill::NoAgent).then_some(palette::AGENT),
        menu_open: view.menu_open,
    };
    let mut commands = worker.browser().human_frame(input);
    commands.extend(overlay::pills(&pills, page_width as f32));
    let scroll_y = worker.browser().state().scroll_offset;
    let target = worker.browser().last_target();
    let mut elements = worker.browser().element_boxes();
    if worker.paused() {
        // The page is the person's while paused: no id badges. A field
        // handed to them (a card still up) keeps its outline.
        elements.retain(|e| worker.waiting().is_some() && Some(e.id) == target);
    }
    // What the outlined target is waiting on (board 3).
    let note = match worker.waiting() {
        Some(worker::Waiting::Approval { .. }) => Some("The agent wants to press this"),
        Some(worker::Waiting::Handoff { .. }) => Some("Type the password here"),
        None => None,
    };
    let page = overlay::page_area(page_width, window_height);
    let ink = overlay::text_obstacles(&commands, page);
    commands.extend(overlay::page_overlay(
        &elements, target, note, scroll_y, page, &ink,
    ));
    let mut buttons = Vec::new();
    if panel_width > 0 {
        let agent = &worker.options().agent;
        let (panel_commands, panel_buttons) = overlay::panel(
            &overlay::PanelInfo {
                approve: agent.approve,
                paused: worker.paused().then(|| worker.held_call()),
                allow_origins: &agent.allow_origins,
                timeline: worker.timeline(),
                waiting: worker.waiting(),
                waited: worker
                    .waiting_since()
                    .map_or(Duration::ZERO, |since| since.elapsed()),
                page_wait: worker.page_wait().map(|(condition, left)| {
                    let what = match condition {
                        mb_agent::WaitCondition::Text(text) => format!("\"{text}\""),
                        mb_agent::WaitCondition::Element(id) => format!("e{}", id.raw()),
                    };
                    (what, left, worker.page_wait_total().unwrap_or(left))
                }),
                attach: worker.options().attach.as_ref(),
                copied: view.copied,
                recording: recording.as_ref().map(|r| overlay::RecordingCard {
                    file: r.file.as_ref().unwrap_or(&r.dir).display().to_string(),
                    created: r.file.is_some(),
                    steps: r.steps,
                }),
            },
            page_width as f32,
            panel_width as f32,
            window_height as f32,
        );
        commands.extend(panel_commands);
        buttons = panel_buttons;
    }
    let mut menu = None;
    if view.menu_open {
        let painted = overlay::menu(
            &overlay::MenuInfo {
                paused: worker.paused(),
                approve: worker.approve_mode(),
                allow_origins: &worker.options().agent.allow_origins,
                recording: recording.as_ref().map(|r| r.since.elapsed()),
                record_failed: view.record_failed,
                hover: view.hover,
            },
            page_width as f32,
        );
        commands.extend(painted.commands);
        menu = Some((painted.buttons, painted.rect));
    }
    if let Some(viewer) = view.viewer.as_deref_mut() {
        commands.extend(paint_viewer(worker, viewer, page, view.hover));
    }
    Composed {
        commands,
        buttons,
        menu,
    }
}

// "What the agent sees": the blocks for the chosen scope, painted over the
// page. Whole page is a fresh full snapshot — `full_snapshot`, never
// `observe`: it must not issue ids (they'd become valid for the agent
// without it having seen them) nor drain console / dialog records the
// agent's next result should carry.
fn paint_viewer(
    worker: &mut Worker,
    viewer: &mut Viewer,
    page: Rect,
    hover: Option<(f32, f32)>,
) -> Vec<DisplayCommand> {
    let (shown, source) = match viewer.scope {
        overlay::ViewScope::OnScreen => match worker.seen() {
            Some(seen) => (
                Some((seen.browser.clone(), seen.page.clone())),
                format!(
                    "Exactly the text the agent's last result carried ({}).",
                    seen.from
                ),
            ),
            None => (None, "The agent hasn't received a page yet.".to_string()),
        },
        overlay::ViewScope::WholePage => {
            let key: WholeKey = (
                worker.browser().state().document_generation(),
                worker.seen().cloned(),
            );
            let fresh = viewer.whole.as_ref().is_none_or(|(_, k)| *k != key);
            if fresh {
                let blocks = worker.browser().full_snapshot().to_blocks();
                viewer.whole = Some((blocks, key));
            }
            (
                viewer.whole.as_ref().map(|(blocks, _)| blocks.clone()),
                "The whole page as an observation, taken now — not sent to the agent. Ids it hasn't been shown aren't valid for it yet."
                    .to_string(),
            )
        }
    };
    let painted = overlay::agent_view(
        &overlay::AgentViewInfo {
            scope: viewer.scope,
            blocks: shown.as_ref().map(|(b, p)| (b.as_str(), p.as_str())),
            source: &source,
            scroll: viewer.scroll,
            copied: viewer.copied,
            hover,
        },
        page,
    );
    viewer.scroll = painted.scroll;
    viewer.shown = shown;
    viewer.painted = Some(ViewerPainted {
        buttons: painted.buttons,
        rect: painted.rect,
        page_rows: painted.page_rows,
    });
    painted.commands
}

// Who drives, for the chrome pill. `--http` before any call: nobody yet
// (the HTTP server doesn't tell us about sessions; the first call does).
fn agent_pill(worker: &Worker) -> overlay::Pill {
    if worker.paused() {
        overlay::Pill::Paused
    } else if worker.options().attach.is_some()
        && worker.timeline().is_empty()
        && worker.waiting().is_none()
        && worker.page_wait().is_none()
    {
        overlay::Pill::NoAgent
    } else {
        overlay::Pill::Attached(worker.approve_mode())
    }
}

// The setup command holds the token: it goes to the clipboard, never to
// the screen or the log.
fn copy_to_clipboard(text: &str) -> bool {
    match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("[mb-mcp] clipboard unavailable: {error}");
            false
        }
    }
}

/// `--preview PATH`: render the frame the observation window would show
/// (after `--start-url` and any `--preview-click ID`s) to a PNG, without
/// opening a window. For checking the overlay and for docs.
pub fn preview(options: Options, path: &std::path::Path, clicks: &[String]) -> Result<(), String> {
    let width = options.agent.viewport_width + overlay::PANEL_WIDTH;
    let height = options.agent.viewport_height + CHROME_HEIGHT as usize;
    let scale = options.preview_scale;
    let (pause, menu_open) = (options.preview_pause, options.preview_menu);
    let mut viewer = options.preview_agent_view.map(|scope| Viewer {
        scope: match scope {
            crate::cli::PreviewView::OnScreen => overlay::ViewScope::OnScreen,
            crate::cli::PreviewView::WholePage => overlay::ViewScope::WholePage,
        },
        ..Viewer::default()
    });
    let mut worker = Worker::new(options);
    if !clicks.is_empty() {
        // Ids are only accepted once an observation listed them.
        worker.process(&worker::Request::Observe {
            full: true,
            cursor: 0,
        });
    }
    if pause {
        // Clicks after this hold, as they would for a paused agent.
        worker.pause();
    }
    // Replies the preview never reads; kept so held calls stay attached.
    let mut pending = Vec::new();
    for id in clicks {
        // Through `submit`, as in the window: an action that needs the
        // person parks, and the preview shows its card.
        let (reply, receiver) = tokio::sync::oneshot::channel();
        pending.push(receiver);
        worker.submit(Job {
            request: worker::Request::Click { id: id.clone() },
            reply,
        });
    }
    // Two frames: the first lays out at the window size, the second paints
    // with the hover/focus state that settled.
    let input = WindowInput::default();
    let mut view = View {
        copied: false,
        menu_open,
        record_failed: false,
        hover: None,
        viewer: viewer.as_mut(),
    };
    let _ = compose(&mut worker, &input, width, height, &mut view);
    let commands = compose(&mut worker, &input, width, height, &mut view).commands;
    // Same path as a HiDPI window: lay out in CSS px, paint scaled.
    let commands = scale_display_list(&commands, scale);
    let (pw, ph) = (
        (width as f32 * scale).round() as usize,
        (height as f32 * scale).round() as usize,
    );
    let pixels = rasterize(&commands, pw, ph);
    let png = worker::encode_rgb_png(&pixels, pw, ph);
    std::fs::write(path, png).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click_at(x: f32, y: f32) -> WindowInput {
        WindowInput {
            mouse_position: Some((x, y)),
            left_mouse_pressed: true,
            left_mouse_held: true,
            ..WindowInput::default()
        }
    }

    fn painted(page_width: f32) -> (Vec<overlay::Button>, Rect) {
        let menu = overlay::menu(
            &overlay::MenuInfo {
                paused: false,
                approve: mb_agent::ApproveMode::Ask,
                allow_origins: &[],
                recording: None,
                record_failed: false,
                hover: None,
            },
            page_width,
        );
        (menu.buttons, menu.rect)
    }

    #[test]
    fn menu_clicks_never_reach_the_page() {
        let width = 900.0;
        let button = menu_button_rect(width);
        let on_button = click_at(button.x + 4.0, button.y + 4.0);
        // Closed: the ⋮ button opens it, other clicks are the page's.
        let closed = route_menu_input(false, None, width, &on_button);
        assert_eq!(closed.click, MenuClick::Toggle);
        let page = route_menu_input(false, None, width, &click_at(200.0, 300.0));
        assert_eq!(page.click, MenuClick::None);

        // Open: an item is picked, padding is swallowed, outside closes.
        let menu = painted(width);
        let item = &menu.0[0];
        let picked = route_menu_input(
            true,
            Some(&menu),
            width,
            &click_at(item.rect.x + 2.0, item.rect.y + 2.0),
        );
        assert_eq!(
            picked.click,
            MenuClick::Pick(overlay::PanelAction::Control(overlay::Control::Pause))
        );
        assert!(picked.over_menu);
        let padding = route_menu_input(
            true,
            Some(&menu),
            width,
            &click_at(menu.1.x + 2.0, menu.1.y + 2.0),
        );
        assert_eq!(padding.click, MenuClick::Swallow);
        let outside = route_menu_input(true, Some(&menu), width, &click_at(100.0, 500.0));
        assert_eq!(outside.click, MenuClick::Close);
        assert!(!outside.over_menu);
        assert_eq!(
            route_menu_input(true, Some(&menu), width, &on_button).click,
            MenuClick::Toggle
        );
        // Hovering the open menu hides the mouse from the page.
        let hover = WindowInput {
            mouse_position: Some((item.rect.x + 2.0, item.rect.y + 2.0)),
            ..WindowInput::default()
        };
        let hovered = route_menu_input(true, Some(&menu), width, &hover);
        assert_eq!(hovered.click, MenuClick::None);
        assert!(hovered.over_menu);
    }

    #[test]
    fn escape_closes_the_menu_and_the_view_item_opens_the_viewer() {
        let width = 900.0;
        let menu = painted(width);
        let escape = WindowInput {
            escape_pressed: true,
            ..WindowInput::default()
        };
        assert_eq!(
            route_menu_input(true, Some(&menu), width, &escape).click,
            MenuClick::Escape
        );
        // Closed: Escape is the page's.
        assert_eq!(
            route_menu_input(false, None, width, &escape).click,
            MenuClick::None
        );
        let view = menu
            .0
            .iter()
            .find(|b| {
                b.action == overlay::PanelAction::Window(overlay::WindowAction::ShowAgentView)
            })
            .unwrap();
        let picked = route_menu_input(
            true,
            Some(&menu),
            width,
            &click_at(view.rect.x + 2.0, view.rect.y + 2.0),
        );
        assert_eq!(picked.click, MenuClick::Pick(view.action));
    }

    fn painted_viewer() -> ViewerPainted {
        let view = overlay::agent_view(
            &overlay::AgentViewInfo {
                scope: overlay::ViewScope::OnScreen,
                blocks: Some((
                    "# browser (trusted)\n",
                    "# page content (untrusted, origin=x)\n",
                )),
                source: "",
                scroll: 0,
                copied: false,
                hover: None,
            },
            overlay::page_area(900, 800),
        );
        ViewerPainted {
            buttons: view.buttons,
            rect: view.rect,
            page_rows: view.page_rows,
        }
    }

    #[test]
    fn the_viewer_is_modal_and_closes_by_escape_button_or_outside() {
        let viewer = painted_viewer();
        let route = |input: &WindowInput| route_viewer_input(Some(&viewer), input);
        let at = |action| {
            let b = viewer.buttons.iter().find(|b| b.action == action).unwrap();
            click_at(b.rect.x + 2.0, b.rect.y + 2.0)
        };
        use overlay::{PanelAction::Window, ViewScope, WindowAction};
        let close = Window(WindowAction::CloseAgentView);
        assert_eq!(
            route(&at(close)).click,
            ViewerClick::Act(WindowAction::CloseAgentView)
        );
        let whole = Window(WindowAction::Scope(ViewScope::WholePage));
        assert_eq!(
            route(&at(whole)).click,
            ViewerClick::Act(WindowAction::Scope(ViewScope::WholePage))
        );
        // Inside but not on a button: nothing, and it goes nowhere else.
        let inside = click_at(
            viewer.rect.x + 30.0,
            viewer.rect.y + viewer.rect.height - 60.0,
        );
        assert_eq!(route(&inside).click, ViewerClick::Swallow);
        // Outside (page or panel): closes.
        assert_eq!(route(&click_at(4.0, 120.0)).click, ViewerClick::Close);
        assert_eq!(route(&click_at(1000.0, 400.0)).click, ViewerClick::Close);
        let escape = WindowInput {
            escape_pressed: true,
            ..WindowInput::default()
        };
        assert_eq!(route(&escape).click, ViewerClick::Close);
        // Wheel down (negative = down) / page down scroll the text.
        let wheel = WindowInput {
            scroll_y: -3.0,
            ..WindowInput::default()
        };
        assert_eq!(route(&wheel).scroll, 4);
        let page_down = WindowInput {
            page_down_pressed: true,
            ..WindowInput::default()
        };
        assert_eq!(route(&page_down).scroll, viewer.page_rows as isize);
        // The frame it opened on (nothing painted yet): Escape still works.
        assert_eq!(route_viewer_input(None, &escape).click, ViewerClick::Close);
    }

    #[test]
    fn whole_page_shows_everything_but_issues_no_ids() {
        let base = crate::worker::tests::serve_page();
        let (mut worker, _, _) =
            crate::worker::tests::worker_on(&base, mb_agent::ApproveMode::Auto);
        let reply = worker.process(&worker::Request::Navigate {
            url: format!("{base}/long"),
        });
        let worker::Reply::Observation { browser, page } = reply else {
            panic!("navigate");
        };
        assert!(!page.contains("Far away"), "below the first screen");
        let navigated = (browser, page);
        let mut viewer = Viewer {
            scope: overlay::ViewScope::WholePage,
            ..Viewer::default()
        };
        paint_viewer(&mut worker, &mut viewer, overlay::page_area(800, 696), None);
        let (_, whole) = viewer.shown.clone().unwrap();
        let line = whole.lines().find(|l| l.contains("Far away")).unwrap();
        let id = line[1..line.find(']').unwrap()].to_string();
        // The agent was never shown it: still not a valid id for it.
        let reply = worker.process(&worker::Request::Click { id });
        let worker::Reply::Error(report) = reply else {
            panic!("an id the agent never saw must not work");
        };
        assert_eq!(report.code, "stale_element");
        assert!(report.observation.is_none());
        // On screen = exactly what the agent got last (the navigate; the
        // error carried no observation), not the snapshot.
        viewer.scope = overlay::ViewScope::OnScreen;
        paint_viewer(&mut worker, &mut viewer, overlay::page_area(800, 696), None);
        assert_eq!(viewer.shown, Some(navigated));
    }
}
