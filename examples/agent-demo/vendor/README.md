# vendor

Third-party bundles used by the demo pages, checked in so the demos run
offline with `python3 -m http.server` (no build, no modules).

| File | Package | Version | License |
|---|---|---|---|
| `preact.min.umd.js` | `preact/dist/preact.min.umd.js` | 10.29.8 | MIT ([PREACT-LICENSE](PREACT-LICENSE)) |
| `preact-hooks.umd.js` | `preact/hooks/dist/hooks.umd.js` | 10.29.8 | MIT ([PREACT-LICENSE](PREACT-LICENSE)) |

Used by [`../preact.html`](../preact.html) and the mb-agent capstone test
`preact_demo_adds_toggles_filters_and_goes_back`. Don't edit these files;
upgrade by copying the same paths from a newer `preact` package.
