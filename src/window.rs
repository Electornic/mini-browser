use std::{cell::RefCell, rc::Rc};

use minifb::{InputCallback, Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};

use crate::{render, render::DisplayCommand};

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
    pub left_mouse_pressed: bool,
}

struct TextCollector {
    chars: Rc<RefCell<String>>,
}

impl InputCallback for TextCollector {
    fn add_char(&mut self, uni_char: u32) {
        if let Some(ch) = char::from_u32(uni_char) {
            self.chars.borrow_mut().push(ch);
        }
    }
}

pub fn run<F>(
    title: &str,
    initial_width: usize,
    initial_height: usize,
    mut build_scene: F,
) -> Result<(), minifb::Error>
where
    F: FnMut(usize, usize, &WindowInput) -> Vec<DisplayCommand>,
{
    let mut window = Window::new(
        title,
        initial_width,
        initial_height,
        WindowOptions::default(),
    )?;
    let typed_chars = Rc::new(RefCell::new(String::new()));
    window.set_input_callback(Box::new(TextCollector {
        chars: Rc::clone(&typed_chars),
    }));
    window.set_target_fps(60);
    let mut last_left_down = false;

    // minifb pushes typed characters via callback, while special keys are polled each frame.
    while window.is_open() && !window.is_key_down(Key::Escape) {
        let size = window.get_size();
        let left_down = window.get_mouse_down(MouseButton::Left);
        let command_or_ctrl = window.is_key_down(Key::LeftSuper)
            || window.is_key_down(Key::RightSuper)
            || window.is_key_down(Key::LeftCtrl)
            || window.is_key_down(Key::RightCtrl);
        let alt = window.is_key_down(Key::LeftAlt) || window.is_key_down(Key::RightAlt);
        let input = WindowInput {
            typed: std::mem::take(&mut *typed_chars.borrow_mut()),
            enter_pressed: window.is_key_pressed(Key::Enter, KeyRepeat::No),
            backspace_pressed: window.is_key_pressed(Key::Backspace, KeyRepeat::Yes),
            focus_address_bar: command_or_ctrl && window.is_key_pressed(Key::L, KeyRepeat::No),
            back_pressed: (alt && window.is_key_pressed(Key::Left, KeyRepeat::No))
                || (command_or_ctrl && window.is_key_pressed(Key::LeftBracket, KeyRepeat::No)),
            forward_pressed: (alt && window.is_key_pressed(Key::Right, KeyRepeat::No))
                || (command_or_ctrl && window.is_key_pressed(Key::RightBracket, KeyRepeat::No)),
            scroll_y: window
                .get_scroll_wheel()
                .map(|scroll| scroll.1)
                .unwrap_or(0.0),
            move_up: window.is_key_pressed(Key::Up, KeyRepeat::Yes),
            move_down: window.is_key_pressed(Key::Down, KeyRepeat::Yes),
            page_up_pressed: window.is_key_pressed(Key::PageUp, KeyRepeat::No),
            page_down_pressed: window.is_key_pressed(Key::PageDown, KeyRepeat::No),
            mouse_position: window.get_mouse_pos(MouseMode::Clamp),
            left_mouse_pressed: left_down && !last_left_down,
        };
        let commands = build_scene(size.0, size.1, &input);
        let buffer = render::rasterize(&commands, size.0, size.1);

        window.update_with_buffer(&buffer, size.0, size.1)?;
        last_left_down = left_down;
    }

    Ok(())
}
