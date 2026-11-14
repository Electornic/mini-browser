// L3 headless integration tests (docs/concept.md "테스트 전략"): a real
// AgentBrowser against a tiny HTTP fixture server running in-process on
// 127.0.0.1 — no python, no network beyond loopback.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use mb_agent::{
    AgentBrowser, AgentConfig, AgentError, ApproveMode, Observation, ObserveOptions, Role,
};
use mb_runtime::dom::NodeId;

// ---------------------------------------------------------------------------
// Fixture server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Received {
    method: String,
    path: String,
    body: String,
}

struct Server {
    base: String,
    received: Arc<Mutex<Vec<Received>>>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let received = Arc::new(Mutex::new(Vec::new()));
        let log = received.clone();
        let other_origin = base.replace("127.0.0.1", "localhost");
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = log.clone();
                let other = other_origin.clone();
                thread::spawn(move || handle(stream, &log, &other));
            }
        });
        Self { base, received }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn paths(&self) -> Vec<String> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect()
    }

    fn posts(&self) -> Vec<Received> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "POST")
            .cloned()
            .collect()
    }
}

fn handle(stream: TcpStream, log: &Mutex<Vec<Received>>, other_origin: &str) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
    let body = String::from_utf8_lossy(&body).to_string();
    log.lock().unwrap().push(Received {
        method: method.clone(),
        path: path.clone(),
        body: body.clone(),
    });

    let (status, extra, html) = route(&method, &path, &body, other_origin);
    let mut stream = stream;
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn route(
    method: &str,
    path: &str,
    body: &str,
    other_origin: &str,
) -> (&'static str, String, String) {
    match (method, path.split('?').next().unwrap_or("")) {
        ("GET", "/signup") => ("200 OK", String::new(), signup_page(other_origin)),
        // Post/redirect/get, as real sites do.
        ("POST", "/signup") => (
            "303 See Other",
            "Location: /welcome\r\n".into(),
            String::new(),
        ),
        ("GET", "/welcome") => (
            "200 OK",
            String::new(),
            "<html><head><title>Welcome</title></head><body><h1>Account created</h1><a href=\"/signup\">Back</a></body></html>".into(),
        ),
        ("GET", "/timers") => ("200 OK", String::new(), TIMERS_PAGE.into()),
        ("GET", "/delayed") => (
            "200 OK",
            String::new(),
            "<html><body><p id=\"s\">waiting</p><script>setTimeout(function () { document.getElementById('s').textContent = 'ready now'; }, 1500);</script></body></html>".into(),
        ),
        // Same-origin URLs that bounce somewhere else: the redirect hop
        // must go through the policy gate too.
        ("GET", "/bounce") => (
            "302 Found",
            format!("Location: {other_origin}/elsewhere\r\n"),
            String::new(),
        ),
        ("POST", "/bounce-post") => (
            "307 Temporary Redirect",
            format!("Location: {other_origin}/collect\r\n"),
            String::new(),
        ),
        ("GET", "/long") => ("200 OK", String::new(), long_page()),
        _ => {
            let _ = body;
            ("404 Not Found", String::new(), "<html><body>not found</body></html>".into())
        }
    }
}

fn signup_page(other_origin: &str) -> String {
    format!(
        r#"<html><head><title>Sign up</title></head><body>
<h1>Create account</h1>
<form action="/signup" method="post">
  <label for="email">Email</label> <input id="email" name="email" type="email">
  <label for="pw">Password</label> <input id="pw" name="password" type="password">
  <label><input type="checkbox" name="terms"> I agree</label>
  <select name="plan"><option value="free">Free</option><option value="pro" selected>Pro</option></select>
  <button type="submit">Sign up</button>
</form>
<p><a href="{other_origin}/elsewhere">Other origin</a> <a href="/bounce">Bounce</a></p>
<form action="/bounce-post" method="post"><input name="secret" value="s3cret"><button>Send</button></form>
</body></html>"#
    )
}

const TIMERS_PAGE: &str = r#"<html><head><title>Timers</title></head><body>
<p id="status">loading</p>
<p id="rand"></p>
<div id="js-button">Delegated</div>
<script>
  setTimeout(function () { document.getElementById('status').textContent = 'loaded later'; }, 30);
  setInterval(function () { window.ticks = (window.ticks || 0) + 1; }, 1000);
  document.getElementById('rand').textContent = 'r=' + Math.floor(Math.random() * 1e9);
  document.getElementById('js-button').addEventListener('click', function () {
    var ok = confirm('Really delete?');
    console.warn('confirm said ' + ok);
    document.getElementById('status').textContent = ok ? 'deleted' : 'kept';
  });
</script>
</body></html>"#;

fn long_page() -> String {
    let items: String = (1..=80)
        .map(|i| format!("<p><a href=\"/item/{i}\">Item {i}</a></p>"))
        .collect();
    format!(
        "<html><body>{items}<button onclick=\"this.textContent='done'\">Bottom</button></body></html>"
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn browser(approve: ApproveMode) -> AgentBrowser {
    AgentBrowser::new(AgentConfig {
        viewport_width: 800,
        viewport_height: 600,
        approve,
        ..AgentConfig::default()
    })
}

fn find(observation: &Observation, role: Role, name: &str) -> NodeId {
    observation
        .elements
        .iter()
        .find(|e| e.role == role && e.name == name)
        .unwrap_or_else(|| panic!("no {role} named {name:?} in:\n{}", observation.to_text()))
        .id
}

fn path_of(observation: &Observation) -> String {
    let url = observation.url.clone().unwrap_or_default();
    let without_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(&url);
    without_scheme
        .find('/')
        .map(|i| without_scheme[i..].to_string())
        .unwrap_or_else(|| "/".into())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn signup_form_posts_and_follows_the_redirect() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Auto);
    let page = b.navigate(&server.url("/signup")).expect("navigate");
    assert_eq!(page.settled, Some(true));
    assert_eq!(page.title.as_deref(), Some("Sign up"));

    let email = find(&page, Role::Textbox, "Email");
    let password = find(&page, Role::Textbox, "Password");
    let terms = find(&page, Role::Checkbox, "I agree");
    let submit = find(&page, Role::Button, "Sign up");

    let after = b
        .type_text(email, "dev@example.test", false, false)
        .unwrap();
    assert!(after.to_text().contains("value=\"dev@example.test\""));
    assert_eq!(after.changed.as_ref().unwrap().values_changed, vec![email]);

    // 127.0.0.1 + auto-approve: test credentials may be typed.
    let after = b.type_text(password, "hunter22", false, false).unwrap();
    assert!(after.to_text().contains("value=(hidden, 8 chars)"));
    assert!(!after.to_text().contains("hunter22"));

    let after = b.click(terms).unwrap();
    let terms_now = after.elements.iter().find(|e| e.id == terms).unwrap();
    assert_eq!(terms_now.checked, Some(true));

    let done = b.click(submit).unwrap();
    assert_eq!(path_of(&done), "/welcome");
    assert!(done.text.iter().any(|t| t == "Account created"));
    let changed = done.changed.as_ref().unwrap();
    assert_ne!(changed.url_before, changed.url_after);

    let posts = server.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].path, "/signup");
    assert_eq!(
        posts[0].body,
        "email=dev%40example.test&password=hunter22&terms=on&plan=pro"
    );
}

#[test]
fn deny_mode_refuses_the_post_and_remembers_it() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    let page = b.navigate(&server.url("/signup")).unwrap();
    let email = find(&page, Role::Textbox, "Email");
    let submit = find(&page, Role::Button, "Sign up");
    b.type_text(email, "a@b.test", false, false).unwrap();

    match b.click(submit) {
        Err(AgentError::ApprovalDenied {
            remembered,
            observation,
            ..
        }) => {
            assert!(!remembered);
            assert_eq!(path_of(&observation), "/signup", "nothing navigated");
        }
        other => panic!("expected approval_denied, got {other:?}"),
    }
    // Same form through a different route (Enter in the field): refused
    // again without asking.
    match b.type_text(email, "", false, true) {
        Err(AgentError::ApprovalDenied { remembered, .. }) => assert!(remembered),
        other => panic!("expected remembered approval_denied, got {other:?}"),
    }
    assert!(server.posts().is_empty());
}

#[test]
fn password_needs_a_human_unless_local_and_auto_approved() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    let page = b.navigate(&server.url("/signup")).unwrap();
    let password = find(&page, Role::Textbox, "Password");
    let err = b.type_text(password, "secret", false, false).unwrap_err();
    assert_eq!(err.code(), "handoff_required");
}

#[test]
fn cross_origin_link_is_refused_in_deny_mode_and_blocked_by_allow_list() {
    let server = Server::start();

    let mut b = browser(ApproveMode::Deny);
    let page = b.navigate(&server.url("/signup")).unwrap();
    let other = find(&page, Role::Link, "Other origin");
    assert_eq!(b.click(other).unwrap_err().code(), "approval_denied");

    let mut b = AgentBrowser::new(AgentConfig {
        viewport_width: 800,
        viewport_height: 600,
        approve: ApproveMode::Auto,
        allow_origins: vec![server.base.clone()],
        ..AgentConfig::default()
    });
    let page = b.navigate(&server.url("/signup")).unwrap();
    let other = find(&page, Role::Link, "Other origin");
    assert_eq!(b.click(other).unwrap_err().code(), "navigation_blocked");
}

#[test]
fn settle_waits_for_a_short_timer_but_not_a_slow_interval() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    let page = b.navigate(&server.url("/timers")).unwrap();
    assert_eq!(
        page.settled,
        Some(true),
        "a 1s interval must not block settling"
    );
    assert!(
        page.text.iter().any(|t| t == "loaded later"),
        "the 30ms timer must have fired: {:?}",
        page.text
    );
}

#[test]
fn listener_only_elements_are_clickable_and_dialogs_are_reported() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    let page = b.navigate(&server.url("/timers")).unwrap();
    let button = find(&page, Role::Clickable, "Delegated");
    let after = b.click(button).unwrap();
    // confirm() answered false without blocking, and the page saw it.
    assert!(after.text.iter().any(|t| t == "kept"));
    assert_eq!(after.dialogs.len(), 1);
    assert_eq!(after.dialogs[0].kind, "confirm");
    let text = after.to_text();
    assert!(
        text.contains("dialog: confirm \"Really delete?\" → false"),
        "{text}"
    );
    assert!(text.contains("console: 1 warning"), "{text}");
}

#[test]
fn clicking_an_element_below_the_fold_scrolls_it_into_view() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    b.navigate(&server.url("/long")).unwrap();
    let full = b.observe(&ObserveOptions {
        scope: mb_agent::Scope::Full,
        ..ObserveOptions::default()
    });
    let bottom = find(&full, Role::Button, "Bottom");
    let after = b.click(bottom).unwrap();
    assert!(b.state().scroll_offset > 0.0);
    let button = after.elements.iter().find(|e| e.id == bottom).unwrap();
    assert_eq!(button.name, "done");
}

#[test]
fn ids_from_a_previous_page_are_stale() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Auto);
    let page = b.navigate(&server.url("/signup")).unwrap();
    let email = find(&page, Role::Textbox, "Email");
    b.navigate(&server.url("/timers")).unwrap();
    let err = b.click(email).unwrap_err();
    assert_eq!(err.code(), "stale_element");
    assert!(err.message().contains("previous page"), "{}", err.message());
}

#[test]
fn tab_moves_focus_through_the_form() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    let page = b.navigate(&server.url("/signup")).unwrap();
    let email = find(&page, Role::Textbox, "Email");
    let password = find(&page, Role::Textbox, "Password");
    b.press("Tab").unwrap();
    assert_eq!(b.state().focused_element(), Some(email));
    b.press("Tab").unwrap();
    assert_eq!(b.state().focused_element(), Some(password));
    b.press("Escape").unwrap();
    assert_eq!(b.state().focused_element(), None);
    assert_eq!(b.press("F5").unwrap_err().code(), "unsupported_key");
}

#[test]
fn same_actions_give_identical_observations() {
    let run = || {
        let server = Server::start();
        let mut b = browser(ApproveMode::Deny);
        let page = b.navigate(&server.url("/timers")).unwrap();
        let button = find(&page, Role::Clickable, "Delegated");
        let after = b.click(button).unwrap();
        // The port differs per server; strip it so the comparison is about
        // the page, not the fixture.
        after.to_text().replace(&server.base, "http://fixture")
    };
    let a = run();
    let b = run();
    assert_eq!(a, b);
    assert!(
        a.contains("r="),
        "seeded Math.random output is part of the page"
    );
}

#[test]
fn a_redirect_to_another_origin_is_gated_like_a_direct_link() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    let page = b.navigate(&server.url("/signup")).unwrap();
    let bounce = find(&page, Role::Link, "Bounce");
    match b.click(bounce) {
        Err(AgentError::ApprovalDenied { observation, .. }) => {
            // Refused hop: the page stays put, no error page.
            assert_eq!(path_of(&observation), "/signup");
            assert_eq!(observation.title.as_deref(), Some("Sign up"));
        }
        other => panic!("expected approval_denied, got {other:?}"),
    }
    let paths = server.paths();
    assert!(paths.contains(&"GET /bounce".to_string()));
    assert!(
        !paths.contains(&"GET /elsewhere".to_string()),
        "the refused hop must never be fetched: {paths:?}"
    );
}

#[test]
fn allow_list_also_blocks_redirect_hops_and_keeps_post_bodies_home() {
    let server = Server::start();
    let mut b = AgentBrowser::new(AgentConfig {
        viewport_width: 800,
        viewport_height: 600,
        approve: ApproveMode::Auto,
        allow_origins: vec![server.base.clone()],
        ..AgentConfig::default()
    });
    let page = b.navigate(&server.url("/signup")).unwrap();

    let bounce = find(&page, Role::Link, "Bounce");
    assert_eq!(b.click(bounce).unwrap_err().code(), "navigation_blocked");

    // POST to a same-origin URL is approved (auto), but its 307 would
    // carry the body to another origin: that hop is blocked.
    let page = b.observe(&ObserveOptions::default());
    let send = find(&page, Role::Button, "Send");
    assert_eq!(b.click(send).unwrap_err().code(), "navigation_blocked");
    let posts = server.posts();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].path, "/bounce-post");
    assert!(!server.paths().contains(&"POST /collect".to_string()));
}

#[test]
fn wait_for_lets_slow_timers_fire_and_times_out_otherwise() {
    use std::time::Duration;
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    let page = b.navigate(&server.url("/delayed")).unwrap();
    // 1.5s is far beyond settle's horizon: navigate returns before it fires.
    assert!(page.text.iter().any(|t| t == "waiting"));
    let ready = b
        .wait_for(
            mb_agent::WaitCondition::Text("ready now".into()),
            Duration::from_secs(3),
        )
        .unwrap();
    assert!(ready.text.iter().any(|t| t == "ready now"));
    let err = b
        .wait_for(
            mb_agent::WaitCondition::Text("never".into()),
            Duration::from_millis(200),
        )
        .unwrap_err();
    assert_eq!(err.code(), "wait_timeout");
}

#[test]
fn screenshot_renders_the_page_area_without_chrome() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    b.navigate(&server.url("/signup")).unwrap();
    let shot = b.screenshot();
    assert_eq!((shot.width, shot.height), (800, 600));
    assert_eq!(shot.pixels.len(), 800 * 600);
    let non_white = shot
        .pixels
        .iter()
        .filter(|&&p| p & 0x00FF_FFFF != 0x00FF_FFFF)
        .count();
    assert!(
        non_white > 100,
        "the form should paint something ({non_white} px)"
    );
}

#[test]
fn wait_for_never_turns_a_guessed_or_old_id_into_a_usable_one() {
    use std::time::Duration;
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    let page = b.navigate(&server.url("/signup")).unwrap();
    let email = find(&page, Role::Textbox, "Email");
    b.navigate(&server.url("/timers")).unwrap();
    // The old id may well name a rendered node on the new page; waiting on
    // it must not launder it.
    let err = b
        .wait_for(
            mb_agent::WaitCondition::Element(email),
            Duration::from_millis(100),
        )
        .unwrap_err();
    assert_eq!(err.code(), "stale_element");
    assert_eq!(b.click(email).unwrap_err().code(), "stale_element");
}
