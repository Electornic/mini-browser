// `mb-mcp replay FILE --window` (docs/concept.md "재생 뷰 (G5b)"): step
// through a replay in a window. The page is the replaying browser itself
// (read-only: only scrolling reaches it, so the comparison stays honest),
// the next step's target is outlined, and the panel shows each step's
// outcome and, for the last one run, recorded vs now.
//
// Back re-runs from the start up to the previous step: the replay is
// deterministic (virtual clock, same inputs), so that's the same state.

use std::process::ExitCode;

use mb_runtime::chrome::{CHROME_HEIGHT, ChromeExtras};
use mb_runtime::dom::NodeId;
use mb_runtime::input::WindowInput;
use mb_runtime::render::{DisplayCommand, rasterize, scale_display_list};

use crate::cli::ReplayOptions;
use crate::overlay::{self, PanelAction, ReplayControl, ReplayPanelInfo, StepMark};
use crate::replay::{NEEDS_PERSON, Replayer, SAME, StepOutcome};

// Result-card text width in characters (PANEL_WIDTH minus the panel's and
// the card's padding, ~7 px each).
const PANEL_CHARS: usize = (overlay::PANEL_WIDTH - 72) / 7;

#[derive(Default)]
struct View {
    // The next step's target, for the step it was computed at.
    target: Option<(usize, Option<NodeId>)>,
    buttons: Vec<overlay::Button>,
}

pub fn run(options: &ReplayOptions) -> ExitCode {
    let mut replayer = match Replayer::open(options) {
        Ok(replayer) => replayer,
        Err(stop) => {
            eprintln!("{}", stop.message);
            return ExitCode::from(stop.code);
        }
    };
    if let Some(path) = &options.preview {
        return match preview(
            &mut replayer,
            path,
            options.preview_steps,
            options.preview_scale,
        ) {
            Ok(()) => {
                eprintln!("[mb-mcp] wrote {}", path.display());
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("mb-mcp: {message}");
                ExitCode::FAILURE
            }
        };
    }

    let (page_width, page_height) = replayer.viewport();
    let mut view = View::default();
    let result = mb_shell::window::run(
        &format!("mini-browser — replay {}", replayer.file_name()),
        page_width + overlay::PANEL_WIDTH,
        page_height + CHROME_HEIGHT as usize,
        |frame, input, _wake| {
            if input.left_mouse_pressed
                && let Some((x, y)) = input.mouse_position
                && let Some(button) = view.buttons.iter().find(|b| b.contains(x, y))
                && let PanelAction::Replay(control) = button.action
            {
                apply(&mut replayer, control);
                view.target = None;
            }
            if input.enter_pressed {
                apply(&mut replayer, ReplayControl::Next);
                view.target = None;
            }
            let (commands, buttons) =
                compose(&mut replayer, &mut view, &read_only(input), frame.height);
            view.buttons = buttons;
            frame.present(&commands);
            false
        },
    );
    if let Err(error) = result {
        eprintln!("[mb-mcp] window error: {error}");
    }
    eprintln!("{}", replayer.summary());
    match replayer.outcomes().last() {
        Some(StepOutcome::Stopped(stop)) => ExitCode::from(stop.code),
        _ => ExitCode::from(SAME),
    }
}

fn apply(replayer: &mut Replayer, control: ReplayControl) {
    match control {
        ReplayControl::Next => {
            replayer.step();
        }
        ReplayControl::RunAll => while replayer.step().is_some() {},
        ReplayControl::Back => {
            let to = replayer.position().saturating_sub(1);
            replayer.go_to(to);
        }
        ReplayControl::Restart => replayer.restart(),
    }
}

// Only scrolling reaches the page: clicks, typing and hover could change
// it, and then it would no longer be comparable with the recording.
fn read_only(input: &WindowInput) -> WindowInput {
    WindowInput {
        scroll_y: input.scroll_y,
        page_up_pressed: input.page_up_pressed,
        page_down_pressed: input.page_down_pressed,
        ..WindowInput::default()
    }
}

fn compose(
    replayer: &mut Replayer,
    view: &mut View,
    input: &WindowInput,
    window_height: usize,
) -> (Vec<DisplayCommand>, Vec<overlay::Button>) {
    let position = replayer.position();
    let target = match view.target {
        Some((at, target)) if at == position => target,
        _ => {
            let target = replayer.next_target();
            view.target = Some((position, target));
            target
        }
    };
    let (page_width, _) = replayer.viewport();
    let browser = replayer.browser();
    // Board 5: the chrome says the page only replays.
    let pill = overlay::Pill::Replaying;
    browser.state_mut().chrome_extras = ChromeExtras {
        accessory_width: pill.width(),
        ..ChromeExtras::default()
    };
    let mut commands = browser.human_frame(input);
    commands.extend(pill.commands(page_width as f32));
    let scroll_y = browser.state().scroll_offset;
    let elements = browser.element_boxes();
    let page = overlay::page_area(page_width, window_height);
    let ink = overlay::text_obstacles(&commands, page);
    commands.extend(overlay::page_overlay(
        &elements, target, None, scroll_y, page, &ink,
    ));
    let info = panel_info(replayer, target.is_some());
    let (panel, buttons) = overlay::replay_panel(
        &info,
        page_width as f32,
        overlay::PANEL_WIDTH as f32,
        window_height as f32,
    );
    commands.extend(panel);
    (commands, buttons)
}

fn panel_info(replayer: &Replayer, target_outlined: bool) -> ReplayPanelInfo {
    let outcomes = replayer.outcomes();
    let finished = replayer.finished();
    let steps = (0..replayer.total())
        .map(|i| {
            let mark = match outcomes.get(i) {
                Some(StepOutcome::Matched(_)) => StepMark::Matched,
                Some(StepOutcome::Stopped(stop)) if stop.code == NEEDS_PERSON => {
                    StepMark::NeedsPerson
                }
                Some(StepOutcome::Stopped(_)) => StepMark::Differs,
                None if i == outcomes.len() && !finished => StepMark::Next,
                None => StepMark::Pending,
            };
            (mark, replayer.label(i))
        })
        .collect();
    let next = (!finished).then(|| (replayer.label(replayer.position()), target_outlined));
    let last = outcomes.last().map(|outcome| {
        let title = format!("step {}", outcomes.len());
        let lines = match outcome {
            StepOutcome::Matched(line) => {
                let mut lines = overlay::wrap(line, PANEL_CHARS, 3);
                lines.push("matches the recording".into());
                lines
            }
            StepOutcome::Stopped(stop) => stop
                .message
                .lines()
                .flat_map(|line| {
                    // Diff lines stay one per row (clipped) so -/+ colors hold.
                    if line.trim_start().starts_with(['-', '+']) {
                        vec![line.to_string()]
                    } else {
                        overlay::wrap(line, PANEL_CHARS, 4)
                    }
                })
                .collect(),
        };
        (title, lines)
    });
    ReplayPanelInfo {
        file: replayer.file_name(),
        position: replayer.position(),
        total: replayer.total(),
        matched: replayer.matched(),
        steps,
        next,
        last,
    }
}

/// `--preview PATH`: the window after `steps` steps, as a PNG.
fn preview(
    replayer: &mut Replayer,
    path: &std::path::Path,
    steps: usize,
    scale: f32,
) -> Result<(), String> {
    while replayer.position() < steps && replayer.step().is_some() {}
    let (page_width, page_height) = replayer.viewport();
    let (width, height) = (
        page_width + overlay::PANEL_WIDTH,
        page_height + CHROME_HEIGHT as usize,
    );
    let mut view = View::default();
    let input = WindowInput::default();
    let _ = compose(replayer, &mut view, &input, height);
    let (commands, _) = compose(replayer, &mut view, &input, height);
    let commands = scale_display_list(&commands, scale);
    let (pw, ph) = (
        (width as f32 * scale).round() as usize,
        (height as f32 * scale).round() as usize,
    );
    let pixels = rasterize(&commands, pw, ph);
    let png = crate::worker::encode_rgb_png(&pixels, pw, ph);
    std::fs::write(path, png).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_scrolling_reaches_the_page() {
        let input = WindowInput {
            typed: "x".into(),
            enter_pressed: true,
            mouse_position: Some((10.0, 200.0)),
            left_mouse_pressed: true,
            left_mouse_held: true,
            scroll_y: 40.0,
            page_down_pressed: true,
            ..WindowInput::default()
        };
        let page = read_only(&input);
        assert_eq!(page.scroll_y, 40.0);
        assert!(page.page_down_pressed);
        assert!(page.typed.is_empty() && !page.enter_pressed);
        assert!(page.mouse_position.is_none() && !page.left_mouse_pressed);
    }
}
