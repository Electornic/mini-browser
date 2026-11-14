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

            // 1. Agent jobs. Each runs to completion (action + settle) before
            //    the frame is painted.
            loop {
                match jobs.try_recv() {
                    Ok(job) => {
                        let reply = worker.process(&job.request);
                        let _ = job.reply.send(reply);
                    }
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

            // 2 + 3. The page as the person drives it, with what the agent
            //        sees drawn over it.
            let commands = compose(&mut worker, input, window_width, window_height);
            frame.present(&commands);
            worker.browser().state().wants_continuous_redraw()
        },
    );
    if let Err(error) = result {
        eprintln!("[mb-mcp] window error: {error}");
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
) -> Vec<DisplayCommand> {
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
    if panel_width > 0 {
        let agent = &worker.options().agent;
        commands.extend(overlay::panel(
            &overlay::PanelInfo {
                approve: agent.approve,
                allow_origins: &agent.allow_origins,
                timeline: worker.timeline(),
            },
            page_width as f32,
            panel_width as f32,
            window_height as f32,
        ));
    }
    commands
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
        worker.process(&worker::Request::Click { id: id.clone() });
    }
    // Two frames: the first lays out at the window size, the second paints
    // with the hover/focus state that settled.
    let input = WindowInput::default();
    let _ = compose(&mut worker, &input, width, height);
    let commands = compose(&mut worker, &input, width, height);
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
