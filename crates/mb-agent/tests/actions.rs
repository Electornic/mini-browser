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
        ("GET", "/adder") => (
            "200 OK",
            String::new(),
            "<html><body><div id=\"list\"></div><button onclick=\"var c = document.createElement('input'); c.setAttribute('type', 'checkbox'); document.getElementById('list').appendChild(c);\">Add</button></body></html>".into(),
        ),
        ("GET", "/props") => ("200 OK", String::new(), PROPS_PAGE.into()),
        ("GET", "/router") => ("200 OK", String::new(), ROUTER_PAGE.into()),
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
        ("GET", demo) if demo.starts_with("/demo/") => demo_file(&demo["/demo/".len()..]),
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

// Builds its controls the way frameworks do — IDL property assignment, no
// setAttribute — and reads state back through properties.
const PROPS_PAGE: &str = r#"<html><body><div id="root"></div><p id="out">-</p><script>
var root = document.getElementById('root');
var box = document.createElement('input');
box.type = 'checkbox';
box.id = 'agree';
box.checked = true;
var label = document.createElement('label');
label.htmlFor = 'agree';
label.textContent = 'I agree';
var off = document.createElement('button');
off.textContent = 'Locked';
off.disabled = true;
root.appendChild(box);
root.appendChild(label);
root.appendChild(off);
box.addEventListener('change', function () {
  document.getElementById('out').textContent =
    'type=' + box.type + ' checked=' + box.checked + ' disabled=' + off.disabled;
});
</script></body></html>"#;

// A hash router plus a pushState link: in-memory state (`clicks`) must
// survive both, which only holds if neither reloads the document.
const ROUTER_PAGE: &str = r##"<html><body><nav><a href="#/done">Done</a> <a href="#/all">All</a> <a id="push" href="/router/about">About</a></nav>
<button id="bump">Bump</button><p id="view">-</p><script>
var clicks = 0, events = [];
function show() {
  document.getElementById('view').textContent =
    'route=' + (location.hash || '#/all') + ' path=' + location.pathname + ' clicks=' + clicks + ' events=' + events.join(',');
}
document.getElementById('bump').addEventListener('click', function () { clicks++; show(); });
document.getElementById('push').addEventListener('click', function (e) {
  e.preventDefault(); history.pushState({}, '', '/router/about'); events.push('push'); show();
});
window.addEventListener('popstate', function () { events.push('pop'); show(); });
window.addEventListener('hashchange', function () { events.push('hash'); show(); });
show();
</script></body></html>"##;

/// Serves `examples/agent-demo/<name>` as is — the demo pages double as
/// capstone fixtures.
fn demo_file(name: &str) -> (&'static str, String, String) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/agent-demo/");
    match std::fs::read_to_string(format!("{dir}{name}")) {
        Ok(body) if !name.contains("..") => ("200 OK", String::new(), body),
        _ => ("404 Not Found", String::new(), String::new()),
    }
}

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
    let changed = after.changed.as_ref().unwrap();
    assert!(!changed.new_document);
    assert_eq!(
        changed
            .values_changed
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>(),
        vec![email]
    );
    // Before → after, in the untrusted page block (it's a page string).
    let (trusted, page_block) = after.to_blocks();
    let detail = format!("e{} value \"\" → \"dev@example.test\"", email.raw());
    assert!(page_block.contains(&detail), "{page_block}");
    assert!(
        !trusted.contains("dev@example.test"),
        "values stay out of the trusted block"
    );

    // 127.0.0.1 + auto-approve: test credentials may be typed.
    let after = b.type_text(password, "hunter22", false, false).unwrap();
    assert!(after.to_text().contains("value=(hidden, 8 chars)"));
    assert!(!after.to_text().contains("hunter22"));

    let after = b.click(terms).unwrap();
    let terms_now = after.elements.iter().find(|e| e.id == terms).unwrap();
    assert_eq!(terms_now.checked, Some(true));
    assert!(
        after
            .to_text()
            .contains(&format!("e{} checked false → true", terms.raw()))
    );

    let done = b.click(submit).unwrap();
    assert_eq!(path_of(&done), "/welcome");
    assert!(done.text.iter().any(|t| t == "Account created"));
    let changed = done.changed.as_ref().unwrap();
    assert_ne!(changed.url_before, changed.url_after);
    // Landing on another document: flagged, and no cross-document diff.
    assert!(changed.new_document);
    assert!(changed.values_changed.is_empty());
    let text = done.to_text();
    assert!(
        text.contains("new document: element ids were re-issued"),
        "{text}"
    );
    assert!(!text.contains("changes:"), "{text}");

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
    let changed = after.changed.as_ref().unwrap();
    assert_eq!(changed.text_added, vec!["kept".to_string()]);
    assert_eq!(changed.text_removed, vec!["loaded later".to_string()]);
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
fn the_screenshot_is_the_same_whatever_the_window_theme() {
    use mb_runtime::palette::{Theme, set_theme};
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    b.navigate(&server.url("/signup")).unwrap();
    let light = b.screenshot().pixels;
    // The observation window paints dark on this thread…
    set_theme(Theme::Dark);
    let dark = b.screenshot().pixels;
    // …and the screenshot didn't leave it light.
    assert_eq!(mb_runtime::palette::theme(), Theme::Dark);
    set_theme(Theme::Light);
    assert!(
        light == dark,
        "the agent's pixels follow the person's theme"
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

#[test]
fn ask_mode_holds_the_post_until_a_person_approves() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Ask);
    let page = b.navigate(&server.url("/signup")).unwrap();
    let email = find(&page, Role::Textbox, "Email");
    let submit = find(&page, Role::Button, "Sign up");
    b.type_text(email, "dev@example.test", false, false)
        .unwrap();

    let err = b.click(submit).unwrap_err();
    assert_eq!(err.code(), "approval_pending");
    assert!(err.message().contains("POST"), "{}", err.message());
    assert!(b.pending_approval().unwrap().starts_with("POST "));
    assert!(server.posts().is_empty(), "nothing is sent while held");
    // While waiting, other actions are refused rather than queued.
    assert_eq!(b.scroll(100.0).unwrap_err().code(), "approval_pending");

    let done = b.resolve_approval(true).unwrap();
    assert_eq!(path_of(&done), "/welcome");
    assert!(done.changed.as_ref().unwrap().new_document);
    assert_eq!(server.posts().len(), 1);
    assert!(b.pending_approval().is_none());
    assert_eq!(
        b.resolve_approval(true).unwrap_err().code(),
        "nothing_pending"
    );
}

#[test]
fn ask_mode_refusal_is_reported_and_remembered() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Ask);
    let page = b.navigate(&server.url("/signup")).unwrap();
    let submit = find(&page, Role::Button, "Sign up");
    assert_eq!(b.click(submit).unwrap_err().code(), "approval_pending");

    match b.resolve_approval(false) {
        Err(AgentError::ApprovalDenied {
            remembered,
            observation,
            ..
        }) => {
            assert!(!remembered);
            assert_eq!(path_of(&observation), "/signup");
        }
        other => panic!("expected approval_denied, got {other:?}"),
    }
    // Asking again for the same thing doesn't bother the person twice.
    match b.click(submit) {
        Err(AgentError::ApprovalDenied { remembered, .. }) => assert!(remembered),
        other => panic!("expected remembered refusal, got {other:?}"),
    }
    assert!(b.pending_approval().is_none());
    assert!(server.posts().is_empty());
}

#[test]
fn a_page_the_person_changed_is_flagged_once() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    b.navigate(&server.url("/signup")).unwrap();
    let welcome = b.navigate(&server.url("/welcome")).unwrap();
    assert!(!welcome.changed_by_person, "the agent's own navigation");
    let stale = find(&welcome, Role::Link, "Back");

    // The person presses back in the window between two tool calls.
    b.human_frame(&mb_runtime::input::WindowInput {
        back_pressed: true,
        ..Default::default()
    });
    b.settle();

    // The next tool call says so — its ids from before are gone.
    let err = b.click(stale).unwrap_err();
    assert_eq!(err.code(), "stale_element");
    assert!(
        err.message().contains("The person changed the page"),
        "{}",
        err.message()
    );
    let now = b.observe(&Default::default());
    assert_eq!(path_of(&now), "/signup");
    assert!(now.changed_by_person);
    assert!(now.to_text().contains("the person used the window"));
    // Only once: after that the agent has seen the page.
    assert!(!b.observe(&Default::default()).changed_by_person);
}

#[test]
fn changed_names_the_elements_that_appeared() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    let page = b.navigate(&server.url("/adder")).unwrap();
    let add = find(&page, Role::Button, "Add");
    let after = b.click(add).unwrap();
    let changed = after.changed.as_ref().unwrap();
    assert_eq!(changed.elements_added, 1);
    let new = changed.added_ids[0];
    assert!(
        after
            .elements
            .iter()
            .any(|e| e.id == new && e.role == Role::Checkbox)
    );
    let text = after.to_text();
    assert!(
        text.contains(&format!("+1 (e{}) / -0 elements", new.raw())),
        "{text}"
    );
}

#[test]
fn controls_built_with_property_setters_render_and_toggle() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    let page = b.navigate(&server.url("/props")).unwrap();
    let text = page.to_text();
    assert!(
        text.contains("checkbox  name=\"I agree\" checked=true"),
        "{text}"
    );
    assert!(text.contains("name=\"Locked\"  (disabled)"), "{text}");
    let after = b.click(find(&page, Role::Checkbox, "I agree")).unwrap();
    assert!(
        after
            .text
            .iter()
            .any(|t| t == "type=checkbox checked=false disabled=true"),
        "{}",
        after.to_text()
    );
}

#[test]
fn hash_links_and_push_state_stay_in_the_same_document() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    let page = b.navigate(&server.url("/router")).unwrap();
    let page = b.click(find(&page, Role::Button, "Bump")).unwrap();
    let view = |o: &Observation| {
        o.text
            .iter()
            .find(|t| t.starts_with("route="))
            .cloned()
            .unwrap()
    };
    assert_eq!(view(&page), "route=#/all path=/router clicks=1 events=");

    // A fragment link: no reload (clicks survives), popstate + hashchange.
    let done = b.click(find(&page, Role::Link, "Done")).unwrap();
    assert_eq!(
        view(&done),
        "route=#/done path=/router clicks=1 events=pop,hash"
    );
    assert_eq!(
        done.url.as_deref(),
        Some(server.url("/router#/done").as_str())
    );
    assert!(
        !done.to_text().contains("new document"),
        "{}",
        done.to_text()
    );

    // pushState: the trusted url follows the page's same-origin URL.
    let about = b.click(find(&done, Role::Link, "About")).unwrap();
    assert_eq!(
        view(&about),
        "route=#/all path=/router/about clicks=1 events=pop,hash,push"
    );
    assert_eq!(
        about.url.as_deref(),
        Some(server.url("/router/about").as_str())
    );

    // Back walks the same-document entries first, still without a reload.
    let back = b.back().unwrap();
    assert_eq!(
        view(&back),
        "route=#/done path=/router clicks=1 events=pop,hash,push,pop"
    );
    let back = b.back().unwrap();
    assert_eq!(
        view(&back),
        "route=#/all path=/router clicks=1 events=pop,hash,push,pop,pop,hash"
    );
    assert_eq!(back.url.as_deref(), Some(server.url("/router").as_str()));
    let fwd = b.forward().unwrap();
    assert_eq!(
        fwd.url.as_deref(),
        Some(server.url("/router#/done").as_str())
    );
}

/// Capstone for SPA Tier 1.5: the Preact + hooks demo (classic UMD bundles,
/// no build) driven end to end with agent actions — controlled input,
/// keyed list, checkbox props, a hash router, and back through it.
#[test]
fn preact_demo_adds_toggles_filters_and_goes_back() {
    let server = Server::start();
    let mut b = browser(ApproveMode::Deny);
    let page = b.navigate(&server.url("/demo/preact.html")).unwrap();
    assert!(
        page.text.iter().any(|t| t == "1 of 2 done"),
        "{}",
        page.to_text()
    );

    let page = b
        .type_text(
            find(&page, Role::Textbox, "New item"),
            "buy milk",
            false,
            true,
        )
        .unwrap();
    let page = b.click(find(&page, Role::Checkbox, "buy milk")).unwrap();
    assert!(
        page.text.iter().any(|t| t == "2 of 3 done"),
        "{}",
        page.to_text()
    );
    assert!(page.console.is_empty(), "{}", page.to_text());

    let done = b.click(find(&page, Role::Link, "Done")).unwrap();
    assert!(
        done.url
            .as_deref()
            .unwrap()
            .ends_with("/demo/preact.html#/done")
    );
    let names: Vec<_> = done
        .elements
        .iter()
        .filter(|e| e.role == Role::Checkbox)
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, ["Read the concept doc", "buy milk"]);

    let all = b.back().unwrap();
    assert!(all.url.as_deref().unwrap().ends_with("/demo/preact.html"));
    assert_eq!(
        all.elements
            .iter()
            .filter(|e| e.role == Role::Checkbox)
            .count(),
        3,
        "back to the unfiltered list, state kept: {}",
        all.to_text()
    );
}
