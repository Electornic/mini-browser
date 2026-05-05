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

use anyhow::{Context, Result, anyhow};
use softbuffer::{Context as SbContext, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

// WindowInput is the per-frame snapshot that the browser UI consumes.
#[derive(Debug, Clone, Default)]
pub struct WindowInput {
    pub typed: String,
    pub enter_pressed: bool,
    pub backspace_pressed: bool,
    pub focus_address_bar: bool,
    pub back_pressed: bool,
    pub forward_pressed: bool,
    pub scroll_y: f32,
    pub move_up: bool,
    pub move_down: bool,
    pub page_up_pressed: bool,
    pub page_down_pressed: bool,
    pub mouse_position: Option<(f32, f32)>,
    // `left_mouse_pressed` is the *edge* (true only on the frame the button
    // transitions from up to down). `left_mouse_held` is the *level*, which is
    // what `:active` needs.
    pub left_mouse_pressed: bool,
    pub left_mouse_held: bool,
}

pub fn run<F>(
    title: &str,
    initial_width: usize,
    initial_height: usize,
    build_scene: F,
) -> Result<()>
where
    F: FnMut(usize, usize, &WindowInput) -> Vec<u32>,
{
    let event_loop = EventLoop::new().context("create event loop")?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(
        title.to_string(),
        initial_width as u32,
        initial_height as u32,
        build_scene,
    );
    event_loop.run_app(&mut app).context("run event loop")?;
    if let Some(err) = app.error.take() {
        return Err(err);
    }
    Ok(())
}

struct App<F>
where
    F: FnMut(usize, usize, &WindowInput) -> Vec<u32>,
{
    title: String,
    initial_size: (u32, u32),
    build_scene: F,
    window: Option<Rc<Window>>,
    surface: Option<Surface<Rc<Window>, Rc<Window>>>,
    pending: PendingInput,
    last_left_down: bool,
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
    F: FnMut(usize, usize, &WindowInput) -> Vec<u32>,
{
    fn new(title: String, w: u32, h: u32, build_scene: F) -> Self {
        Self {
            title,
            initial_size: (w, h),
            build_scene,
            window: None,
            surface: None,
            pending: PendingInput::default(),
            last_left_down: false,
            error: None,
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, err: anyhow::Error) {
        self.error = Some(err);
        event_loop.exit();
    }
}

impl<F> ApplicationHandler for App<F>
where
    F: FnMut(usize, usize, &WindowInput) -> Vec<u32>,
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
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(mods) => {
                self.pending.modifiers = mods.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.handle_keyboard(event_loop, event);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pending.mouse_position = Some((position.x as f32, position.y as f32));
            }
            WindowEvent::CursorLeft { .. } => {
                self.pending.mouse_position = None;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.pending.left_held = state == ElementState::Pressed;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    // PixelDelta arrives from precise input devices (trackpads).
                    // 20 px per "line" roughly matches what minifb returned.
                    MouseScrollDelta::PixelDelta(p) => (p.y as f32) / 20.0,
                };
                self.pending.scroll_y += dy;
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(err) = self.redraw() {
                    self.fail(event_loop, err);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

impl<F> App<F>
where
    F: FnMut(usize, usize, &WindowInput) -> Vec<u32>,
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
        let pixels = (self.build_scene)(size.width as usize, size.height as usize, &input);
        self.last_left_down = input.left_mouse_held;

        let mut buffer = surface
            .buffer_mut()
            .map_err(|e| anyhow!("softbuffer buffer: {e}"))?;
        let n = buffer.len().min(pixels.len());
        buffer[..n].copy_from_slice(&pixels[..n]);
        // If the closure produced fewer pixels than the surface (e.g. mid-resize
        // race) zero the tail so we don't show stale junk.
        if n < buffer.len() {
            for slot in &mut buffer[n..] {
                *slot = 0;
            }
        }
        buffer
            .present()
            .map_err(|e| anyhow!("softbuffer present: {e}"))?;
        Ok(())
    }
}
