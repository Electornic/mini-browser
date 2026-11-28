// compat round 4 step 0 S: the N6 re-fetch filter on a hand-written
// `misses.tsv` (fake hosts).

#[allow(dead_code)]
#[path = "../examples/compat_support/refetch_filter.rs"]
mod refetch_filter;

use refetch_filter::{Verdict, has_beacon_trace, judge, parse_misses, refetch_lines};

const MISSES: &str = include_str!("fixtures/compat-misses.tsv");
const HOST: &str = "www.example.test";

#[test]
fn subresource_kinds_come_from_any_host_and_others_only_from_the_document_host() {
    let misses = parse_misses(MISSES);
    let verdict = |url: &str| judge(misses.iter().find(|m| m.url == url).unwrap(), HOST);
    assert_eq!(
        verdict("https://static.other.test/skin.css?v=3"),
        Verdict::Fetch
    );
    assert_eq!(verdict("https://fonts.other.test/a.woff2"), Verdict::Fetch);
    assert_eq!(
        verdict("https://cdn.example.test/chunks/app-1a2b.js"),
        Verdict::Fetch
    );
    assert_eq!(
        verdict("https://www.example.test/api/menu.json"),
        Verdict::Fetch
    );
    assert_eq!(verdict("https://www.example.test/next"), Verdict::Fetch);
    // A subdomain is another host.
    assert_eq!(
        verdict("https://api.example.test/v1/feed"),
        Verdict::SkipOtherHost
    );
    assert_eq!(verdict("https://other.test/"), Verdict::SkipOtherHost);
}

#[test]
fn beacon_traces_are_skipped_whatever_the_kind() {
    let misses = parse_misses(MISSES);
    let verdict = |url: &str| judge(misses.iter().find(|m| m.url == url).unwrap(), HOST);
    assert_eq!(
        verdict("https://ads.other.test/pixel.gif?id=9"),
        Verdict::SkipBeacon
    );
    assert_eq!(
        verdict("https://www.example.test/track/view?x=1"),
        Verdict::SkipBeacon
    );
    assert_eq!(
        verdict("https://www.example.test/g/collect?v=2"),
        Verdict::SkipBeacon
    );
    assert_eq!(
        verdict("https://stats.other.test/log?e=load"),
        Verdict::SkipBeacon
    );
    // Tokens, not substrings: a logo, a blog and a catalog are not beacons.
    assert_eq!(
        verdict("https://cdn.example.test/img/logo.png"),
        Verdict::Fetch
    );
    assert_eq!(
        verdict("https://www.example.test/blog/catalog"),
        Verdict::Fetch
    );
    // The host is not looked at.
    assert!(!has_beacon_trace("https://log.example.test/a.css"));
    assert!(has_beacon_trace("https://a.test/x?tracking=1"));
}

#[test]
fn list_lines_name_each_url_once_and_count_what_was_filtered() {
    let (lines, skipped) = refetch_lines("example", HOST, &parse_misses(MISSES));
    assert_eq!(
        lines,
        [
            "example\thttps://cdn.example.test/img/logo.png\timg",
            "example\thttps://static.other.test/skin.css?v=3\tcss",
            "example\thttps://cdn.example.test/chunks/app-1a2b.js\tjs",
            "example\thttps://fonts.other.test/a.woff2\tfont",
            "example\thttps://www.example.test/api/menu.json\tfetch",
            "example\thttps://www.example.test/blog/catalog\tother",
            "example\thttps://www.example.test/next\tdocument",
        ]
    );
    // 2 other-host + 4 beacons; the repeated logo is a duplicate, not skipped.
    assert_eq!(skipped, 6);
}
