//! Opt-in record/replay cache for real-site compatibility surveys.
//!
//! `MB_NET_CACHE=DIR` makes every fetch look in `DIR` first and store what it
//! fetched there, so a survey can run the engine over a real page many times
//! while the site sees one visit. `MB_NET_CACHE_OFFLINE=1` turns a miss into
//! an error instead of a fetch (replay only, never touch the network).
//!
//! File names are a 64-bit hash of the request, so two requests can share a
//! name; each entry also stores the full request identity and only an exact
//! match is a hit (a mismatch is a miss, or an error offline). Without that a
//! colliding — or crafted — request would be served another one's response.
//!
//! The cache holds third-party content, so `DIR` must stay out of the
//! repository (`compat-cache/` is git-ignored). Nothing here runs unless the
//! variable is set: normal browsing and every test go straight to the wire.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use super::{FetchResult, HttpResponse, NetworkError, Url};

pub(super) struct Cache {
    dir: PathBuf,
    offline: bool,
}

impl Cache {
    pub(super) fn from_env() -> Option<Self> {
        let dir = std::env::var_os("MB_NET_CACHE")?;
        let offline = std::env::var_os("MB_NET_CACHE_OFFLINE").is_some();
        Some(Self {
            dir: PathBuf::from(dir),
            offline,
        })
    }

    pub(super) fn fetch(
        &self,
        key_parts: (&Url, &str, bool, &[u8]),
        fetch: impl FnOnce() -> Result<FetchResult, NetworkError>,
    ) -> Result<FetchResult, NetworkError> {
        let (url, method, follows_redirects, body) = key_parts;
        let url_text = url.to_string();
        let identity = identity_line(&url_text, method, follows_redirects, body);
        let key = cache_key(&url_text, method, follows_redirects, body);
        let meta_path = self.dir.join(format!("{key}.meta"));
        let body_path = self.dir.join(format!("{key}.body"));
        if let Some(hit) = read_entry(&meta_path, &body_path, &identity) {
            return Ok(hit);
        }
        if self.offline {
            return Err(NetworkError::Io(format!("not in MB_NET_CACHE: {url}")));
        }
        let result = fetch()?;
        // A failed write only costs a refetch next time; never fail the load.
        let _ = self.write_entry(&meta_path, &body_path, &identity, url, &result);
        Ok(result)
    }

    fn write_entry(
        &self,
        meta_path: &Path,
        body_path: &Path,
        identity: &str,
        url: &Url,
        result: &FetchResult,
    ) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let response = &result.response;
        let mut meta = format!(
            "{identity}\n{}\n{}\n",
            response.status_code, result.final_url
        );
        for (name, value) in &response.headers {
            meta.push_str(&format!("{name}\t{value}\n"));
        }
        fs::write(body_path, &response.body)?;
        fs::write(meta_path, meta)?;
        let content_type = response.header("content-type").unwrap_or("-");
        let mut index = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("index.tsv"))?;
        // One write per line: fetches run on parallel threads.
        let line = format!(
            "{}\t{}\t{}\t{}\n",
            response.status_code,
            content_type,
            response.body.len(),
            url
        );
        index.write_all(line.as_bytes())
    }
}

/// The `.meta` file's first line: everything that identifies the request,
/// compared byte for byte on read. The body is hex so it stays on one line.
fn identity_line(url: &str, method: &str, follows_redirects: bool, body: &[u8]) -> String {
    let flag = if follows_redirects { "F" } else { "N" };
    let body_hex: String = body.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("identity\t{method}\t{flag}\t{url}\t{body_hex}")
}

fn read_entry(meta_path: &Path, body_path: &Path, identity: &str) -> Option<FetchResult> {
    let meta = fs::read_to_string(meta_path).ok()?;
    let mut lines = meta.lines();
    // Another request that hashed to the same file name, or an entry from
    // before identities were stored: not ours.
    if lines.next()? != identity {
        return None;
    }
    let body = fs::read(body_path).ok()?;
    let status_code: u16 = lines.next()?.parse().ok()?;
    let final_url = Url::parse(lines.next()?).ok()?;
    let headers = lines
        .filter_map(|line| line.split_once('\t'))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    Some(FetchResult {
        final_url,
        response: HttpResponse {
            status_code,
            reason_phrase: String::new(),
            headers,
            body,
        },
    })
}

/// FNV-1a over the request identity. Stable across runs and Rust versions
/// (unlike `DefaultHasher`), which is all a file name needs.
fn cache_key(url: &str, method: &str, follows_redirects: bool, body: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let flag = if follows_redirects { b"F" } else { b"N" };
    for part in [method.as_bytes(), flag, url.as_bytes(), body] {
        for byte in part.iter().chain(std::iter::once(&0u8)) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{Cache, FetchResult, HttpResponse, NetworkError, Url, cache_key};

    fn response(body: &str) -> Result<FetchResult, NetworkError> {
        Ok(FetchResult {
            final_url: Url::parse("https://a.test/").unwrap(),
            response: HttpResponse {
                status_code: 200,
                reason_phrase: String::new(),
                headers: vec![("content-type".into(), "text/plain".into())],
                body: body.as_bytes().to_vec(),
            },
        })
    }

    fn temp_cache(name: &str, offline: bool) -> Cache {
        let dir = std::env::temp_dir().join(format!("mb-net-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Cache { dir, offline }
    }

    #[test]
    fn hit_requires_the_stored_request_identity_to_match() {
        let url = Url::parse("https://a.test/page").unwrap();
        let other = Url::parse("https://evil.test/other").unwrap();
        let cache = temp_cache("identity", false);

        // Record `other`, then move its files to the name `url` hashes to —
        // what a hash collision (or a crafted request) would look like.
        let _ = cache.fetch((&other, "GET", true, b""), || response("other's body"));
        let other_key = cache_key(&other.to_string(), "GET", true, b"");
        let key = cache_key(&url.to_string(), "GET", true, b"");
        for ext in ["meta", "body"] {
            std::fs::rename(
                cache.dir.join(format!("{other_key}.{ext}")),
                cache.dir.join(format!("{key}.{ext}")),
            )
            .unwrap();
        }

        let fetched = Cell::new(false);
        let result = cache
            .fetch((&url, "GET", true, b""), || {
                fetched.set(true);
                response("real body")
            })
            .unwrap();
        assert!(fetched.get(), "a mismatched entry must be a miss");
        assert_eq!(result.response.body, b"real body");

        // Now recorded under its own identity: a hit, no fetch.
        let again = cache
            .fetch((&url, "GET", true, b""), || panic!("should be cached"))
            .unwrap();
        assert_eq!(again.response.body, b"real body");

        // Offline, a mismatch is an error rather than someone else's response:
        // copy `url`'s entry to a third request's file name and replay it.
        let third = Url::parse("https://a.test/third").unwrap();
        let third_key = cache_key(&third.to_string(), "GET", true, b"");
        for ext in ["meta", "body"] {
            std::fs::copy(
                cache.dir.join(format!("{key}.{ext}")),
                cache.dir.join(format!("{third_key}.{ext}")),
            )
            .unwrap();
        }
        let offline = Cache {
            dir: cache.dir.clone(),
            offline: true,
        };
        assert!(
            offline
                .fetch((&third, "GET", true, b""), || panic!(
                    "offline never fetches"
                ))
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&cache.dir);
    }

    #[test]
    fn cache_key_separates_method_redirect_mode_and_body() {
        let base = cache_key("https://a.test/", "GET", true, b"");
        assert_eq!(base, cache_key("https://a.test/", "GET", true, b""));
        assert_ne!(base, cache_key("https://a.test/", "POST", true, b""));
        assert_ne!(base, cache_key("https://a.test/", "GET", false, b""));
        assert_ne!(base, cache_key("https://a.test/", "GET", true, b"x"));
        assert_ne!(base, cache_key("https://a.test/x", "GET", true, b""));
    }
}
