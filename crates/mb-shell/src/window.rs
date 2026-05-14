// Window glue: wraps winit's `EventLoop` and softbuffer's `Surface` behind the
// `WindowInput` snapshot the rest of the browser consumes. The closure passed to
// `run` returns a `Vec<u32>` of 0x00RRGGBB pixels; we copy it straight into the
// softbuffer presentation buffer (same layout as the legacy minifb backend).
//
// Input model: winit is event-driven, so we accumulate edge presses (Enter,
// PageUp, ...) and level state (mouse held, modifiers) onto `PendingInput`.
// On `RedrawRequested` we drain it into a `WindowInput` for that frame, which
// rebuilds `left_mouse_pressed` (the up→down edge) from the stored level.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use softbuffer::{Context as SbContext, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

// `WindowInput` lives in `mb-engine::input` so the engine and runtime can
// speak the shape without depending on winit. Local code paints / reads it
// through that name.
use mb_engine::input::WindowInput;

/// Cross-thread wake-up handle. Workers (currently just the navigation
/// loader) call `wake()` after they finish so the shell promptly
/// schedules a redraw — that's what lets `wants_continuous_redraw`
/// drop the `pending_navigation.is_some()` arm and idle at 0% CPU
/// while a fetch is in flight. `as_arc` packages the proxy into the
/// `Arc<dyn Fn() + Send + Sync>` shape `BrowserState` stores.
#[derive(Clone)]
pub struct WakeHandle {
    proxy: EventLoopProxy<()>,
}

impl WakeHandle {
    /// Package the proxy into the `Arc<dyn Fn() + Send + Sync>` shape
    /// `BrowserState` stores so it can hand the hook to worker
    /// threads. Send may fail if the event loop has already exited
    /// (e.g. user closed the window mid-fetch); the closure drops the
    /// error since there is nothing left to redraw.
    pub fn as_arc(&self) -> Arc<dyn Fn() + Send + Sync> {
        let proxy = self.proxy.clone();
        Arc::new(move || {
            let _ = proxy.send_event(());
        })
    }
}

/// The per-frame closure paints directly into the softbuffer surface
/// (`target`) and returns whether the browser wants another frame even
/// without input: `true` schedules another `request_redraw` from
/// `about_to_wait`, `false` lets winit block on the next real input
/// event and drops idle CPU to ~0%. The `wake` handle is the same on
/// every invocation; callers typically register it with their state
/// on the first frame so background workers can poke the shell when
/// they finish.
pub fn run<F>(
    title: &str,
    initial_width: usize,
    initial_height: usize,
    build_scene: F,
) -> Result<()>
where
    F: FnMut(usize, usize, &WindowInput, &mut [u32], &WakeHandle) -> bool,
{
    let event_loop = EventLoop::new().context("create event loop")?;
    // `Wait` lets winit block on the next real event when no animation
    // is pending. The shell explicitly schedules redraws via
    // `request_redraw` for input events (keyboard / mouse / scroll /
    // resize), for time-driven UI (caret blink, JS timers) when the
    // closure asks for one — see `App::about_to_wait` — and via the
    // `UserEvent` arm below when a background worker pokes the proxy.
    event_loop.set_control_flow(ControlFlow::Wait);
    let wake = WakeHandle {
        proxy: event_loop.create_proxy(),
    };
    let mut app = App::new(
        title.to_string(),
        initial_width as u32,
        initial_height as u32,
        build_scene,
        wake,
    );
    event_loop.run_app(&mut app).context("run event loop")?;
    if let Some(err) = app.error.take() {
        return Err(err);
    }
    Ok(())
}

struct App<F>
where
    F: FnMut(usize, usize, &WindowInput, &mut [u32], &WakeHandle) -> bool,
{
    title: String,
    initial_size: (u32, u32),
    build_scene: F,
    wake: WakeHandle,
    window: Option<Rc<Window>>,
    surface: Option<Surface<Rc<Window>, Rc<Window>>>,
    pending: PendingInput,
    last_left_down: bool,
    last_wants_redraw: bool,
    error: Option<anyhow::Error>,
}

#[derive(Default)]
struct PendingInput {
    typed: String,
    enter_pressed: bool,
    backspace_pressed: bool,
    focus_address_bar: bool,
    back_pressed: bool,
    forward_pressed: bool,
    scroll_y: f32,
    move_up: bool,
    move_down: bool,
    page_up_pressed: bool,
    page_down_pressed: bool,
    mouse_position: Option<(f32, f32)>,
    left_held: bool,
    modifiers: ModifiersState,
}

impl PendingInput {
    fn drain_for_frame(&mut self, last_left_down: bool) -> WindowInput {
        WindowInput {
            typed: std::mem::take(&mut self.typed),
            enter_pressed: std::mem::take(&mut self.enter_pressed),
            backspace_pressed: std::mem::take(&mut self.backspace_pressed),
            focus_address_bar: std::mem::take(&mut self.focus_address_bar),
            back_pressed: std::mem::take(&mut self.back_pressed),
            forward_pressed: std::mem::take(&mut self.forward_pressed),
            scroll_y: std::mem::take(&mut self.scroll_y),
            move_up: std::mem::take(&mut self.move_up),
            move_down: std::mem::take(&mut self.move_down),
            page_up_pressed: std::mem::take(&mut self.page_up_pressed),
            page_down_pressed: std::mem::take(&mut self.page_down_pressed),
            mouse_position: self.mouse_position,
            left_mouse_pressed: self.left_held && !last_left_down,
            left_mouse_held: self.left_held,
        }
    }
}

impl<F> App<F>
where
    F: FnMut(usize, usize, &WindowInput, &mut [u32], &WakeHandle) -> bool,
{
    fn new(title: String, w: u32, h: u32, build_scene: F, wake: WakeHandle) -> Self {
        Self {
            title,
            initial_size: (w, h),
            build_scene,
            wake,
            window: None,
            surface: None,
            pending: PendingInput::default(),
            last_left_down: false,
            // The first frame is unconditionally requested from `resumed`,
            // and that frame's closure reports back what to do next.
            last_wants_redraw: false,
            error: None,
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, err: anyhow::Error) {
        self.error = Some(err);
        event_loop.exit();
    }

    fn request_redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

impl<F> ApplicationHandler for App<F>
where
    F: FnMut(usize, usize, &WindowInput, &mut [u32], &WakeHandle) -> bool,
{
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(LogicalSize::new(self.initial_size.0, self.initial_size.1));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => return self.fail(event_loop, anyhow::Error::new(e).context("create window")),
        };
        let context = match SbContext::new(window.clone()) {
            Ok(c) => c,
            Err(e) => return self.fail(event_loop, anyhow!("softbuffer context: {e}")),
        };
        let surface = match Surface::new(&context, window.clone()) {
            Ok(s) => s,
            Err(e) => return self.fail(event_loop, anyhow!("softbuffer surface: {e}")),
        };
        window.request_redraw();
        self.window = Some(window);
        self.surface = Some(surface);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        // Every input branch that updates `pending` also schedules a
        // redraw — without this the shell sits on `ControlFlow::Wait`
        // forever and the user's keystroke / click never reaches a
        // frame. `ModifiersChanged` is the lone exception: it just
        // tracks the chord state for the next keyboard event and has
        // no visible effect on its own.
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(mods) => {
                self.pending.modifiers = mods.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.handle_keyboard(event_loop, event);
                self.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pending.mouse_position = Some((position.x as f32, position.y as f32));
                self.request_redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                self.pending.mouse_position = None;
                self.request_redraw();
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.pending.left_held = state == ElementState::Pressed;
                self.request_redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    // PixelDelta arrives from precise input devices (trackpads).
                    // 20 px per "line" roughly matches what minifb returned.
                    MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 20.0,
                };
                self.pending.scroll_y += dy;
                self.request_redraw();
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if let Err(err) = self.redraw() {
                    self.fail(event_loop, err);
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        // A background worker (currently just the navigation loader)
        // pinged the proxy from off-thread because something it owns
        // is now ready to be picked up by the next frame. We don't
        // care which worker — just schedule one redraw so the closure
        // runs and drains whatever channel it owns.
        self.request_redraw();
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Drop into `ControlFlow::Wait` (set in `run`) unless the last
        // frame told us it was animating — caret blink while the
        // address bar is focused, a live JS timer / rAF, etc. Every
        // other case waits for an actual input event (or a worker
        // `user_event` wake) and burns no CPU on idle.
        if self.last_wants_redraw {
            self.request_redraw();
        }
    }
}

impl<F> App<F>
where
    F: FnMut(usize, usize, &WindowInput, &mut [u32], &WakeHandle) -> bool,
{
    fn handle_keyboard(&mut self, event_loop: &ActiveEventLoop, event: KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let mods = self.pending.modifiers;
        let cmd_or_ctrl = mods.super_key() || mods.control_key();
        let alt = mods.alt_key();
        let repeat = event.repeat;

        match &event.logical_key {
            Key::Named(NamedKey::Escape) if !repeat => {
                event_loop.exit();
                return;
            }
            Key::Named(NamedKey::Enter) if !repeat => self.pending.enter_pressed = true,
            // Backspace: minifb used `KeyRepeat::Yes`, so we accept repeats too.
            Key::Named(NamedKey::Backspace) => self.pending.backspace_pressed = true,
            Key::Named(NamedKey::ArrowUp) => self.pending.move_up = true,
            Key::Named(NamedKey::ArrowDown) => self.pending.move_down = true,
            Key::Named(NamedKey::ArrowLeft) if alt && !repeat => {
                self.pending.back_pressed = true;
            }
            Key::Named(NamedKey::ArrowRight) if alt && !repeat => {
                self.pending.forward_pressed = true;
            }
            Key::Named(NamedKey::PageUp) if !repeat => self.pending.page_up_pressed = true,
            Key::Named(NamedKey::PageDown) if !repeat => self.pending.page_down_pressed = true,
            Key::Character(c) if cmd_or_ctrl && !repeat => match c.as_str() {
                s if s.eq_ignore_ascii_case("l") => self.pending.focus_address_bar = true,
                "[" => self.pending.back_pressed = true,
                "]" => self.pending.forward_pressed = true,
                _ => {}
            },
            _ => {}
        }

        // Typed-char accumulation mirrors minifb's `InputCallback::add_char`:
        // only printable Unicode, and never while a chord modifier is engaged.
        if !cmd_or_ctrl
            && !alt
            && let Some(text) = event.text.as_ref()
        {
            for ch in text.chars() {
                if !ch.is_control() {
                    self.pending.typed.push(ch);
                }
            }
        }
    }

    fn redraw(&mut self) -> Result<()> {
        let window = self.window.as_ref().ok_or_else(|| anyhow!("no window"))?;
        let surface = self
            .surface
            .as_mut()
            .ok_or_else(|| anyhow!("no surface"))?;
        let size = window.inner_size();
        let (Some(width_nz), Some(height_nz)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            // Window minimised or zero-sized: skip this frame.
            return Ok(());
        };
        surface
            .resize(width_nz, height_nz)
            .map_err(|e| anyhow!("softbuffer resize: {e}"))?;

        let input = self.pending.drain_for_frame(self.last_left_down);
        self.last_left_down = input.left_mouse_held;

        let mut buffer = surface
            .buffer_mut()
            .map_err(|e| anyhow!("softbuffer buffer: {e}"))?;
        // softbuffer's `resize` above guarantees `buffer.len() == width *
        // height`, and the renderer paints every pixel inside that
        // rectangle starting from an opaque-white pixmap. No prior-frame
        // residue can leak through, so no explicit zero pass is needed.
        let wants_redraw = (self.build_scene)(
            size.width as usize,
            size.height as usize,
            &input,
            &mut buffer,
            &self.wake,
        );
        self.last_wants_redraw = wants_redraw;
        buffer
            .present()
            .map_err(|e| anyhow!("softbuffer present: {e}"))?;
        Ok(())
    }
}
