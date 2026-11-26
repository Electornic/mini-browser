//! Opt-in record/replay cache for real-site compatibility surveys.
//!
//! `MB_NET_CACHE=DIR` makes every fetch look in `DIR` first and store what it
//! fetched there, so a survey can run the engine over a real page many times
//! while the site sees one visit. `MB_NET_CACHE_OFFLINE=1` turns a miss into
//! an error instead of a fetch (replay only, never touch the network).
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
        let key = cache_key(&url.to_string(), method, follows_redirects, body);
        let meta_path = self.dir.join(format!("{key}.meta"));
        let body_path = self.dir.join(format!("{key}.body"));
        if let Some(hit) = read_entry(&meta_path, &body_path) {
            return Ok(hit);
        }
        if self.offline {
            return Err(NetworkError::Io(format!("not in MB_NET_CACHE: {url}")));
        }
        let result = fetch()?;
        // A failed write only costs a refetch next time; never fail the load.
        let _ = self.write_entry(&meta_path, &body_path, url, &result);
        Ok(result)
    }

    fn write_entry(
        &self,
        meta_path: &Path,
        body_path: &Path,
        url: &Url,
        result: &FetchResult,
    ) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let response = &result.response;
        let mut meta = format!("{}\n{}\n", response.status_code, result.final_url);
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

fn read_entry(meta_path: &Path, body_path: &Path) -> Option<FetchResult> {
    let meta = fs::read_to_string(meta_path).ok()?;
    let body = fs::read(body_path).ok()?;
    let mut lines = meta.lines();
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
    use super::cache_key;

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
