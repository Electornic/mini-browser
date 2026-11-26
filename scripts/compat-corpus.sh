#!/usr/bin/env bash
# Real-site compatibility corpus (docs/compat-round3.md 0.2).
#
#   # record once (the only step that touches the network)
#   MB_NET_CACHE=$PWD/compat-cache/net scripts/compat-corpus.sh record [NAME…]
#   # replay as often as needed (always offline)
#   MB_NET_CACHE=$PWD/compat-cache/net scripts/compat-corpus.sh run [LABEL]
#   # what appeared / disappeared between two runs
#   scripts/compat-corpus.sh diff LABEL_BEFORE LABEL_AFTER
#
# Sites come from scripts/compat-sites.tsv. Outputs (all git-ignored):
#   compat-cache/corpus/recorded.tsv        name, url, robots verdict, rule, http code, date — written by record
#   compat-cache/corpus/robots/<name>.txt   the robots.txt that verdict came from (+ .code = HTTP status)
#   compat-cache/corpus/<LABEL>/<name>/     compat_probe output + stdout/stderr ("record" for the recording run)
#   compat-cache/corpus/<LABEL>/corpus-report.tsv  one row per site
#
# Politeness (record): robots.txt fetched once per site by this script and
# obeyed for the `*` group (disallowed → skipped); one visit per site,
# sequential, 2 s apart; a site already in recorded.tsv is never visited
# again (the cache is frozen for compat round 3) — `record --supplement NAME…`
# is the documented exception (offline misses after an engine change, at
# most once per step). Bot blocks are recorded as `blocked`, never worked
# around. While recording, the survey cache sends only GET (net/cache.rs).
#
# Safety: MB_NET_CACHE must be absolute; `run` refuses an empty/missing
# cache; nothing is written to a path `git check-ignore` doesn't confirm
# is ignored (real-site content must never be committed).

set -euo pipefail

ROOT=$(git rev-parse --show-toplevel)
SITES=$ROOT/scripts/compat-sites.tsv
CORPUS=$ROOT/compat-cache/corpus
PROBE=$ROOT/target/release/examples/compat_probe
UA='Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36'
LIMIT_SECS=120
GAP_SECS=2

die() {
    echo "compat-corpus: $*" >&2
    exit 1
}

usage() {
    sed -n '2,10p' "$0" >&2
    exit 2
}

# Refuse to write anywhere git would track (or outside the repo).
require_ignored() {
    local path=$1
    git -C "$ROOT" check-ignore -q -- "$path" 2>/dev/null ||
        die "refusing to write $path: not a git-ignored path inside the repo (real-site content must never be committed)"
}

require_cache_env() {
    [[ -n ${MB_NET_CACHE:-} ]] || die "MB_NET_CACHE is not set (e.g. MB_NET_CACHE=\$PWD/compat-cache/net)"
    [[ $MB_NET_CACHE == /* ]] || die "MB_NET_CACHE must be an absolute path, got '$MB_NET_CACHE'"
    require_ignored "$MB_NET_CACHE"
}

find_timeout() {
    if command -v timeout >/dev/null; then
        TIMEOUT=timeout
    elif command -v gtimeout >/dev/null; then
        TIMEOUT=gtimeout
    else
        die "neither timeout nor gtimeout found — refusing to run probes without a time limit"
    fi
}

build_probe() {
    (cd "$ROOT" && cargo build --release -p mb-agent --example compat_probe >&2)
}

# name<TAB>url<TAB>expected, comments and blank lines skipped.
sites() {
    grep -v '^#' "$SITES" | grep -v '^[[:space:]]*$'
}

site_field() { # name, field number
    sites | awk -F'\t' -v n="$1" -v f="$2" '$1 == n { print $f }'
}

recorded_field() { # name, field number (latest line wins)
    [[ -f $CORPUS/recorded.tsv ]] || return 0
    awk -F'\t' -v n="$1" -v f="$2" '$1 == n { v = $f } END { if (v != "") print v }' "$CORPUS/recorded.tsv"
}

# robots.txt verdict for URL under the `*` group, Google/RFC 9309 rules:
# longest matching pattern wins, Allow wins a tie, `*` wildcard, `$` end.
# Prints "allow|disallow<TAB>matched rule". Reads untrusted data only.
ROBOTS_PY='
import re, sys
from urllib.parse import urlsplit
text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
parts = urlsplit(sys.argv[2])
path = (parts.path or "/") + ("?" + parts.query if parts.query else "")
groups, agents, rules, in_rules = [], [], [], False
for raw in text.splitlines():
    line = raw.split("#", 1)[0].strip()
    if ":" not in line:
        continue
    key, value = (s.strip() for s in line.split(":", 1))
    key = key.lower()
    if key == "user-agent":
        if in_rules:
            groups.append((agents, rules)); agents, rules, in_rules = [], [], False
        agents.append(value.lower())
    elif key in ("allow", "disallow"):
        in_rules = True
        rules.append((key, value))
if agents:
    groups.append((agents, rules))
star = [r for a, rs in groups if "*" in a for r in rs]
def matches(pattern, path):
    anchored = pattern.endswith("$")
    body = pattern[:-1] if anchored else pattern
    regex = "".join(".*" if c == "*" else re.escape(c) for c in body)
    return re.match(regex + ("$" if anchored else ""), path) is not None
best = None
for key, pattern in star:
    if not pattern or not matches(pattern, path):
        continue
    rank = (len(pattern), key == "allow")
    if best is None or rank > best[0]:
        best = (rank, key, pattern)
if best is None or best[1] == "allow":
    print("allow\t" + ("-" if best is None else "Allow: " + best[2]))
else:
    print("disallow\tDisallow: " + best[2])
'

robots_check() { # name url → sets ROBOTS_VERDICT ROBOTS_RULE ROBOTS_CODE
    local name=$1 url=$2 origin file
    origin=$(printf '%s' "$url" | sed -E 's#^(https?://[^/]+).*#\1#')
    file=$CORPUS/robots/$name.txt
    mkdir -p "$CORPUS/robots"
    # Once per site, even across an interrupted record: reuse what we got.
    if [[ -f $file && -f $CORPUS/robots/$name.code ]]; then
        ROBOTS_CODE=$(cat "$CORPUS/robots/$name.code")
    else
        ROBOTS_CODE=$(curl -sS -L --max-time 20 -A "$UA" -o "$file" -w '%{http_code}' "$origin/robots.txt" 2>/dev/null) || ROBOTS_CODE=000
        printf '%s\n' "$ROBOTS_CODE" >"$CORPUS/robots/$name.code"
    fi
    case $ROBOTS_CODE in
        2??)
            local out
            out=$(python3 -I -c "$ROBOTS_PY" "$file" "$url")
            ROBOTS_VERDICT=${out%%$'\t'*}
            ROBOTS_RULE=${out#*$'\t'}
            ;;
        4??)
            # RFC 9309 2.3.1.3: unavailable robots.txt = no restrictions.
            ROBOTS_VERDICT=allow ROBOTS_RULE="no robots.txt ($ROBOTS_CODE)"
            ;;
        *)
            # Unreachable (5xx, network): assume complete disallow.
            ROBOTS_VERDICT=disallow ROBOTS_RULE="robots.txt unreachable ($ROBOTS_CODE)"
            ;;
    esac
}

# Runs the probe for one site into DIR; sets RESULT from the exit code or
# the probe's own summary.tsv.
probe_site() { # url dir expected
    local url=$1 dir=$2 expected=$3 status=0
    rm -rf "$dir"
    mkdir -p "$dir"
    "$TIMEOUT" "$LIMIT_SECS" "$PROBE" "$url" "$dir" "$expected" >"$dir/stdout.txt" 2>"$dir/stderr.txt" || status=$?
    case $status in
        124) RESULT=timeout ;;
        101 | 134) RESULT=panic ;; # unwinding panic / abort (stack overflow)
        *)
            if [[ -f $dir/summary.tsv ]]; then
                RESULT=$(awk -F'\t' 'NR == 2 { print $2 }' "$dir/summary.tsv")
            else
                RESULT="nav-fail"
            fi
            ;;
    esac
}

report_header() {
    printf 'name\trobots\tbudget_lines\t%s\n' "$("$PROBE" --header)"
}

# One report row: name, robots, [mb-budget] lines, then the probe columns
# (blank "-" columns when the probe died before writing them).
report_row() { # name url robots dir
    local name=$1 url=$2 robots=$3 dir=$4 budget=- columns
    [[ -f $dir/stderr.txt ]] && budget=$(grep -c '^\[mb-budget\]' "$dir/stderr.txt" || true)
    columns=$("$PROBE" --header | awk -F'\t' '{ print NF }')
    if [[ -f $dir/summary.tsv && $RESULT != timeout && $RESULT != panic ]]; then
        printf '%s\t%s\t%s\t%s\n' "$name" "$robots" "$budget" "$(sed -n 2p "$dir/summary.tsv")"
    else
        printf '%s\t%s\t%s\t%s\t%s' "$name" "$robots" "$budget" "$url" "$RESULT"
        for ((i = 3; i <= columns; i++)); do printf '\t-'; done
        printf '\n'
    fi
}

print_table() { # report
    awk -F'\t' 'NR == 1 { for (i = 1; i <= NF; i++) col[$i] = i; next }
        { printf "%-13s %-14s %7s ms  decl %5s/%-5s rule %4s/%-4s miss %-3s err %-3s controls %s %s\n",
          $col["name"], $col["result"], $col["load_ms"], $col["decl_drop_ours"], $col["decl_drop_browser"],
          $col["rule_drop_ours"], $col["rule_drop_browser"], $col["offline_miss"], $col["console_errors"],
          $col["controls_found"], ($col["controls_missing"] == "-" ? "" : $col["controls_missing"]) }' "$1"
}

cmd_record() {
    require_cache_env
    [[ -z ${MB_NET_CACHE_OFFLINE:-} ]] || die "record with MB_NET_CACHE_OFFLINE set would record nothing; unset it"
    command -v curl >/dev/null || die "curl is needed for robots.txt"
    command -v python3 >/dev/null || die "python3 is needed to evaluate robots.txt"
    local supplement=0
    if [[ ${1:-} == --supplement ]]; then
        supplement=1
        shift
        (($# > 0)) || die "--supplement needs the site names to re-record"
    fi
    require_ignored "$CORPUS"
    find_timeout
    build_probe
    mkdir -p "$CORPUS" "$MB_NET_CACHE"
    [[ -f $CORPUS/recorded.tsv ]] || printf 'name\turl\trobots\trule\trobots_http\tdate\tresult\n' >"$CORPUS/recorded.tsv"

    # (macOS ships bash 3.2: no mapfile, and "${empty[@]}" trips set -u.)
    local names=() first=1 name
    if (($# > 0)); then
        names=("$@")
    else
        while IFS= read -r name; do names+=("$name"); done < <(sites | cut -f1)
    fi
    ((${#names[@]} > 0)) || die "no sites in $SITES"
    for name in "${names[@]}"; do
        local url expected
        url=$(site_field "$name" 2)
        [[ -n $url ]] || die "no site '$name' in $SITES"
        expected=$(site_field "$name" 3)
        if [[ -n $(recorded_field "$name" 1) && $supplement == 0 ]]; then
            echo "skip $name: already recorded on $(recorded_field "$name" 6) (cache frozen; see --supplement)" >&2
            continue
        fi
        ((first)) || sleep "$GAP_SECS"
        first=0
        if [[ $supplement == 0 ]]; then
            robots_check "$name" "$url"
        else
            ROBOTS_VERDICT=$(recorded_field "$name" 3) ROBOTS_RULE=$(recorded_field "$name" 4) ROBOTS_CODE=$(recorded_field "$name" 5)
            [[ $ROBOTS_VERDICT == allow ]] || die "$name was not allowed by robots.txt; not re-recording"
        fi
        local date
        date=$(date -u +%Y-%m-%dT%H:%M:%SZ)
        if [[ $ROBOTS_VERDICT != allow ]]; then
            echo "skip $name: robots.txt $ROBOTS_RULE" >&2
            printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$url" "$ROBOTS_VERDICT" "$ROBOTS_RULE" "$ROBOTS_CODE" "$date" skipped >>"$CORPUS/recorded.tsv"
            continue
        fi
        sleep "$GAP_SECS" # robots.txt and the page are two requests to the same host
        echo "record $name ($url)" >&2
        probe_site "$url" "$CORPUS/record/$name" "$expected"
        echo "  -> $RESULT" >&2
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$url" "$ROBOTS_VERDICT" "$ROBOTS_RULE" "$ROBOTS_CODE" "$date" "$RESULT" >>"$CORPUS/recorded.tsv"
    done
}

cmd_run() {
    require_cache_env
    [[ -d $MB_NET_CACHE ]] && compgen -G "$MB_NET_CACHE/*.meta" >/dev/null ||
        die "MB_NET_CACHE ($MB_NET_CACHE) is missing or empty — record first (a run without a cache would mark every site nav-fail)"
    # Replay never touches the network, whatever the caller's environment.
    export MB_NET_CACHE_OFFLINE=1
    local label=${1:-latest}
    [[ $label =~ ^[A-Za-z0-9._-]+$ && $label != record ]] || die "bad label '$label'"
    local out=$CORPUS/$label
    require_ignored "$out"
    find_timeout
    build_probe
    mkdir -p "$out"
    local report=$out/corpus-report.tsv
    report_header >"$report"
    while IFS=$'\t' read -r name url expected; do
        local robots verdict
        verdict=$(recorded_field "$name" 3)
        robots="${verdict:-unknown} $(recorded_field "$name" 6)"
        if [[ $verdict != allow ]]; then
            RESULT=skipped
            [[ -z $verdict ]] && RESULT=not-recorded
            report_row "$name" "$url" "$robots" "/nonexistent" >>"$report"
            echo "$name: $RESULT" >&2
            continue
        fi
        probe_site "$url" "$out/$name" "$expected"
        report_row "$name" "$url" "$robots" "$out/$name" >>"$report"
        echo "$name: $RESULT" >&2
    done < <(sites)
    print_table "$report"
    echo "report: $report" >&2
}

cmd_diff() {
    (($# == 2)) || usage
    local before=$CORPUS/$1 after=$CORPUS/$2
    [[ -d $before && -d $after ]] || die "no such run: $before or $after"
    while IFS=$'\t' read -r name _ _; do
        local a=$before/$name/elements.txt b=$after/$name/elements.txt
        [[ -f $a && -f $b ]] || continue
        local gone added
        gone=$(comm -23 <(sort "$a") <(sort "$b"))
        added=$(comm -13 <(sort "$a") <(sort "$b"))
        [[ -z $gone && -z $added ]] && continue
        echo "== $name: -$(grep -c . <<<"$gone" || true) +$(grep -c . <<<"$added" || true)"
        [[ -n $gone ]] && sed 's/^/- /' <<<"$gone"
        [[ -n $added ]] && sed 's/^/+ /' <<<"$added"
    done < <(sites)
}

case ${1:-} in
    record) shift; cmd_record "$@" ;;
    run) shift; cmd_run "$@" ;;
    diff) shift; cmd_diff "$@" ;;
    *) usage ;;
esac
