// `Url` is the toy's flat 4-field representation of an absolute URL. It lives
// in mb-dom so the engine layer (which paints `Image` commands keyed by URL)
// can refer to the type without depending on the runtime's network stack.
// Parsing and resolution wrap the `url` crate.
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
}

// Shared error enum for URL parsing + (separately) HTTP fetching. The
// non-parse variants are produced from `mb-runtime::net` once the workspace
// split lands; keeping the type in mb-dom avoids a `mb_runtime::NetworkError`
// dependency hop for the engine layer.
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
    pub(crate) fn from_url(parsed: &url::Url) -> Result<Self, NetworkError> {
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
