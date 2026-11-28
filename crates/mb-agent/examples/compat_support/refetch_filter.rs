// The N6 re-fetch filter (docs/compat-round4.md N6, step 0 S): which of a
// site's offline misses (`misses.tsv` — kind, host, URL) a targeted
// re-fetch may request.
//   (iii) a beacon trace in the path or query — never, whatever the kind
//   (i)   css · js · font · img — from any host (round 3 recorded these)
//   (ii)  anything else (document · fetch · xhr · other) — only from the
//         document's own host
// A trace is a lower-cased path/query token (split on non-alphanumerics)
// starting with collect / beacon / pixel / track, or equal to log / logs /
// logging / logger — tokens, so `logo.png` and `blog` pass.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Miss {
    pub kind: String,
    pub host: String,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Fetch,
    SkipBeacon,
    SkipOtherHost,
}

/// `kind<TAB>host<TAB>url` lines (blank lines skipped).
pub fn parse_misses(tsv: &str) -> Vec<Miss> {
    tsv.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let (kind, host, url) = (fields.next()?, fields.next()?, fields.next()?);
            (!url.trim().is_empty()).then(|| Miss {
                kind: kind.trim().to_string(),
                host: host.trim().to_string(),
                url: url.trim().to_string(),
            })
        })
        .collect()
}

pub fn judge(miss: &Miss, document_host: &str) -> Verdict {
    if has_beacon_trace(&miss.url) {
        return Verdict::SkipBeacon;
    }
    if matches!(miss.kind.as_str(), "css" | "js" | "font" | "img") || miss.host == document_host {
        Verdict::Fetch
    } else {
        Verdict::SkipOtherHost
    }
}

pub fn has_beacon_trace(url: &str) -> bool {
    // Path and query only: the host (`log.example.com`) isn't the request.
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path_and_query = after_scheme.find('/').map_or("", |at| &after_scheme[at..]);
    let path_and_query = path_and_query.split('#').next().unwrap_or("");
    path_and_query
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| {
            ["collect", "beacon", "pixel", "track"]
                .iter()
                .any(|marker| token.starts_with(marker))
                || matches!(token, "log" | "logs" | "logging" | "logger")
        })
}

/// The list-file lines for site `name` (`name<TAB>url<TAB>kind`, each URL
/// once) and how many misses were filtered out.
pub fn refetch_lines(name: &str, document_host: &str, misses: &[Miss]) -> (Vec<String>, usize) {
    let mut lines = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut skipped = 0;
    for miss in misses {
        match judge(miss, document_host) {
            Verdict::Fetch => {
                if seen.insert(miss.url.clone()) {
                    lines.push(format!("{name}\t{}\t{}", miss.url, miss.kind));
                }
            }
            Verdict::SkipBeacon | Verdict::SkipOtherHost => skipped += 1,
        }
    }
    (lines, skipped)
}
