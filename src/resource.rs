use crate::{
    dom::{Node, NodeType},
    net::{self, NetworkError, Url},
};

// Resource loading stays separate from HTML parsing so the app can decide when to fetch extras.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceError {
    MissingHref,
    MissingSrc,
    DecodeImage(String),
    Network(NetworkError),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedImage {
    pub url: Url,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

impl From<NetworkError> for ResourceError {
    fn from(value: NetworkError) -> Self {
        Self::Network(value)
    }
}

pub fn load_stylesheets(document: &[Node], base_url: &Url) -> Result<Vec<String>, ResourceError> {
    let stylesheet_urls = stylesheet_urls(document, base_url)?;
    stylesheet_urls
        .iter()
        .map(net::load_css)
        .collect::<Result<Vec<_>, _>>()
        .map_err(ResourceError::from)
}

pub fn load_images(document: &[Node], base_url: &Url) -> Result<Vec<LoadedImage>, ResourceError> {
    let image_urls = image_urls(document, base_url)?;
    image_urls
        .iter()
        .map(|url| {
            let bytes = net::load_image(url)?;
            decode_image(url.clone(), &bytes)
        })
        .collect()
}

fn stylesheet_urls(document: &[Node], base_url: &Url) -> Result<Vec<Url>, ResourceError> {
    let mut urls = Vec::new();

    for node in document {
        collect_stylesheet_urls(node, base_url, &mut urls)?;
    }

    Ok(urls)
}

fn image_urls(document: &[Node], base_url: &Url) -> Result<Vec<Url>, ResourceError> {
    let mut urls = Vec::new();

    for node in document {
        collect_image_urls(node, base_url, &mut urls)?;
    }

    Ok(urls)
}

fn collect_stylesheet_urls(
    node: &Node,
    base_url: &Url,
    urls: &mut Vec<Url>,
) -> Result<(), ResourceError> {
    if let NodeType::Element(element) = &node.node_type {
        // Only explicit stylesheet links are treated as CSS resources.
        if element.tag_name == "link"
            && element
                .attributes
                .get("rel")
                .is_some_and(|value| value.eq_ignore_ascii_case("stylesheet"))
        {
            let href = element
                .attributes
                .get("href")
                .ok_or(ResourceError::MissingHref)?;
            urls.push(base_url.resolve(href)?);
        }
    }

    for child in &node.children {
        collect_stylesheet_urls(child, base_url, urls)?;
    }

    Ok(())
}

fn collect_image_urls(
    node: &Node,
    base_url: &Url,
    urls: &mut Vec<Url>,
) -> Result<(), ResourceError> {
    if let NodeType::Element(element) = &node.node_type {
        // Images are fetched lazily from DOM attributes after the main document is parsed.
        if element.tag_name == "img" {
            let src = element
                .attributes
                .get("src")
                .ok_or(ResourceError::MissingSrc)?;
            urls.push(base_url.resolve(src)?);
        }
    }

    for child in &node.children {
        collect_image_urls(child, base_url, urls)?;
    }

    Ok(())
}

fn decode_image(url: Url, bytes: &[u8]) -> Result<LoadedImage, ResourceError> {
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

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use crate::{html, net::Url};

    use super::{load_images, load_stylesheets};

    #[test]
    fn resolves_stylesheet_links_from_document() {
        let nodes = html::parse(
            r#"
                <html>
                    <head>
                        <link rel="stylesheet" href="/base.css" />
                        <link rel="stylesheet" href="nested/site.css" />
                    </head>
                </html>
            "#,
        )
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 1024];
                let bytes_read = stream.read(&mut request).unwrap();
                let request_text = String::from_utf8_lossy(&request[..bytes_read]);
                let body = if request_text.starts_with("GET /base.css HTTP/1.1") {
                    "body { color: #111111; }"
                } else if request_text.starts_with("GET /articles/nested/site.css HTTP/1.1") {
                    "p { color: #222222; }"
                } else {
                    panic!("unexpected request: {request_text}");
                };

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nConnection: close\r\n\r\n{body}"
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let base_url = Url::parse(&format!("http://127.0.0.1:{port}/articles/index.html")).unwrap();
        let stylesheets = load_stylesheets(&nodes, &base_url).unwrap();

        server.join().unwrap();
        assert_eq!(stylesheets.len(), 2);
        assert_eq!(stylesheets[0], "body { color: #111111; }");
        assert_eq!(stylesheets[1], "p { color: #222222; }");
    }

    #[test]
    fn loads_image_resources_from_document() {
        let nodes = html::parse(
            r#"
                <html>
                    <body>
                        <img src="/pixel.png" />
                    </body>
                </html>
            "#,
        )
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let bytes_read = stream.read(&mut request).unwrap();
            let request_text = String::from_utf8_lossy(&request[..bytes_read]);
            assert!(request_text.starts_with("GET /pixel.png HTTP/1.1"));

            let png: &[u8] = &[
                0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
                0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
                0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
                0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0xF0, 0x1F, 0x00, 0x05, 0x00, 0x01, 0xFF, 0x89, 0x99,
                0x3D, 0x1D, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
            ];
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                png.len()
            );
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(png).unwrap();
        });

        let base_url = Url::parse(&format!("http://127.0.0.1:{port}/index.html")).unwrap();
        let images = load_images(&nodes, &base_url).unwrap();

        server.join().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].width, 1);
        assert_eq!(images[0].height, 1);
        assert_eq!(images[0].pixels, vec![0xFF0000]);
    }
}
