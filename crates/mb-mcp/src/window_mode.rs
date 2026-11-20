// `mb-mcp --window`: the observation window (docs/concept.md G3).
//
// winit must own the main thread (macOS), so here the browser lives inside
// the window loop instead of `worker::run`. Each frame:
//   1. run every agent job that arrived (the MCP thread wakes the loop
//      after queueing one),
//   2. render the page for the person at the window (their input is
//      ordinary human input — the policy gate never judges it),
//   3. paint the overlay (id badges, target outline) and the agent panel.
// When the window closes, remaining and future tool calls get
// `browser_closed`. When the MCP client goes away, the process exits.
// With `--http` (G3b) the person owns the process instead: clients come
// and go, and closing the window ends it.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, OnceLock};

use mb_runtime::chrome::CHROME_HEIGHT;
use mb_runtime::input::WindowInput;
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
            if let Some((x, y)) = input.mouse_position
                && x >= page_width as f32
            {
                if input.left_mouse_pressed
                    && let Some(button) = buttons.iter().find(|b| b.contains(x, y))
                {
                    match button.action {
                        overlay::PanelAction::Answer(answer) => worker.answer(answer),
                        overlay::PanelAction::Replay(_) => {}
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
            let (commands, card_buttons) = compose(
                &mut worker,
                &page_input,
                window_width,
                window_height,
                copied,
            );
            buttons = card_buttons;
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

/// One observation-window frame: the page for `input` (human input), the
/// id badges and target outline, and the agent panel.
fn compose(
    worker: &mut Worker,
    input: &WindowInput,
    window_width: usize,
    window_height: usize,
    copied: bool,
) -> (Vec<DisplayCommand>, Vec<overlay::Button>) {
    let (page_width, panel_width) = overlay::layout(window_width);
    let page_height = window_height.saturating_sub(CHROME_HEIGHT as usize);
    worker.browser().set_viewport(page_width, page_height);
    let mut commands = worker.browser().human_frame(input);
    let scroll_y = worker.browser().state().scroll_offset;
    let elements = worker.browser().element_boxes();
    let target = worker.browser().last_target();
    commands.extend(overlay::page_overlay(
        &elements,
        target,
        scroll_y,
        window_height as f32,
    ));
    let mut buttons = Vec::new();
    if panel_width > 0 {
        let agent = &worker.options().agent;
        let (panel_commands, panel_buttons) = overlay::panel(
            &overlay::PanelInfo {
                approve: agent.approve,
                allow_origins: &agent.allow_origins,
                timeline: worker.timeline(),
                waiting: worker.waiting(),
                page_wait: worker.page_wait().map(|(condition, left)| {
                    let what = match condition {
                        mb_agent::WaitCondition::Text(text) => format!("\"{text}\""),
                        mb_agent::WaitCondition::Element(id) => format!("e{}", id.raw()),
                    };
                    (what, left.as_secs())
                }),
                attach: worker.options().attach.as_ref(),
                copied,
            },
            page_width as f32,
            panel_width as f32,
            window_height as f32,
        );
        commands.extend(panel_commands);
        buttons = panel_buttons;
    }
    (commands, buttons)
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
    let mut worker = Worker::new(options);
    if !clicks.is_empty() {
        // Ids are only accepted once an observation listed them.
        worker.process(&worker::Request::Observe {
            full: true,
            cursor: 0,
        });
    }
    for id in clicks {
        // Through `submit`, as in the window: an action that needs the
        // person parks, and the preview shows its card.
        let (reply, _pending) = tokio::sync::oneshot::channel();
        worker.submit(Job {
            request: worker::Request::Click { id: id.clone() },
            reply,
        });
    }
    // Two frames: the first lays out at the window size, the second paints
    // with the hover/focus state that settled.
    let input = WindowInput::default();
    let _ = compose(&mut worker, &input, width, height, false);
    let (commands, _) = compose(&mut worker, &input, width, height, false);
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
