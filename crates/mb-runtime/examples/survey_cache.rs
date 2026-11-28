// Survey cache helper (docs/compat-round4.md step 0 S), driven by
// `scripts/compat-corpus.sh record --refetch`:
//
//   survey_cache key URL KIND       print the entry name the engine uses
//   MB_NET_CACHE=DIR survey_cache refetch URL KIND
//                                    GET URL once (engine wire path, Accept
//                                    by KIND), store it only if it is a 200
//
// KIND is a `net::RequestKind` name: document css js img font fetch xhr
// other. `refetch` prints `STATUS<TAB>stored|kept` (kept = the cache was
// not changed), or `error<TAB>message` and exits 2. It never retries; the
// caller spaces requests and checks robots.txt.

use mb_runtime::net::{self, RequestKind, Url};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: survey_cache key|refetch URL KIND";
    let [command, url, kind] = args.as_slice() else {
        eprintln!("{usage}");
        std::process::exit(64);
    };
    let Some(kind) = RequestKind::parse(kind) else {
        eprintln!("unknown kind {kind:?}");
        std::process::exit(64);
    };
    let url = match Url::parse(url) {
        Ok(url) => url,
        Err(error) => {
            eprintln!("bad url: {error:?}");
            std::process::exit(64);
        }
    };
    match command.as_str() {
        "key" => println!("{}", net::survey_cache_key(kind, &url)),
        "refetch" => match net::survey_refetch(kind, &url) {
            Ok((status, stored)) => {
                println!("{status}\t{}", if stored { "stored" } else { "kept" })
            }
            Err(error) => {
                println!("error\t{}", format!("{error:?}").replace(['\t', '\n'], " "));
                std::process::exit(2);
            }
        },
        _ => {
            eprintln!("{usage}");
            std::process::exit(64);
        }
    }
}
