// `mb-mcp --http`: the MCP server over streamable HTTP (docs/concept.md
// "HTTP 보안", G3b). The person starts the browser; an agent attaches to
// it at `http://127.0.0.1:<port>/mcp` instead of spawning its own.
//
// A page open in *any* browser on this machine can send requests to a
// loopback port (and DNS rebinding can make them look same-origin), so:
//   - bind 127.0.0.1 only,
//   - every request needs `Authorization: Bearer <token>` (random per
//     launch, or `MB_MCP_TOKEN` for a stable registration),
//   - a request with an `Origin` header is refused (MCP clients aren't
//     browsers), and rmcp refuses any `Host` other than
//     `127.0.0.1:<port>` / `localhost:<port>`.
//
// All clients share the one browser; their tool calls run one at a time
// (the worker queue), exactly like stdio.

use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode, header};
use hyper_util::rt::TokioIo;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

use crate::server::BrowserServer;

pub const PATH: &str = "/mcp";
pub const TOKEN_ENV: &str = "MB_MCP_TOKEN";
const MIN_TOKEN_LEN: usize = 32;

/// Where an agent attaches, and with what.
#[derive(Clone)]
pub struct Attach {
    pub url: String,
    pub token: String,
}

impl std::fmt::Debug for Attach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attach")
            .field("url", &self.url)
            .field("token", &masked(&self.token))
            .finish()
    }
}

impl Attach {
    /// The command that registers this browser with Claude Code.
    pub fn setup_command(&self) -> String {
        format!(
            "claude mcp add --transport http mini-browser {} --header \"Authorization: Bearer {}\"",
            self.url, self.token
        )
    }
}

/// `MB_MCP_TOKEN` if set (so a registration survives restarts), otherwise
/// 32 random bytes as hex.
pub fn token_from_env_or_random() -> Result<String, String> {
    if let Ok(token) = std::env::var(TOKEN_ENV) {
        let token = token.trim().to_string();
        if token.len() < MIN_TOKEN_LEN || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(format!(
                "{TOKEN_ENV} must be at least {MIN_TOKEN_LEN} printable ASCII characters"
            ));
        }
        return Ok(token);
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("random token: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `3f2a…9c1b`: enough to tell tokens apart on screen.
pub fn masked(token: &str) -> String {
    if token.len() <= 8 {
        return "…".into();
    }
    format!("{}…{}", &token[..4], &token[token.len() - 4..])
}

type Body = BoxBody<Bytes, Infallible>;

/// Accept connections until the listener fails; each request passes
/// [`gate`] before reaching rmcp.
pub async fn serve(
    listener: tokio::net::TcpListener,
    token: String,
    make_server: impl Fn() -> BrowserServer + Send + Sync + 'static,
) -> std::io::Result<()> {
    let port = listener.local_addr()?.port();
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts([format!("127.0.0.1:{port}"), format!("localhost:{port}")])
        .enforce_origin_validation();
    let service = StreamableHttpService::new(
        move || Ok(make_server()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let token: Arc<str> = token.into();
    loop {
        let (stream, _) = listener.accept().await?;
        let (service, token) = (service.clone(), token.clone());
        tokio::spawn(async move {
            let handler = hyper::service::service_fn(move |request: Request<Incoming>| {
                let (service, token) = (service.clone(), token.clone());
                async move {
                    Ok::<Response<Body>, Infallible>(match gate(&request, &token) {
                        Err(refusal) => *refusal,
                        Ok(()) => service.handle(request).await,
                    })
                }
            });
            if let Err(error) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), handler)
                .await
            {
                eprintln!("[mb-mcp] http connection: {error}");
            }
        });
    }
}

/// Path, `Origin` and token checks. (`Host` is rmcp's, configured above.)
fn gate<B>(request: &Request<B>, token: &str) -> Result<(), Box<Response<Body>>> {
    if request.uri().path() != PATH {
        return Err(Box::new(refuse(
            StatusCode::NOT_FOUND,
            "not found: the endpoint is /mcp",
        )));
    }
    if request.headers().contains_key(header::ORIGIN) {
        // A web page, not an MCP client.
        eprintln!("[mb-mcp] refused a request with an Origin header");
        return Err(Box::new(refuse(
            StatusCode::FORBIDDEN,
            "requests from web pages are refused",
        )));
    }
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match presented {
        Some(presented) if same_secret(presented.trim(), token) => Ok(()),
        _ => {
            eprintln!("[mb-mcp] refused a request without the right token");
            let mut response = refuse(
                StatusCode::UNAUTHORIZED,
                "missing or wrong bearer token (shown in the window / on stderr at launch)",
            );
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                header::HeaderValue::from_static("Bearer"),
            );
            Err(Box::new(response))
        }
    }
}

// Constant-time in the token's length: no early exit on the first
// differing byte.
fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn refuse(status: StatusCode, message: &'static str) -> Response<Body> {
    let mut response = Response::new(Full::new(Bytes::from_static(message.as_bytes())).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn request(path: &str, headers: &[(&str, &str)]) -> Request<()> {
        let mut builder = Request::builder().uri(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(()).unwrap()
    }

    fn status(result: Result<(), Box<Response<Body>>>) -> Option<StatusCode> {
        result.err().map(|r| r.status())
    }

    #[test]
    fn needs_the_token_on_the_mcp_path_and_no_origin() {
        let bearer = format!("Bearer {TOKEN}");
        let ok = request("/mcp", &[("authorization", &bearer)]);
        assert!(gate(&ok, TOKEN).is_ok());

        let none = request("/mcp", &[]);
        assert_eq!(status(gate(&none, TOKEN)), Some(StatusCode::UNAUTHORIZED));
        let wrong = request(
            "/mcp",
            &[("authorization", "Bearer 0123456789abcdef0123456789abcdeX")],
        );
        assert_eq!(status(gate(&wrong, TOKEN)), Some(StatusCode::UNAUTHORIZED));
        let basic = request("/mcp", &[("authorization", TOKEN)]);
        assert_eq!(status(gate(&basic, TOKEN)), Some(StatusCode::UNAUTHORIZED));

        // A web page that learned the token still can't use it.
        let page = request(
            "/mcp",
            &[
                ("authorization", &bearer),
                ("origin", "http://evil.example"),
            ],
        );
        assert_eq!(status(gate(&page, TOKEN)), Some(StatusCode::FORBIDDEN));

        let elsewhere = request("/", &[("authorization", &bearer)]);
        assert_eq!(status(gate(&elsewhere, TOKEN)), Some(StatusCode::NOT_FOUND));
    }

    #[test]
    fn tokens_are_compared_whole_and_shown_masked() {
        assert!(same_secret(TOKEN, TOKEN));
        assert!(!same_secret(TOKEN, &TOKEN[..31]));
        assert!(!same_secret("", TOKEN));
        assert_eq!(masked(TOKEN), "0123…cdef");
        let attach = Attach {
            url: "http://127.0.0.1:7333/mcp".into(),
            token: TOKEN.into(),
        };
        assert!(!format!("{attach:?}").contains(TOKEN));
        assert!(attach.setup_command().contains(&format!("Bearer {TOKEN}")));
    }

    #[test]
    fn random_tokens_are_long_and_differ() {
        // (MB_MCP_TOKEN is not set in the test environment.)
        if std::env::var(TOKEN_ENV).is_ok() {
            return;
        }
        let (a, b) = (
            token_from_env_or_random().unwrap(),
            token_from_env_or_random().unwrap(),
        );
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }
}
