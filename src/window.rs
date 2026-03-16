use std::{cell::RefCell, rc::Rc};

use minifb::{InputCallback, Key, KeyRepeat, Window, WindowOptions};

use crate::{render, render::DisplayCommand};

#[derive(Debug, Clone, Default)]
pub struct WindowInput {
    pub typed: String,
    pub enter_pressed: bool,
    pub backspace_pressed: bool,
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

    while window.is_open() && !window.is_key_down(Key::Escape) {
        let size = window.get_size();
        let input = WindowInput {
            typed: std::mem::take(&mut *typed_chars.borrow_mut()),
            enter_pressed: window.is_key_pressed(Key::Enter, KeyRepeat::No),
            backspace_pressed: window.is_key_pressed(Key::Backspace, KeyRepeat::Yes),
        };
        let commands = build_scene(size.0, size.1, &input);
        let buffer = render::rasterize(&commands, size.0, size.1);

        window.update_with_buffer(&buffer, size.0, size.1)?;
    }

    Ok(())
}
