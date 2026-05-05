// Pure data + decoder half of the resource layer. The fetching half (which
// reaches into `net::*`) lives in the root crate (will move to `mb-runtime`
// in 4.9d). Engine code only ever reads `LoadedImage`'s pixel buffer, so
// keeping the data type in mb-dom lets the renderer stay below the runtime.

use crate::url::{NetworkError, Url};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceError {
    MissingHref,
    MissingSrc,
    DecodeImage(String),
    Network(NetworkError),
}

impl From<NetworkError> for ResourceError {
    fn from(value: NetworkError) -> Self {
        Self::Network(value)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedImage {
    pub url: Url,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

pub fn decode_image(url: Url, bytes: &[u8]) -> Result<LoadedImage, ResourceError> {
    // Decode to a simple RGB pixel buffer so rendering does not depend on image crate types.
    let decoded = image::load_from_memory(bytes)
        .map_err(|error| ResourceError::DecodeImage(error.to_string()))?
        .to_rgba8();
    let (width, height) = decoded.dimensions();
    let pixels = decoded
        .pixels()
        .map(|pixel| {
            let [r, g, b, _a] = pixel.0;
            (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        })
        .collect();

    Ok(LoadedImage {
        url,
        width: width as usize,
        height: height as usize,
        pixels,
    })
}
