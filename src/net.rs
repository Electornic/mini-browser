use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkError {
    UnsupportedScheme(String),
    InvalidUrl(String),
    Io(String),
    InvalidResponse(String),
    HttpStatus(u16, String),
    InvalidBodyEncoding,
    UnexpectedContentType(String),
}

impl Url {
    pub fn parse(input: &str) -> Result<Self, NetworkError> {
        let (scheme, rest) = input
            .split_once("://")
            .ok_or_else(|| NetworkError::InvalidUrl("missing scheme separator".into()))?;

        if scheme != "http" {
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
            None => (authority, 80),
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
}

pub fn load_html(url: &Url) -> Result<String, NetworkError> {
    let response = http_get(url)?;

    if response.status_code != 200 {
        return Err(NetworkError::HttpStatus(
            response.status_code,
            response.reason_phrase,
        ));
    }

    if let Some(content_type) = header(&response, "content-type") {
        if !content_type.starts_with("text/html") {
            return Err(NetworkError::UnexpectedContentType(
                content_type.to_string(),
            ));
        }
    }

    String::from_utf8(response.body).map_err(|_| NetworkError::InvalidBodyEncoding)
}

pub fn http_get(url: &Url) -> Result<HttpResponse, NetworkError> {
    let mut stream = TcpStream::connect((url.host.as_str(), url.port))
        .map_err(|error| NetworkError::Io(error.to_string()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| NetworkError::Io(error.to_string()))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| NetworkError::Io(error.to_string()))?;

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: mini-browser/0.1\r\nAccept: text/html,*/*\r\n\r\n",
        url.path, url.host
    );

    stream
        .write_all(request.as_bytes())
        .map_err(|error| NetworkError::Io(error.to_string()))?;

    let mut response_bytes = Vec::new();
    stream
        .read_to_end(&mut response_bytes)
        .map_err(|error| NetworkError::Io(error.to_string()))?;

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

fn header<'a>(response: &'a HttpResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(header_name, _)| header_name == name)
        .map(|(_, value)| value.as_str())
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::{NetworkError, Url, http_get, load_html};

    #[test]
    fn parses_default_http_port_and_root_path() {
        let url = Url::parse("http://example.com").unwrap();

        assert_eq!(url.scheme, "http");
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 80);
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
        let error = Url::parse("https://example.com").unwrap_err();

        assert_eq!(error, NetworkError::UnsupportedScheme("https".into()));
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
}
