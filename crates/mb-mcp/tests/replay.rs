// L4 for G5a: the real binary records a session over stdio (`--log`) and
// `mb-mcp replay` re-runs it — exit 0 when the page is unchanged.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread;

use serde_json::{Value, json};

fn fixture_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            let _ = reader.read_line(&mut request_line);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                    break;
                }
            }
            let body = if request_line.contains(" /next ") {
                "<html><head><title>Next</title></head><body><p>Second page</p></body></html>"
            } else {
                "<html><head><title>Home</title></head><body><h1>Hello</h1><a href=\"/next\">Next page</a></body></html>"
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    base
}

#[test]
fn a_logged_stdio_session_replays_cleanly() {
    let base = fixture_server();
    let log = std::env::temp_dir().join(format!("mb-e2e-replay-{}.jsonl", std::process::id()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_mb-mcp"))
        .args(["--viewport", "800x600", "--log"])
        .arg(&log)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut call = |id: u64, message: Value| -> Value {
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
        if message.get("id").is_none() {
            return Value::Null;
        }
        loop {
            let line: Value = serde_json::from_str(&stdout.next().unwrap().unwrap()).unwrap();
            if line["id"] == id {
                return line;
            }
        }
    };
    call(
        0,
        json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}),
    );
    call(
        0,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    let page = call(
        1,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"navigate","arguments":{"url": format!("{base}/")}}}),
    );
    let text = page["result"]["content"][1]["text"]
        .as_str()
        .unwrap()
        .to_string();
    let line = text.lines().find(|l| l.contains("Next page")).unwrap();
    let id = &line[1..line.find(']').unwrap()];
    call(
        2,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"click","arguments":{"id": id}}}),
    );
    drop(stdin);
    child.wait().unwrap();

    let recorded = std::fs::read_to_string(&log).unwrap();
    assert_eq!(recorded.lines().count(), 3, "{recorded}");

    let replay = Command::new(env!("CARGO_BIN_EXE_mb-mcp"))
        .args(["replay"])
        .arg(&log)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&replay.stderr);
    assert!(replay.status.success(), "{stderr}");
    assert!(stderr.contains("✓ 2 click link \"Next page\""), "{stderr}");
    assert!(replay.stdout.is_empty(), "replay writes nothing to stdout");
    let _ = std::fs::remove_file(&log);
}
