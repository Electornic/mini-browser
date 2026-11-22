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
// resume, approval mode. They are window input only: no MCP tool reaches
// them (docs/concept.md "⋮ 메뉴").
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
    // The parked-call deadline a wake-up is already scheduled for.
    let mut wake_scheduled: Option<std::time::Instant> = None;
    let attached = worker.options().attach.is_some();

    let result = mb_shell::window::run(
        "mini-browser — agent view",
        width,
        height,
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
            // The ⋮ menu takes clicks first: the button toggles it; while
            // it is open, a click picks an item or closes it, and never
            // reaches the page (or the panel) below.
            let routed = route_menu_input(menu_open, menu.as_ref(), page_width as f32, input);
            let consumed = routed.click != MenuClick::None;
            match routed.click {
                MenuClick::None | MenuClick::Swallow => {}
                MenuClick::Toggle => menu_open = !menu_open,
                MenuClick::Close => menu_open = false,
                MenuClick::Pick(control) => {
                    apply(&mut worker, control);
                    // The mode switch stays open to show the new
                    // selection; pause / resume is a command.
                    menu_open = matches!(control, overlay::Control::Approve(_));
                }
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
                        overlay::PanelAction::Replay(_) => {}
                        overlay::PanelAction::Control(control) => apply(&mut worker, control),
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
                &View {
                    copied,
                    menu_open,
                    hover: input.mouse_position,
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
    Pick(overlay::Control),
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
            Some(overlay::Button {
                action: overlay::PanelAction::Control(control),
                ..
            }) => MenuClick::Pick(*control),
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
// shows when the person took the wheel.
fn apply(worker: &mut Worker, control: overlay::Control) {
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
    }
}

/// Window state that isn't the worker's: what the overlay needs.
struct View {
    copied: bool,
    menu_open: bool,
    hover: Option<(f32, f32)>,
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
    view: &View,
) -> Composed {
    let (page_width, panel_width) = overlay::layout(window_width);
    let page_height = window_height.saturating_sub(CHROME_HEIGHT as usize);
    worker.browser().set_viewport(page_width, page_height);
    // The chrome of an agent window (board 2): a pill saying who drives,
    // the agent dot on the tab, ⋮ pressed while its menu is open.
    let pill = agent_pill(worker);
    worker.browser().state_mut().chrome_extras = ChromeExtras {
        accessory_width: pill.width(),
        tab_marker: (pill != overlay::Pill::NoAgent).then_some(palette::AGENT),
        menu_open: view.menu_open,
    };
    let mut commands = worker.browser().human_frame(input);
    commands.extend(pill.commands(page_width as f32));
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
                hover: view.hover,
            },
            page_width as f32,
        );
        commands.extend(painted.commands);
        menu = Some((painted.buttons, painted.rect));
    }
    Composed {
        commands,
        buttons,
        menu,
    }
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
    let view = View {
        copied: false,
        menu_open,
        hover: None,
    };
    let _ = compose(&mut worker, &input, width, height, &view);
    let commands = compose(&mut worker, &input, width, height, &view).commands;
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
        assert_eq!(picked.click, MenuClick::Pick(overlay::Control::Pause));
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
}
