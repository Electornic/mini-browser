use std::{
    fmt,
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

use native_tls::TlsConnector;

// The network layer keeps requests tiny: GET only, synchronous, and close-after-response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status_code: u16,
    pub reason_phrase: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header_name, _)| header_name == name)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResult {
    pub final_url: Url,
    pub response: HttpResponse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkError {
    UnsupportedScheme(String),
    InvalidUrl(String),
    Io(String),
    Tls(String),
    InvalidResponse(String),
    MissingLocationHeader,
    RedirectLimitExceeded,
    HttpStatus(u16, String),
    InvalidBodyEncoding,
    UnexpectedContentType(String),
}

impl Url {
    pub fn parse(input: &str) -> Result<Self, NetworkError> {
        // Relative pieces are handled later; parse only fully qualified URLs here.
        let (scheme, rest) = input
            .split_once("://")
            .ok_or_else(|| NetworkError::InvalidUrl("missing scheme separator".into()))?;

        if scheme != "http" && scheme != "https" {
            return Err(NetworkError::UnsupportedScheme(scheme.into()));
        }

        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, "/".to_string()),
        };

        if authority.is_empty() {
            return Err(NetworkError::InvalidUrl("missing host".into()));
        }

        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => {
                let port = port
                    .parse::<u16>()
                    .map_err(|_| NetworkError::InvalidUrl("invalid port".into()))?;
                (host, port)
            }
            None => (authority, if scheme == "https" { 443 } else { 80 }),
        };

        if host.is_empty() {
            return Err(NetworkError::InvalidUrl("missing host".into()));
        }

        Ok(Self {
            scheme: scheme.into(),
            host: host.into(),
            port,
            path,
        })
    }

    pub fn resolve(&self, href: &str) -> Result<Self, NetworkError> {
        if href.contains("://") {
            return Self::parse(href);
        }

        // Relative resources resolve against the current document path, not the process cwd.
        let path = if href.starts_with('/') {
            href.to_string()
        } else {
            let base_dir = self
                .path
                .rsplit_once('/')
                .map(|(prefix, _)| {
                    if prefix.is_empty() {
                        "/".to_string()
                    } else {
                        format!("{prefix}/")
                    }
                })
                .unwrap_or_else(|| "/".to_string());
            format!("{base_dir}{href}")
        };

        Ok(Self {
            scheme: self.scheme.clone(),
            host: self.host.clone(),
            port: self.port,
            path: normalize_path(&path),
        })
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.scheme, self.host)?;
        let default_port = if self.scheme == "https" { 443 } else { 80 };
        if self.port != default_port {
            write!(f, ":{}", self.port)?;
        }
        write!(f, "{}", self.path)
    }
}

pub fn load_html(url: &Url) -> Result<String, NetworkError> {
    let (document, _) = load_html_document(url)?;
    Ok(document)
}

pub fn load_html_document(url: &Url) -> Result<(String, Url), NetworkError> {
    let fetch_result = fetch(url)?;
    let response = fetch_result.response;

    if response.status_code != 200 {
        return Err(NetworkError::HttpStatus(
            response.status_code,
            response.reason_phrase,
        ));
    }

    if let Some(content_type) = response.header("content-type") {
        if !content_type.starts_with("text/html") {
            return Err(NetworkError::UnexpectedContentType(
                content_type.to_string(),
            ));
        }
    }

    let document =
        String::from_utf8(response.body).map_err(|_| NetworkError::InvalidBodyEncoding)?;
    Ok((document, fetch_result.final_url))
}

pub fn load_css(url: &Url) -> Result<String, NetworkError> {
    let response = fetch(url)?.response;

    if response.status_code != 200 {
        return Err(NetworkError::HttpStatus(
            response.status_code,
            response.reason_phrase,
        ));
    }

    if let Some(content_type) = response.header("content-type") {
        if !content_type.starts_with("text/css") {
            return Err(NetworkError::UnexpectedContentType(
                content_type.to_string(),
            ));
        }
    }

    String::from_utf8(response.body).map_err(|_| NetworkError::InvalidBodyEncoding)
}

pub fn load_image(url: &Url) -> Result<Vec<u8>, NetworkError> {
    let response = fetch(url)?.response;

    if response.status_code != 200 {
        return Err(NetworkError::HttpStatus(
            response.status_code,
            response.reason_phrase,
        ));
    }

    if let Some(content_type) = response.header("content-type") {
        if !content_type.starts_with("image/") {
            return Err(NetworkError::UnexpectedContentType(
                content_type.to_string(),
            ));
        }
    }

    Ok(response.body)
}

pub fn fetch(url: &Url) -> Result<FetchResult, NetworkError> {
    let mut current_url = url.clone();

    // Redirects are followed in-place so callers always see the final document URL.
    for _ in 0..10 {
        let response = http_get(&current_url)?;

        if is_redirect_status(response.status_code) {
            let location = response
                .header("location")
                .ok_or(NetworkError::MissingLocationHeader)?;
            current_url = current_url.resolve(location)?;
            continue;
        }

        return Ok(FetchResult {
            final_url: current_url,
            response,
        });
    }

    Err(NetworkError::RedirectLimitExceeded)
}

pub fn http_get(url: &Url) -> Result<HttpResponse, NetworkError> {
    let mut tcp_stream = TcpStream::connect((url.host.as_str(), url.port))
        .map_err(|error| NetworkError::Io(error.to_string()))?;
    tcp_stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| NetworkError::Io(error.to_string()))?;
    tcp_stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| NetworkError::Io(error.to_string()))?;

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: mini-browser/0.1\r\nAccept: text/html,*/*\r\nAccept-Encoding: identity\r\n\r\n",
        url.path, url.host
    );

    let mut response_bytes = Vec::new();
    // TLS only changes how bytes move over the socket; HTTP parsing stays exactly the same.
    match url.scheme.as_str() {
        "http" => {
            tcp_stream
                .write_all(request.as_bytes())
                .map_err(|error| NetworkError::Io(error.to_string()))?;
            tcp_stream
                .read_to_end(&mut response_bytes)
                .map_err(|error| NetworkError::Io(error.to_string()))?;
        }
        "https" => {
            let connector =
                TlsConnector::new().map_err(|error| NetworkError::Tls(error.to_string()))?;
            let mut tls_stream = connector
                .connect(url.host.as_str(), tcp_stream)
                .map_err(|error| NetworkError::Tls(error.to_string()))?;
            tls_stream
                .write_all(request.as_bytes())
                .map_err(|error| NetworkError::Io(error.to_string()))?;
            tls_stream
                .read_to_end(&mut response_bytes)
                .map_err(|error| NetworkError::Io(error.to_string()))?;
        }
        scheme => {
            return Err(NetworkError::UnsupportedScheme(scheme.into()));
        }
    }

    parse_response(&response_bytes)
}

fn parse_response(bytes: &[u8]) -> Result<HttpResponse, NetworkError> {
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| NetworkError::InvalidResponse("missing header terminator".into()))?;

    let header_bytes = &bytes[..header_end];
    let body = bytes[header_end + 4..].to_vec();
    let header_text = std::str::from_utf8(header_bytes)
        .map_err(|_| NetworkError::InvalidResponse("headers are not valid utf-8".into()))?;

    let mut lines = header_text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| NetworkError::InvalidResponse("missing status line".into()))?;
    let mut status_parts = status_line.splitn(3, ' ');
    let _http_version = status_parts
        .next()
        .ok_or_else(|| NetworkError::InvalidResponse("missing http version".into()))?;
    let status_code = status_parts
        .next()
        .ok_or_else(|| NetworkError::InvalidResponse("missing status code".into()))?
        .parse::<u16>()
        .map_err(|_| NetworkError::InvalidResponse("invalid status code".into()))?;
    let reason_phrase = status_parts.next().unwrap_or("").to_string();

    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();

    Ok(HttpResponse {
        status_code,
        reason_phrase,
        headers,
        body,
    })
}

fn is_redirect_status(status_code: u16) -> bool {
    matches!(status_code, 301 | 302 | 303 | 307 | 308)
}

fn normalize_path(path: &str) -> String {
    let mut segments = Vec::new();

    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            value => segments.push(value),
        }
    }

    format!("/{}", segments.join("/"))
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::{
        NetworkError, Url, fetch, http_get, load_css, load_html, load_html_document, load_image,
    };

    #[test]
    fn parses_default_http_port_and_root_path() {
        let url = Url::parse("http://example.com").unwrap();

        assert_eq!(url.scheme, "http");
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 80);
        assert_eq!(url.path, "/");
    }

    #[test]
    fn parses_default_https_port_and_root_path() {
        let url = Url::parse("https://example.com").unwrap();

        assert_eq!(url.scheme, "https");
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 443);
        assert_eq!(url.path, "/");
    }

    #[test]
    fn parses_explicit_port_and_path() {
        let url = Url::parse("http://localhost:8080/index.html").unwrap();

        assert_eq!(url.host, "localhost");
        assert_eq!(url.port, 8080);
        assert_eq!(url.path, "/index.html");
    }

    #[test]
    fn rejects_non_http_scheme() {
        let error = Url::parse("ftp://example.com").unwrap_err();

        assert_eq!(error, NetworkError::UnsupportedScheme("ftp".into()));
    }

    #[test]
    fn resolves_relative_paths_against_base_url() {
        let base = Url::parse("http://example.com/articles/intro/index.html").unwrap();

        assert_eq!(
            base.resolve("../styles/site.css").unwrap(),
            Url {
                scheme: "http".into(),
                host: "example.com".into(),
                port: 80,
                path: "/articles/styles/site.css".into(),
            }
        );
        assert_eq!(
            base.resolve("/reset.css").unwrap(),
            Url {
                scheme: "http".into(),
                host: "example.com".into(),
                port: 80,
                path: "/reset.css".into(),
            }
        );
    }

    #[test]
    fn display_omits_default_https_port() {
        let url = Url::parse("https://example.com/secure").unwrap();
        assert_eq!(url.to_string(), "https://example.com/secure");

        let custom = Url::parse("https://example.com:8443/secure").unwrap();
        assert_eq!(custom.to_string(), "https://example.com:8443/secure");
    }

    #[test]
    fn downloads_html_body_from_local_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let bytes_read = stream.read(&mut request).unwrap();
            let request_text = String::from_utf8_lossy(&request[..bytes_read]);
            assert!(request_text.starts_with("GET / HTTP/1.1"));

            let response = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/html; charset=utf-8\r\n",
                "Connection: close\r\n",
                "\r\n",
                "<html><body>Hello</body></html>"
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        let html = load_html(&url).unwrap();

        server.join().unwrap();
        assert_eq!(html, "<html><body>Hello</body></html>");
    }

    #[test]
    fn follows_redirect_and_returns_final_url() {
        let first = TcpListener::bind("127.0.0.1:0").unwrap();
        let second = TcpListener::bind("127.0.0.1:0").unwrap();
        let first_port = first.local_addr().unwrap().port();
        let second_port = second.local_addr().unwrap().port();

        let redirect_server = thread::spawn(move || {
            let (mut stream, _) = first.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();

            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{second_port}/final.html\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let target_server = thread::spawn(move || {
            let (mut stream, _) = second.accept().unwrap();
            let mut request = [0; 1024];
            let bytes_read = stream.read(&mut request).unwrap();
            let request_text = String::from_utf8_lossy(&request[..bytes_read]);
            assert!(request_text.starts_with("GET /final.html HTTP/1.1"));

            let response = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/html\r\n",
                "Connection: close\r\n",
                "\r\n",
                "<html>done</html>"
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let result =
            fetch(&Url::parse(&format!("http://127.0.0.1:{first_port}/start")).unwrap()).unwrap();

        redirect_server.join().unwrap();
        target_server.join().unwrap();
        assert_eq!(
            result.final_url,
            Url::parse(&format!("http://127.0.0.1:{second_port}/final.html")).unwrap()
        );
        assert_eq!(result.response.body, b"<html>done</html>");
    }

    #[test]
    fn html_loader_reports_final_redirect_url() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            for request_index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 1024];
                let bytes_read = stream.read(&mut request).unwrap();
                let request_text = String::from_utf8_lossy(&request[..bytes_read]);

                if request_index == 0 {
                    assert!(request_text.starts_with("GET /start HTTP/1.1"));
                    let response = "HTTP/1.1 301 Moved Permanently\r\nLocation: /final.html\r\nConnection: close\r\n\r\n";
                    stream.write_all(response.as_bytes()).unwrap();
                } else {
                    assert!(request_text.starts_with("GET /final.html HTTP/1.1"));
                    let response = concat!(
                        "HTTP/1.1 200 OK\r\n",
                        "Content-Type: text/html\r\n",
                        "Connection: close\r\n",
                        "\r\n",
                        "<html>final</html>"
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                }
            }
        });

        let url = Url::parse(&format!("http://127.0.0.1:{port}/start")).unwrap();
        let (html, final_url) = load_html_document(&url).unwrap();

        server.join().unwrap();
        assert_eq!(html, "<html>final</html>");
        assert_eq!(
            final_url,
            Url::parse(&format!("http://127.0.0.1:{port}/final.html")).unwrap()
        );
    }

    #[test]
    fn errors_when_redirect_has_no_location() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();

            let response = "HTTP/1.1 302 Found\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
        });

        let error = fetch(&Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap()).unwrap_err();

        server.join().unwrap();
        assert_eq!(error, NetworkError::MissingLocationHeader);
    }

    #[test]
    fn returns_response_headers_and_status() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();

            let response = concat!(
                "HTTP/1.1 404 Not Found\r\n",
                "Content-Type: text/plain\r\n",
                "Connection: close\r\n",
                "\r\n",
                "missing"
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let url = Url::parse(&format!("http://127.0.0.1:{port}/missing")).unwrap();
        let response = http_get(&url).unwrap();

        server.join().unwrap();
        assert_eq!(response.status_code, 404);
        assert_eq!(response.reason_phrase, "Not Found");
        assert_eq!(response.body, b"missing");
    }

    #[test]
    fn downloads_css_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();

            let response = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/css\r\n",
                "Connection: close\r\n",
                "\r\n",
                "body { margin-top: 8px; }"
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let url = Url::parse(&format!("http://127.0.0.1:{port}/styles.css")).unwrap();
        let css = load_css(&url).unwrap();

        server.join().unwrap();
        assert_eq!(css, "body { margin-top: 8px; }");
    }

    #[test]
    fn downloads_image_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();

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

        let url = Url::parse(&format!("http://127.0.0.1:{port}/pixel.png")).unwrap();
        let bytes = load_image(&url).unwrap();

        server.join().unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }
}
