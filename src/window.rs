use minifb::{Key, Window, WindowOptions};

use crate::{render, render::DisplayCommand};

pub fn run<F>(
    title: &str,
    initial_width: usize,
    initial_height: usize,
    mut build_scene: F,
) -> Result<(), minifb::Error>
where
    F: FnMut(usize, usize) -> Vec<DisplayCommand>,
{
    let mut window = Window::new(
        title,
        initial_width,
        initial_height,
        WindowOptions::default(),
    )?;
    let mut last_size = (0, 0);
    let mut buffer = Vec::new();

    while window.is_open() && !window.is_key_down(Key::Escape) {
        let size = window.get_size();
        if size != last_size || buffer.is_empty() {
            let commands = build_scene(size.0, size.1);
            buffer = render::rasterize(&commands, size.0, size.1);
            last_size = size;
        }

        window.update_with_buffer(&buffer, size.0, size.1)?;
    }

    Ok(())
}
