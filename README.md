# mini-browser

[![Rust](https://img.shields.io/badge/Rust-1.94.1-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![html5ever](https://img.shields.io/badge/html5ever-0.38-8a2be2)](https://crates.io/crates/html5ever)
[![cssparser](https://img.shields.io/badge/cssparser-0.36-2ea44f)](https://crates.io/crates/cssparser)
[![taffy](https://img.shields.io/badge/taffy-0.10-1f6feb)](https://crates.io/crates/taffy)
[![cosmic-text](https://img.shields.io/badge/cosmic--text-0.19-c79e00)](https://crates.io/crates/cosmic-text)
[![tiny-skia](https://img.shields.io/badge/tiny--skia-0.12-9e9e9e)](https://crates.io/crates/tiny-skia)
[![winit](https://img.shields.io/badge/winit-0.30-3478f6)](https://crates.io/crates/winit)
[![rquickjs](https://img.shields.io/badge/rquickjs-0.11-ad24a8)](https://crates.io/crates/rquickjs)

브라우저 내부를 직접 짜보는 학습용 토이 프로젝트. URL 또는 정적 입력에서 HTML/CSS/JS 를 읽어 DOM/스타일/레이아웃을 거쳐 화면에 그리고, JS 가 DOM 을 mutate 하면 같은 프레임에서 재반영한다.

핵심 의존성: `html5ever` / `cssparser` + `selectors` / `taffy` / `cosmic-text` / `tiny-skia` / `winit` + `softbuffer` / `ureq` + `url` / `rquickjs`. 코드는 4-crate Cargo workspace.

## Workspace Layout

```
[workspace]
├── crates/mb-dom      → 파서 + DOM/CSS/style 데이터 (no I/O)
│                        html5ever / cssparser / selectors / image / url
├── crates/mb-engine   → 레이아웃 + 페인트 (mb-dom 위)
│                        taffy / cosmic-text / tiny-skia
├── crates/mb-runtime  → 오케스트레이터 + JS + IO (mb-engine 위)
│                        rquickjs / ureq / tokio / state/
└── crates/mb-shell    → 바이너리 (mb-runtime 위)
                         winit / softbuffer
```

각 crate 는 위에서 아래로만 의존한다. mb-runtime 이 mb-dom + mb-engine 의 surface 를 `pub use` 로 다시 노출하므로 integration tests 와 future embedder 는 `mb_runtime::*` 한 namespace 로 접근 가능.

## Pipeline

```
URL or sample HTML/CSS
  -> Network Loader (ureq)                   [mb-runtime::net]
  -> Resource Loader (parallel scoped fetch) [mb-runtime::resource]
  -> HTML Parser (html5ever)                 [mb-dom::html]
  -> CSS Parser (cssparser + selectors)      [mb-dom::css, mb-dom::dom_select]
  -> JS Runtime (rquickjs)                   [mb-runtime::js]
       │ <script> 실행, document/window globals
       │ DOM bridge (flat-hook 패턴 — Rust 콜백은 primitive 만 받음)
       │ setTimeout / rAF / Promise microtask
  -> Style Engine (cascade, :hover/:focus/:active) [mb-dom::style]
  -> Layout (taffy + boundary block/inline/flex/grid/table) [mb-engine::layout]
  -> Display List (Solid/Rounded/Text/Image/Gradient/Shadow/Transform) [mb-engine::view]
  -> Rasterizer (tiny-skia + cosmic-text + swash) [mb-engine::render]
  -> Window (winit + softbuffer)             [mb-shell::window]
```

매 프레임마다 microtask·setTimeout·requestAnimationFrame 콜백이 layout pass 직전에 drain 되므로, 핸들러가 mutate 한 DOM 은 같은 프레임 화면에 즉시 반영된다.

## Build & Run

Rust toolchain 은 `rust-toolchain.toml` 로 1.94.1 에 핀.

```sh
# 워크스페이스 전체 빌드
cargo build --workspace

# 바이너리 실행
cargo run --bin mb-shell                 # 시작 페이지 (NTP)
cargo run --bin mb-shell -- https://example.com

# 테스트
cargo test --workspace

# 린트
cargo clippy --workspace --all-targets
cargo fmt --all
```

## What Works

### Rendering
- DOM (NodeId arena, tombstone subtree), HTML5 implicit-close 규칙, HTML entities
- 단위 `%` / `em` / `rem`, 색상 `rgb()` / `rgba()` / hex / named color
- Selectors: descendant / child / sibling (`+` `~`) / pseudo (`:hover` `:focus` `:active` `:link` `:nth-child` `:not(...)` 등) / attribute (`[type="text"]`)
- Layout: block / inline / inline-block / margin collapse / `line-height`
- `position: relative|absolute|fixed`, stacking context + `z-index`, float + `clear`
- **Flexbox** (`flex-grow` / `shrink` / `basis`, `justify-content`, `align-items`)
- **Grid** (`grid-template-*`, `fr`, `grid-area`, line names)
- Paint: `opacity`, `linear-gradient`, `radial-gradient` (farthest-corner), `box-shadow`, `text-shadow`, `transform: translate / scale / rotate`
- Text: cosmic-text 셰이핑 + swash 글리프 래스터 (color emoji 포함), 인라인 wrap
- Images: PNG / JPEG / GIF / BMP, `background-image: url(...)`

### Interaction
- 주소창 + 뒤로/앞으로/새로고침/메뉴 + 페이지 hit-testing (`:hover` 1프레임 latency)
- `<input type=text>` / `<textarea>` 포커스 + 캐럿 + 텍스트 편집
- 키보드 / 마우스 / 휠 (winit)

### JavaScript (rquickjs)
- `document.getElementById` / `querySelector` / `getElementsByClassName` / `createElement` / `createTextNode` / `body` / `head`
- Element: `tagName` / `textContent` / `children` / `parentElement` / `classList` / `value` / `innerHTML` / `getAttribute` / `setAttribute` / `appendChild` / `removeChild` / `insertBefore` / `replaceChild` / `cloneNode` / `matches` / `closest`
- 이벤트: `addEventListener` + bubble dispatch + `preventDefault` / `stopPropagation` / `currentTarget` / 키보드 이벤트
- `setTimeout` / `setInterval` / `clearTimeout` / `clearInterval` / `requestAnimationFrame` / `cancelAnimationFrame` / `Date.now`
- Promise microtask + `queueMicrotask`
- `fetch` (network error 시 `Promise.reject`) + `XMLHttpRequest` (status=0 + 'error' event)
- `localStorage` / `sessionStorage` (in-memory)
- `console.log/warn/error` (variadic stringification)
- `window` / `self` / `globalThis` aliases, `navigator.userAgent`, `history.{length,state,pushState,replaceState,back,forward,go}`, `location.{href,protocol,host,...}`

## Out of Scope (for now)

- 네비게이션이 메인 스레드 블로킹 → async migration 후보 (tokio + spawn_blocking 으로 옵션 검토 중)
- HTTP/2 / streaming body / WebSocket — `ureq` 가 HTTP/1.1 only
- Service Worker / Web Worker / Shadow DOM
- Subpixel anti-aliasing — tiny-skia 의 일반 AA 만
- 실제 정확한 spec compliance (table CSS 시맨틱은 boundary 로 fallback, etc.)

## License

학습용 토이 프로젝트. 라이브러리 통합 후 의존하는 crate 들의 라이선스는 각자.
