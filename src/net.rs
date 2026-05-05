use std::{
    collections::HashMap,
    fmt,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use native_tls::{TlsConnector, TlsStream};

// The network layer keeps requests tiny: GET only, synchronous, with HTTP/1.1
// keep-alive backed by a per-host connection pool. Reusing TCP/TLS sessions
// for same-origin resources is the single biggest latency win when a page
// pulls many CSS/IMG/font assets from one host.
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
        let parsed = url::Url::parse(input).map_err(|error| match error {
            url::ParseError::RelativeUrlWithoutBase => {
                NetworkError::InvalidUrl("missing scheme separator".into())
            }
            other => NetworkError::InvalidUrl(other.to_string()),
        })?;
        Self::from_url(&parsed)
    }

    pub fn resolve(&self, href: &str) -> Result<Self, NetworkError> {
        // Reconstruct a `url::Url` from our flattened fields so `join` can do
        // standards-compliant relative resolution (handles `..`, `.`, scheme
        // promotion, query/fragment stripping, etc).
        let base = url::Url::parse(&self.to_string())
            .map_err(|error| NetworkError::InvalidUrl(error.to_string()))?;
        let resolved = base
            .join(href)
            .map_err(|error| NetworkError::InvalidUrl(error.to_string()))?;
        Self::from_url(&resolved)
    }

    // Pulls scheme/host/port out of a `url::Url`, rejects anything that's
    // not http(s), and folds path + `?query` + `#fragment` into the toy's
    // single `path` field. Keeping the fragment glued onto `path` matches
    // the previous hand-rolled parser's behaviour — `JsRuntime`'s location
    // accessors re-split it back out on read for `location.hash`. The HTTP
    // writer is the only other reader of `path` and does not care about
    // the trailing fragment in practice (a real browser would strip it
    // before serialising the request line; we don't bother yet).
    fn from_url(parsed: &url::Url) -> Result<Self, NetworkError> {
        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(NetworkError::UnsupportedScheme(scheme.into()));
        }
        let host = parsed
            .host_str()
            .filter(|host| !host.is_empty())
            .ok_or_else(|| NetworkError::InvalidUrl("missing host".into()))?
            .to_string();
        let port = parsed
            .port_or_known_default()
            .ok_or_else(|| NetworkError::InvalidUrl("missing port".into()))?;

        let mut path = parsed.path().to_string();
        if path.is_empty() {
            path.push('/');
        }
        if let Some(query) = parsed.query() {
            path.push('?');
            path.push_str(query);
        }
        if let Some(fragment) = parsed.fragment() {
            path.push('#');
            path.push_str(fragment);
        }

        Ok(Self {
            scheme: scheme.to_string(),
            host,
            port,
            path,
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

    if let Some(content_type) = response.header("content-type")
        && !content_type.starts_with("text/html")
    {
        return Err(NetworkError::UnexpectedContentType(
            content_type.to_string(),
        ));
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

    if let Some(content_type) = response.header("content-type")
        && !content_type.starts_with("text/css")
    {
        return Err(NetworkError::UnexpectedContentType(
            content_type.to_string(),
        ));
    }

    String::from_utf8(response.body).map_err(|_| NetworkError::InvalidBodyEncoding)
}

// Skips the strict Content-Type check that `load_css` does. Real browsers
// happily run scripts served as `text/javascript`, `application/javascript`,
// `application/x-javascript`, or even `text/plain`, and many static hosts
// label JS files inconsistently — enforcing a whitelist here would silently
// break otherwise-working pages.
pub fn load_script(url: &Url) -> Result<String, NetworkError> {
    let response = fetch(url)?.response;

    if response.status_code != 200 {
        return Err(NetworkError::HttpStatus(
            response.status_code,
            response.reason_phrase,
        ));
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

    if let Some(content_type) = response.header("content-type")
        && !content_type.starts_with("image/")
    {
        return Err(NetworkError::UnexpectedContentType(
            content_type.to_string(),
        ));
    }

    Ok(response.body)
}

pub fn fetch(url: &Url) -> Result<FetchResult, NetworkError> {
    fetch_with_request(url, "GET", &[], &[])
}

/// Generalised fetch that lets callers pick the HTTP method and tack
/// on extra headers / a request body. `extra_headers` are appended
/// after the toy's defaults (Host, Connection, User-Agent, Accept,
/// Accept-Encoding); duplicate names are sent verbatim — the toy does
/// not deduplicate. The body is sent verbatim and the request also
/// gains a `Content-Length` header when non-empty.
///
/// On 3xx the toy follows redirects with the same method (no GET
/// downgrade for 303). That's good enough for the common case where
/// a POST endpoint redirects to the same origin's success page; a
/// real browser would distinguish 303 vs 307/308.
pub fn fetch_with_request(
    url: &Url,
    method: &str,
    extra_headers: &[(String, String)],
    body: &[u8],
) -> Result<FetchResult, NetworkError> {
    let mut current_url = url.clone();

    for _ in 0..10 {
        let response = http_request(method, &current_url, extra_headers, body)?;

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
    http_request("GET", url, &[], &[])
}

// Single send-and-receive for any HTTP/1.1 method. The connection
// pool is method-agnostic — keep-alive sockets work fine for back-
// to-back POSTs as long as the server returns Content-Length /
// chunked framing (same condition `read_response` already enforces).
// Mirrors the same one-retry policy http_get used: a stale pooled
// socket fails the first exchange and we fall back to a fresh conn.
fn http_request(
    method: &str,
    url: &Url,
    extra_headers: &[(String, String)],
    body: &[u8],
) -> Result<HttpResponse, NetworkError> {
    if let Some(mut conn) = take_conn(&url.scheme, &url.host, url.port)
        && let Ok((response, reusable)) = exchange(&mut conn, url, method, extra_headers, body)
    {
        if reusable {
            return_conn(&url.scheme, &url.host, url.port, conn);
        }
        return Ok(response);
    }

    let mut conn = create_conn(url)?;
    let (response, reusable) = exchange(&mut conn, url, method, extra_headers, body)?;
    if reusable {
        return_conn(&url.scheme, &url.host, url.port, conn);
    }
    Ok(response)
}

fn exchange(
    conn: &mut PoolConn,
    url: &Url,
    method: &str,
    extra_headers: &[(String, String)],
    body: &[u8],
) -> Result<(HttpResponse, bool), NetworkError> {
    let mut request = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\nUser-Agent: mini-browser/0.1\r\nAccept: text/html,*/*\r\nAccept-Encoding: identity\r\n",
        method, url.path, url.host
    );
    // Author-supplied headers ride after the defaults so they can
    // override Accept/User-Agent for callers that need to. The toy
    // does not deduplicate — if the same header lands twice the
    // server sees both copies.
    for (name, value) in extra_headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    if !body.is_empty() {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    conn.write_all(request.as_bytes())
        .map_err(|error| NetworkError::Io(error.to_string()))?;
    if !body.is_empty() {
        conn.write_all(body)
            .map_err(|error| NetworkError::Io(error.to_string()))?;
    }
    read_response(conn)
}

// Reads one full HTTP/1.1 response off `conn` and returns the parsed response
// plus a hint about whether the underlying connection can be returned to the
// pool. Reusable means: response body length was determined precisely (either
// Content-Length or chunked), and the server did not signal Connection: close.
fn read_response(conn: &mut PoolConn) -> Result<(HttpResponse, bool), NetworkError> {
    let mut status_line = String::new();
    if conn
        .read_line(&mut status_line)
        .map_err(|error| NetworkError::Io(error.to_string()))?
        == 0
    {
        return Err(NetworkError::InvalidResponse(
            "connection closed before status line".into(),
        ));
    }

    let trimmed_status = status_line.trim_end_matches(['\r', '\n']);
    let mut status_parts = trimmed_status.splitn(3, ' ');
    let _http_version = status_parts
        .next()
        .ok_or_else(|| NetworkError::InvalidResponse("missing http version".into()))?;
    let status_code = status_parts
        .next()
        .ok_or_else(|| NetworkError::InvalidResponse("missing status code".into()))?
        .parse::<u16>()
        .map_err(|_| NetworkError::InvalidResponse("invalid status code".into()))?;
    let reason_phrase = status_parts.next().unwrap_or("").to_string();

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if conn
            .read_line(&mut line)
            .map_err(|error| NetworkError::Io(error.to_string()))?
            == 0
        {
            return Err(NetworkError::InvalidResponse(
                "connection closed mid-headers".into(),
            ));
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some((name, value)) = trimmed.split_once(':') {
            headers.push((
                name.trim().to_ascii_lowercase(),
                value.trim().to_string(),
            ));
        }
    }

    let content_length = find_header(&headers, "content-length")
        .and_then(|value| value.parse::<usize>().ok());
    let chunked = find_header(&headers, "transfer-encoding")
        .map(|value| value.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    let connection_close = find_header(&headers, "connection")
        .map(|value| value.to_ascii_lowercase().contains("close"))
        .unwrap_or(false);

    let body = if chunked {
        read_chunked_body(conn)?
    } else if let Some(length) = content_length {
        let mut body = vec![0u8; length];
        conn.read_exact(&mut body)
            .map_err(|error| NetworkError::Io(error.to_string()))?;
        body
    } else {
        // No length signal — read until the server closes. Connection cannot
        // be reused after this since framing is "until EOF".
        let mut body = Vec::new();
        conn.read_to_end(&mut body)
            .map_err(|error| NetworkError::Io(error.to_string()))?;
        body
    };

    let reusable = !connection_close && (chunked || content_length.is_some());

    Ok((
        HttpResponse {
            status_code,
            reason_phrase,
            headers,
            body,
        },
        reusable,
    ))
}

fn read_chunked_body(conn: &mut PoolConn) -> Result<Vec<u8>, NetworkError> {
    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        if conn
            .read_line(&mut size_line)
            .map_err(|error| NetworkError::Io(error.to_string()))?
            == 0
        {
            return Err(NetworkError::InvalidResponse(
                "connection closed mid-chunk-size".into(),
            ));
        }
        let size_token = size_line
            .trim_end_matches(['\r', '\n'])
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let size = usize::from_str_radix(size_token, 16).map_err(|_| {
            NetworkError::InvalidResponse(format!("invalid chunk size {size_token:?}"))
        })?;
        if size == 0 {
            // Discard optional trailers up to the final blank line.
            loop {
                let mut trailer = String::new();
                if conn
                    .read_line(&mut trailer)
                    .map_err(|error| NetworkError::Io(error.to_string()))?
                    == 0
                    || trailer == "\r\n"
                    || trailer == "\n"
                {
                    break;
                }
            }
            return Ok(body);
        }
        let mut chunk = vec![0u8; size];
        conn.read_exact(&mut chunk)
            .map_err(|error| NetworkError::Io(error.to_string()))?;
        body.extend_from_slice(&chunk);
        let mut crlf = [0u8; 2];
        conn.read_exact(&mut crlf)
            .map_err(|error| NetworkError::Io(error.to_string()))?;
    }
}

fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header_name, _)| header_name == name)
        .map(|(_, value)| value.as_str())
}

// A `PoolConn` is "a buffered stream we can speak HTTP over". The buffering is
// kept on the same value as the stream so that any unread bytes inside the
// BufReader come back along with the connection when it returns to the pool.
enum PoolConn {
    Plain(BufReader<TcpStream>),
    Tls(BufReader<TlsStream<TcpStream>>),
}

impl Read for PoolConn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            PoolConn::Plain(reader) => reader.read(buf),
            PoolConn::Tls(reader) => reader.read(buf),
        }
    }
}

impl BufRead for PoolConn {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        match self {
            PoolConn::Plain(reader) => reader.fill_buf(),
            PoolConn::Tls(reader) => reader.fill_buf(),
        }
    }

    fn consume(&mut self, amount: usize) {
        match self {
            PoolConn::Plain(reader) => reader.consume(amount),
            PoolConn::Tls(reader) => reader.consume(amount),
        }
    }
}

impl Write for PoolConn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            PoolConn::Plain(reader) => reader.get_mut().write(buf),
            PoolConn::Tls(reader) => reader.get_mut().write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            PoolConn::Plain(reader) => reader.get_mut().flush(),
            PoolConn::Tls(reader) => reader.get_mut().flush(),
        }
    }
}

#[derive(Default)]
struct ConnectionPool {
    plain: HashMap<(String, u16), Vec<BufReader<TcpStream>>>,
    tls: HashMap<(String, u16), Vec<BufReader<TlsStream<TcpStream>>>>,
}

fn pool() -> &'static Mutex<ConnectionPool> {
    static POOL: OnceLock<Mutex<ConnectionPool>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(ConnectionPool::default()))
}

fn take_conn(scheme: &str, host: &str, port: u16) -> Option<PoolConn> {
    let mut p = pool().lock().ok()?;
    match scheme {
        "http" => {
            let queue = p.plain.get_mut(&(host.to_string(), port))?;
            queue.pop().map(PoolConn::Plain)
        }
        "https" => {
            let queue = p.tls.get_mut(&(host.to_string(), port))?;
            queue.pop().map(PoolConn::Tls)
        }
        _ => None,
    }
}

fn return_conn(scheme: &str, host: &str, port: u16, conn: PoolConn) {
    let Ok(mut p) = pool().lock() else { return };
    match (scheme, conn) {
        ("http", PoolConn::Plain(reader)) => {
            p.plain
                .entry((host.to_string(), port))
                .or_default()
                .push(reader);
        }
        ("https", PoolConn::Tls(reader)) => {
            p.tls
                .entry((host.to_string(), port))
                .or_default()
                .push(reader);
        }
        // Mismatched scheme/conn variants are dropped (closed). Should not
        // happen in practice, but we'd rather close than poison the pool.
        _ => {}
    }
}

fn create_conn(url: &Url) -> Result<PoolConn, NetworkError> {
    let stream = TcpStream::connect((url.host.as_str(), url.port))
        .map_err(|error| NetworkError::Io(error.to_string()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| NetworkError::Io(error.to_string()))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| NetworkError::Io(error.to_string()))?;

    match url.scheme.as_str() {
        "http" => Ok(PoolConn::Plain(BufReader::new(stream))),
        "https" => {
            let connector =
                TlsConnector::new().map_err(|error| NetworkError::Tls(error.to_string()))?;
            let tls_stream = connector
                .connect(url.host.as_str(), stream)
                .map_err(|error| NetworkError::Tls(error.to_string()))?;
            Ok(PoolConn::Tls(BufReader::new(tls_stream)))
        }
        scheme => Err(NetworkError::UnsupportedScheme(scheme.into())),
    }
}

fn is_redirect_status(status_code: u16) -> bool {
    matches!(status_code, 301 | 302 | 303 | 307 | 308)
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

            let body = "<html><body>Hello</body></html>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
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
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{second_port}/final.html\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let target_server = thread::spawn(move || {
            let (mut stream, _) = second.accept().unwrap();
            let mut request = [0; 1024];
            let bytes_read = stream.read(&mut request).unwrap();
            let request_text = String::from_utf8_lossy(&request[..bytes_read]);
            assert!(request_text.starts_with("GET /final.html HTTP/1.1"));

            let body = "<html>done</html>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
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
                    let response = "HTTP/1.1 301 Moved Permanently\r\nLocation: /final.html\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    stream.write_all(response.as_bytes()).unwrap();
                } else {
                    assert!(request_text.starts_with("GET /final.html HTTP/1.1"));
                    let body = "<html>final</html>";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
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

            let response = "HTTP/1.1 302 Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
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

            let body = "missing";
            let response = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
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

            let body = "body { margin-top: 8px; }";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
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

    #[test]
    fn decodes_chunked_response_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();

            // "Wikipedia in\r\n\r\nchunks." spread over two chunks plus terminator.
            let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
        });

        let url = Url::parse(&format!("http://127.0.0.1:{port}/chunked")).unwrap();
        let response = http_get(&url).unwrap();

        server.join().unwrap();
        assert_eq!(response.body, b"Wikipedia");
    }

    #[test]
    fn reuses_pooled_connection_for_same_host() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        // Server accepts ONE connection and serves TWO requests over it. If our
        // client opens a second TCP connection instead of reusing the pooled
        // one, accept() never returns and the test hangs.
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for index in 0..2 {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0, "request {index} read 0 bytes");
                let body = format!("payload-{index}");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let url = Url::parse(&format!("http://127.0.0.1:{port}/x")).unwrap();
        let first = http_get(&url).unwrap();
        let second = http_get(&url).unwrap();

        server.join().unwrap();
        assert_eq!(first.body, b"payload-0");
        assert_eq!(second.body, b"payload-1");
    }
}
