// L4 end-to-end for `--http` (docs/concept.md G3b): run the real binary
// headless with `--http --port 0`, then speak streamable HTTP to it the way
// an attaching MCP client would. Checks the security gates (token, Origin,
// Host, path) and that two clients share one browser.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const TOKEN: &str = "e2e-token-0123456789abcdef0123456789";

fn fixture_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                    break;
                }
            }
            let body =
                "<html><head><title>Home</title></head><body><h1>Hello agent</h1></body></html>";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    base
}

struct Server {
    child: Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mb-mcp"))
            .args(["--http", "--port", "0", "--viewport", "800x600"])
            .env("MB_MCP_TOKEN", TOKEN)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn mb-mcp");
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut lines = Vec::new();
        while Instant::now() < deadline {
            let Ok(line) = rx.recv_timeout(Duration::from_millis(200)) else {
                continue;
            };
            if let Some(url) = line.split("ready (http) at ").nth(1) {
                let port = url
                    .trim_start_matches("http://127.0.0.1:")
                    .trim_end_matches("/mcp")
                    .parse()
                    .unwrap();
                // Keep draining so the child never blocks on stderr.
                thread::spawn(move || for _ in rx {});
                return Server { child, port };
            }
            lines.push(line);
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("mb-mcp --http never became ready: {lines:?}");
    }
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The JSON-RPC message in a JSON or SSE (`data: ...`) body.
    fn message(&self) -> Value {
        let raw = if self.body.trim_start().starts_with('{') {
            self.body.trim()
        } else {
            self.body
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim)
                .find(|d| d.starts_with('{'))
                .unwrap_or_else(|| panic!("no data line in {:?}", self.body))
        };
        serde_json::from_str(raw).unwrap()
    }
}

/// One HTTP/1.1 request on a fresh connection. Reads until the response
/// is complete: Content-Length, end of stream, or (SSE) a JSON-RPC
/// response event.
fn request(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Response {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("host")) {
        head.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body.as_bytes()).unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => panic!("read: {e}"),
        }
        let text = String::from_utf8_lossy(&raw);
        if let Some((head, rest)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().to_string())
                })
                .and_then(|v| v.parse::<usize>().ok());
            if length.is_some_and(|n| rest.len() >= n) {
                break;
            }
            // An SSE stream may stay open; the response event is enough.
            if rest.contains("\"result\"") && rest.contains("\n\n")
                || rest.contains("\"error\"") && rest.contains("\n\n")
            {
                break;
            }
        }
        assert!(Instant::now() < deadline, "response timed out: {text}");
    }
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("bad status line in {text:?}"));
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
        .collect();
    Response {
        status,
        headers,
        body: dechunk(body),
    }
}

// Good enough for tests: drop chunk-size lines from a chunked body.
fn dechunk(body: &str) -> String {
    if !body
        .lines()
        .next()
        .is_some_and(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return body.to_string();
    }
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, after)) = rest.split_once("\r\n") {
        let Ok(n) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if n == 0 || after.len() < n {
            out.push_str(&after[..after.len().min(n)]);
            break;
        }
        out.push_str(&after[..n]);
        rest = after[n..].trim_start_matches("\r\n");
    }
    out
}

const BEARER: &str = "Bearer e2e-token-0123456789abcdef0123456789";

fn rpc_headers<'a>(session: Option<&'a str>, version: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    let mut headers = vec![
        ("Authorization", BEARER),
        ("Content-Type", "application/json"),
        ("Accept", "application/json, text/event-stream"),
    ];
    if let Some(session) = session {
        headers.push(("Mcp-Session-Id", session));
    }
    if let Some(version) = version {
        headers.push(("MCP-Protocol-Version", version));
    }
    headers
}

struct Session {
    port: u16,
    id: Option<String>,
    version: String,
    next: u64,
}

impl Session {
    fn open(port: u16) -> Self {
        let init = json!({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "e2e", "version": "0"}
            }
        });
        let response = request(
            port,
            "POST",
            "/mcp",
            &rpc_headers(None, None),
            &init.to_string(),
        );
        assert_eq!(response.status, 200, "{}", response.body);
        let message = response.message();
        assert_eq!(message["result"]["serverInfo"]["name"], "mini-browser");
        let version = message["result"]["protocolVersion"]
            .as_str()
            .unwrap()
            .to_string();
        let id = response.header("mcp-session-id").map(str::to_string);
        let session = Session {
            port,
            id,
            version,
            next: 1,
        };
        let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let ack = request(
            port,
            "POST",
            "/mcp",
            &rpc_headers(session.id.as_deref(), Some(&session.version)),
            &initialized.to_string(),
        );
        assert!(ack.status == 202 || ack.status == 200, "{}", ack.status);
        session
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let body = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}
        });
        let response = request(
            self.port,
            "POST",
            "/mcp",
            &rpc_headers(self.id.as_deref(), Some(&self.version)),
            &body.to_string(),
        );
        assert_eq!(response.status, 200, "{}", response.body);
        let message = response.message();
        assert_eq!(message["id"], id);
        message["result"].clone()
    }
}

fn text_of(result: &Value) -> String {
    result["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn refuses_requests_without_token_from_pages_or_other_hosts() {
    let server = Server::start();
    let port = server.port;
    let init = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}"#;
    let json = ("Content-Type", "application/json");
    let accept = ("Accept", "application/json, text/event-stream");

    let anonymous = request(port, "POST", "/mcp", &[json, accept], init);
    assert_eq!(anonymous.status, 401);
    assert_eq!(anonymous.header("www-authenticate"), Some("Bearer"));

    let wrong = request(
        port,
        "POST",
        "/mcp",
        &[
            ("Authorization", "Bearer not-the-token-0123456789abcdef01"),
            json,
            accept,
        ],
        init,
    );
    assert_eq!(wrong.status, 401);

    // A web page — even one that somehow has the token.
    let page = request(
        port,
        "POST",
        "/mcp",
        &[
            ("Authorization", BEARER),
            ("Origin", "http://evil.example"),
            json,
            accept,
        ],
        init,
    );
    assert_eq!(page.status, 403);

    // DNS rebinding: the right port reached under another name.
    let evil_host = format!("evil.example:{port}");
    let rebound = request(
        port,
        "POST",
        "/mcp",
        &[
            ("Authorization", BEARER),
            ("Host", &evil_host),
            json,
            accept,
        ],
        init,
    );
    assert_eq!(rebound.status, 403);

    let elsewhere = request(port, "GET", "/", &[("Authorization", BEARER)], "");
    assert_eq!(elsewhere.status, 404);
}

#[test]
fn two_clients_attach_to_the_same_browser() {
    let base = fixture_server();
    let server = Server::start();

    let mut first = Session::open(server.port);
    let page = first.call("navigate", json!({"url": format!("{base}/")}));
    assert!(text_of(&page).contains("Hello agent"), "{page}");

    // A second client (another agent, or the same one reconnecting) sees
    // the page the first one left — the browser outlives connections.
    let mut second = Session::open(server.port);
    let seen = second.call("observe", json!({}));
    let text = text_of(&seen);
    assert!(text.contains("Hello agent"), "{text}");
    assert!(text.contains(&base), "{text}");
}
