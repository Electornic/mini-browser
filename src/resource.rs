use crate::{
    dom::{Node, NodeType},
    net::{self, NetworkError, Url},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceError {
    MissingHref,
    Network(NetworkError),
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

fn stylesheet_urls(document: &[Node], base_url: &Url) -> Result<Vec<Url>, ResourceError> {
    let mut urls = Vec::new();

    for node in document {
        collect_stylesheet_urls(node, base_url, &mut urls)?;
    }

    Ok(urls)
}

fn collect_stylesheet_urls(
    node: &Node,
    base_url: &Url,
    urls: &mut Vec<Url>,
) -> Result<(), ResourceError> {
    if let NodeType::Element(element) = &node.node_type {
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

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use crate::{html, net::Url};

    use super::load_stylesheets;

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
}
