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
//! The identity is length-prefixed fields, never delimited text, so no field
//! value (a page-chosen fetch method, a URL, a body) can pose as another
//! field; the method must also be a valid HTTP token before the cache looks.
//!
//! Only `GET` ever reaches the network while the cache is on: a survey runs
//! real page scripts, and their beacons, `POST`s and `fetch(method≠GET)`
//! would send fake impressions and form traffic to third parties. Those
//! requests fail locally instead (always — there is no switch; the cache is
//! survey-only, so normal browsing is unaffected). Some script paths change
//! because of it, which a survey accepts (docs/compat-round3.md 0.2).
//!
//! The cache holds third-party content, so `DIR` must stay out of the
//! repository (`compat-cache/` is git-ignored). Nothing here runs unless the
//! variable is set: normal browsing and every test go straight to the wire.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use super::{FetchResult, HttpResponse, NetworkError, RequestKind, Url};

pub(super) struct Cache {
    dir: PathBuf,
    offline: bool,
}

/// What the cache refused this process, for the survey probe's report
/// (`compat_probe` reads it once at the end; nothing in the browser does).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SurveyCacheStats {
    /// `METHOD url` (query kept: the probe classifies by path) of each
    /// offline replay that had no entry.
    pub offline_misses: Vec<String>,
    /// The same misses with what each request was for, in the same order
    /// (compat round 4 P7: the re-fetch filter needs the kind).
    pub misses: Vec<SurveyMiss>,
    /// Requests with a method other than `GET`, failed without sending.
    pub refused_non_get: usize,
}

/// One offline miss: the request's kind and URL (query kept).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurveyMiss {
    pub kind: RequestKind,
    pub url: Url,
}

static STATS: Mutex<SurveyCacheStats> = Mutex::new(SurveyCacheStats {
    offline_misses: Vec::new(),
    misses: Vec::new(),
    refused_non_get: 0,
});

/// Snapshot of [`SurveyCacheStats`] since the process started.
pub fn survey_stats() -> SurveyCacheStats {
    STATS.lock().map(|stats| stats.clone()).unwrap_or_default()
}

fn note(update: impl FnOnce(&mut SurveyCacheStats)) {
    if let Ok(mut stats) = STATS.lock() {
        update(&mut stats);
    }
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
        kind: RequestKind,
        key_parts: (&Url, &str, bool, &[u8]),
        fetch: impl FnOnce() -> Result<FetchResult, NetworkError>,
    ) -> Result<FetchResult, NetworkError> {
        let (url, method, follows_redirects, body) = key_parts;
        // Rejected here exactly as the wire request would be: a method that
        // isn't an HTTP token never reaches the cache.
        let method = normalize_method(method)?;
        if method != "GET" {
            note(|stats| stats.refused_non_get += 1);
            return Err(NetworkError::Io(format!(
                "MB_NET_CACHE: survey sends GET only, {method} {url} not sent"
            )));
        }
        let url_text = url.to_string();
        let identity = identity_line(&url_text, &method, follows_redirects, body);
        let key = cache_key(&url_text, &method, follows_redirects, body);
        let meta_path = self.dir.join(format!("{key}.meta"));
        let body_path = self.dir.join(format!("{key}.body"));
        if let Some(hit) = read_entry(&meta_path, &body_path, &identity) {
            return Ok(hit);
        }
        if self.offline {
            note(|stats| {
                stats.offline_misses.push(format!("{method} {url}"));
                stats.misses.push(SurveyMiss {
                    kind,
                    url: url.clone(),
                });
            });
            return Err(NetworkError::Io(format!("not in MB_NET_CACHE: {url}")));
        }
        let result = fetch()?;
        // A failed write only costs a refetch next time; never fail the load.
        let _ = self.write_entry(&meta_path, &body_path, &identity, url, &result);
        Ok(result)
    }

    /// A targeted re-fetch (compat round 4 S): `fetch` goes to the network
    /// whatever the cache holds, and only a 200 is written (replacing the
    /// entry). Any other status leaves the cache as it was — an old entry
    /// stays, a miss stays a miss. Returns (status, stored).
    pub(super) fn refetch(
        &self,
        kind: RequestKind,
        url: &Url,
        fetch: impl FnOnce() -> Result<FetchResult, NetworkError>,
    ) -> Result<(u16, bool), NetworkError> {
        if self.offline {
            return Err(NetworkError::Io(
                "MB_NET_CACHE_OFFLINE is set: a re-fetch needs the network".into(),
            ));
        }
        let follows_redirects = kind != RequestKind::Document;
        let url_text = url.to_string();
        let identity = identity_line(&url_text, "GET", follows_redirects, b"");
        let key = survey_cache_key(kind, url);
        let result = fetch()?;
        let status = result.response.status_code;
        if status != 200 {
            return Ok((status, false));
        }
        let meta_path = self.dir.join(format!("{key}.meta"));
        let body_path = self.dir.join(format!("{key}.body"));
        self.write_entry(&meta_path, &body_path, &identity, url, &result)
            .map_err(|error| NetworkError::Io(error.to_string()))?;
        Ok((status, true))
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

/// `method` as the cache keys it: an HTTP token (RFC 9110 `tchar`s), with
/// the six standard methods upper-cased like fetch() normalizes them (others
/// stay case-sensitive, as in the Fetch spec).
fn normalize_method(method: &str) -> Result<String, NetworkError> {
    let is_tchar = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b);
    if method.is_empty() || !method.bytes().all(is_tchar) {
        return Err(NetworkError::InvalidUrl(format!(
            "invalid http method: {method:?}"
        )));
    }
    let upper = method.to_ascii_uppercase();
    Ok(
        if matches!(
            upper.as_str(),
            "DELETE" | "GET" | "HEAD" | "OPTIONS" | "POST" | "PUT"
        ) {
            upper
        } else {
            method.to_string()
        },
    )
}

/// The request identity, unambiguously: each variable field is its byte
/// length (u64 little-endian) followed by the bytes, plus one flag byte.
/// Both the file name and the stored identity derive from this.
fn identity_bytes(url: &str, method: &str, follows_redirects: bool, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + method.len() + url.len() + body.len());
    for field in [method.as_bytes(), url.as_bytes()] {
        out.extend_from_slice(&(field.len() as u64).to_le_bytes());
        out.extend_from_slice(field);
    }
    out.push(u8::from(follows_redirects));
    out.extend_from_slice(&(body.len() as u64).to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// The `.meta` file's first line: the whole identity, hex-encoded (so it is
/// one line whatever the fields hold), compared in full on read.
fn identity_line(url: &str, method: &str, follows_redirects: bool, body: &[u8]) -> String {
    let hex: String = identity_bytes(url, method, follows_redirects, body)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("identity {hex}")
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

/// The entry name (`DIR/{key}.meta`/`.body`) the engine uses for a GET of
/// `url` made as `kind`: a navigation doesn't follow redirects (each hop
/// has its own entry), every subresource does.
pub fn survey_cache_key(kind: RequestKind, url: &Url) -> String {
    cache_key(&url.to_string(), "GET", kind != RequestKind::Document, b"")
}

/// FNV-1a over the identity bytes. Stable across runs and Rust versions
/// (unlike `DefaultHasher`), which is all a file name needs — collisions are
/// caught by the full identity comparison. Public for the survey tooling
/// (the re-fetch helper names entries with it); nothing else needs it.
pub fn cache_key(url: &str, method: &str, follows_redirects: bool, body: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in identity_bytes(url, method, follows_redirects, body) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{
        Cache, FetchResult, HttpResponse, NetworkError, RequestKind, SurveyMiss, Url, cache_key,
        identity_bytes, normalize_method, survey_cache_key, survey_stats,
    };

    fn response_with(status_code: u16, body: &str) -> Result<FetchResult, NetworkError> {
        let mut result = response(body)?;
        result.response.status_code = status_code;
        Ok(result)
    }

    #[test]
    fn survey_cache_key_names_the_entry_the_engine_writes() {
        // The re-fetch helper (compat round 4 S) finds entries by this key:
        // a navigation is keyed without following redirects, a stylesheet
        // with.
        let cache = temp_cache("survey-key", false);
        let page = Url::parse("https://a.test/page").unwrap();
        let sheet = Url::parse("https://a.test/site.css").unwrap();
        cache
            .fetch(RequestKind::Document, (&page, "GET", false, b""), || {
                response("<p>")
            })
            .unwrap();
        cache
            .fetch(RequestKind::Css, (&sheet, "GET", true, b""), || {
                response("p{}")
            })
            .unwrap();
        for (kind, url) in [(RequestKind::Document, &page), (RequestKind::Css, &sheet)] {
            let meta = cache
                .dir
                .join(format!("{}.meta", survey_cache_key(kind, url)));
            assert!(meta.exists(), "{kind:?} entry at {meta:?}");
        }
        assert_ne!(
            survey_cache_key(RequestKind::Document, &page),
            survey_cache_key(RequestKind::Css, &page)
        );
        let _ = std::fs::remove_dir_all(&cache.dir);
    }

    #[test]
    fn refetch_stores_only_a_200_and_otherwise_keeps_the_old_entry() {
        let cache = temp_cache("refetch", false);
        let url = Url::parse("https://a.test/skin.css").unwrap();
        let key = (&url, "GET", true, &b""[..]);
        // Recorded once as a 429 (the round-3 kowiki skin sheet).
        cache
            .fetch(RequestKind::Css, key, || response_with(429, "slow down"))
            .unwrap();
        // A re-fetch that is rate-limited again changes nothing.
        assert_eq!(
            cache
                .refetch(RequestKind::Css, &url, || response_with(429, "again"))
                .unwrap(),
            (429, false)
        );
        let hit = cache
            .fetch(RequestKind::Css, key, || panic!("cached"))
            .unwrap();
        assert_eq!(hit.response.body, b"slow down");
        // A 200 replaces it.
        assert_eq!(
            cache
                .refetch(RequestKind::Css, &url, || response("p{}"))
                .unwrap(),
            (200, true)
        );
        let hit = cache
            .fetch(RequestKind::Css, key, || panic!("cached"))
            .unwrap();
        assert_eq!(
            (hit.response.status_code, &hit.response.body[..]),
            (200, &b"p{}"[..])
        );
        // A miss answered with 404 stays a miss.
        let gone = Url::parse("https://a.test/chunk-1a2b.js").unwrap();
        assert_eq!(
            cache
                .refetch(RequestKind::Script, &gone, || response_with(404, ""))
                .unwrap(),
            (404, false)
        );
        let meta = cache.dir.join(format!(
            "{}.meta",
            survey_cache_key(RequestKind::Script, &gone)
        ));
        assert!(!meta.exists());
        let _ = std::fs::remove_dir_all(&cache.dir);
    }

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
    fn method_with_tab_or_newline_is_rejected_before_the_cache() {
        let url = Url::parse("https://a.test/").unwrap();
        let cache = temp_cache("method", false);
        for method in ["GET\tx", "GET\nidentity", "PO ST", "", "GET\r"] {
            let result = cache.fetch(RequestKind::Other, (&url, method, true, b""), || {
                panic!("an invalid method must never be fetched or cached")
            });
            assert!(result.is_err(), "{method:?} accepted");
        }
        assert!(!cache.dir.exists(), "nothing was written");
        // Standard methods normalize like fetch(); others keep their case.
        assert_eq!(normalize_method("get").unwrap(), "GET");
        assert_eq!(normalize_method("Patch").unwrap(), "Patch");
    }

    #[test]
    fn recording_sends_only_get_and_fails_other_methods_locally() {
        let url = Url::parse("https://beacon.test/collect").unwrap();
        let cache = temp_cache("get-only", false);
        let refused_before = survey_stats().refused_non_get;
        for method in ["POST", "post", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"] {
            let result = cache.fetch(RequestKind::Other, (&url, method, true, b"payload"), || {
                panic!("{method} must not reach the network while recording")
            });
            assert!(result.is_err(), "{method} was sent");
        }
        assert!(!cache.dir.exists(), "nothing recorded for refused methods");
        // Other tests may run in parallel, so only a lower bound holds.
        assert!(survey_stats().refused_non_get >= refused_before + 7);

        let fetched = Cell::new(false);
        let result = cache.fetch(RequestKind::Other, (&url, "get", true, b""), || {
            fetched.set(true);
            response("ok")
        });
        assert!(fetched.get() && result.is_ok(), "GET still goes out");
        let _ = std::fs::remove_dir_all(&cache.dir);
    }

    #[test]
    fn offline_miss_is_counted_for_the_survey_report() {
        let url = Url::parse("https://a.test/never-recorded.css").unwrap();
        let cache = temp_cache("miss", true);
        let result = cache.fetch(RequestKind::Other, (&url, "GET", true, b""), || {
            panic!("offline never fetches")
        });
        assert!(result.is_err());
        assert!(
            survey_stats()
                .offline_misses
                .contains(&"GET https://a.test/never-recorded.css".to_string())
        );
    }

    #[test]
    fn offline_miss_records_the_request_kind() {
        let url = Url::parse("https://a.test/never-recorded-kind.css").unwrap();
        let cache = temp_cache("miss-kind", true);
        let _ = cache.fetch(RequestKind::Css, (&url, "GET", true, b""), || {
            panic!("offline never fetches")
        });
        assert!(survey_stats().misses.contains(&SurveyMiss {
            kind: RequestKind::Css,
            url,
        }));
    }

    #[test]
    fn identity_fields_cannot_pose_as_each_other() {
        // With delimiters, moving bytes between fields could produce the
        // same identity; length prefixes make every split distinct.
        let a = identity_bytes("https://a.test/xy", "GET", true, b"");
        let b = identity_bytes("https://a.test/x", "GET", true, b"y");
        let c = identity_bytes("https://a.test/xy", "GET", false, b"");
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn hit_requires_the_stored_request_identity_to_match() {
        let url = Url::parse("https://a.test/page").unwrap();
        let other = Url::parse("https://evil.test/other").unwrap();
        let cache = temp_cache("identity", false);

        // Record `other`, then move its files to the name `url` hashes to —
        // what a hash collision (or a crafted request) would look like.
        let _ = cache.fetch(RequestKind::Other, (&other, "GET", true, b""), || {
            response("other's body")
        });
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
            .fetch(RequestKind::Other, (&url, "GET", true, b""), || {
                fetched.set(true);
                response("real body")
            })
            .unwrap();
        assert!(fetched.get(), "a mismatched entry must be a miss");
        assert_eq!(result.response.body, b"real body");

        // Now recorded under its own identity: a hit, no fetch.
        let again = cache
            .fetch(RequestKind::Other, (&url, "GET", true, b""), || {
                panic!("should be cached")
            })
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
                .fetch(RequestKind::Other, (&third, "GET", true, b""), || panic!(
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
