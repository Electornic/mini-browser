# mini-browser

[![Rust](https://img.shields.io/badge/Rust-2024-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![boa_engine](https://img.shields.io/badge/boa__engine-0.21-8a2be2)](https://crates.io/crates/boa_engine)
[![minifb](https://img.shields.io/badge/minifb-0.27-1f6feb)](https://crates.io/crates/minifb)
[![fontdue](https://img.shields.io/badge/fontdue-0.9-2ea44f)](https://crates.io/crates/fontdue)
[![image](https://img.shields.io/badge/image-0.25-c79e00)](https://crates.io/crates/image)
[![native-tls](https://img.shields.io/badge/native--tls-0.2-9e9e9e)](https://crates.io/crates/native-tls)

브라우저 렌더링/스타일/JS 엔진을 Rust로 직접 구현해보는 학습용 프로젝트.

URL 또는 정적 입력에서 HTML/CSS를 읽어 DOM/스타일/레이아웃 구조로 변환한 뒤, 자체 렌더러로 화면에 그리고, [Boa](https://crates.io/crates/boa_engine) JS 엔진으로 페이지 스크립트를 실행한다. 단계(Phase) 단위로 범위를 늘려간다 — Phase 0은 정적 렌더링 + Chrome 스타일 UI, Phase 1은 CSS 표현력 확장(`%`/`em`/`rem`, descendant/`:hover`/`:focus`, `position`, `transform`, gradient, **flexbox**, **grid**), Phase 2는 JS 엔진 임베드와 DOM 바인딩 + 이벤트 + microtask/timer/rAF.

## Phase Status

| Phase | Status | Theme |
|---|---|---|
| 0 | **Done** | Block layout + Chrome 스타일 UI + NTP 시작 페이지 |
| 1 | **Done** | CSS Expansion — units, selectors, position, transform, gradient, **flexbox**, **grid** |
| 2 | **Done** | JS Engine Integration — Boa 임베드 + DOM read/mutate + 이벤트 + microtask/setTimeout/rAF |
| 3 | Backlog | Interactive Browser — 입력(`<input>`/`<textarea>`/`<form>`), preventDefault·키보드·focus 이벤트, fetch/XHR, navigator/location stub |

자세한 task 분해는 [docs/roadmap.md](docs/roadmap.md). Phase 3 워킹셋은 [Notion 보드](https://www.notion.so/d2148621e352424ba22199e4be237e22)에서 관리한다.

## Pipeline

```text
URL or sample input
  -> Network Loader (HTTP/HTTPS, redirect, keep-alive)
  -> HTML / CSS / Script bytes
  -> HTML Parser / CSS Parser
  -> DOM (NodeId arena) / Stylesheet
  -> JS Runtime (Boa) — <script> 실행, document/window globals, 이벤트 디스패치
  -> Style Engine (cascade + inheritance, :hover/:focus/:active)
  -> Layout (block / inline / inline-block / position / float / flex / grid)
  -> Display List (SolidRect / RoundedRect / Text / Image / LinearGradient / RadialGradient / BoxShadow / TransformGroup)
  -> Rasterizer
  -> Window (minifb)
```

매 프레임마다 JS 가 큐잉한 microtask·setTimeout·requestAnimationFrame 콜백이 layout pass 직전에 drain 되므로, 핸들러가 수행한 DOM mutation 은 같은 프레임의 화면에 즉시 반영된다.

## What Works (Phase 0–2)

### Rendering 파이프라인
- DOM (NodeId 기반 arena, tombstone subtree), HTML/CSS parser, Style Engine
- Layout: block / inline / inline-block / `position: relative|absolute|fixed` / float + `clear` / margin collapse / `line-height` / **flexbox** (`flex-grow`/`shrink`/`basis`, `justify-content`, `align-items`) / **grid** (`grid-template-columns`/`-rows`, `fr` 단위, `grid-area`, line names)
- Paint: `opacity`, `linear-gradient`, `radial-gradient`, `box-shadow`, `text-shadow`, `transform: translate / scale / rotate` (per-pixel inverse transform sampling)
- Display List: `SolidRect` / `RoundedRect` / `Text` / `Image` / `LinearGradient` / `RadialGradient` / `BoxShadow` / `TransformGroup`
- 기본 inline 흐름 (`<a>` / `<span>` / `<img>`)

### CSS support
- Selector: tag/class/id, descendant (` `), child (`>`), pseudo-classes (`:hover`/`:focus`/`:active`)
- 단위: `px` / `%` / `em` / `rem`
- 색상: hex (`#RGB`/`#RRGGBB`), `rgb()` / `rgba()`, named (HTML4 16색 + transparent)
- Property set: `display`, `width`/`height`, `margin-*`/`padding-*`/`border-*` (+ `border-radius` 4-value), `background-color`, `background-image` (gradient), `color`, `font-size`, `text-align`, `line-height`, `position`/`top`/`right`/`bottom`/`left`, `z-index`, `float`/`clear`, `opacity`, `box-shadow`, `text-shadow`, `transform`, **flex** 일족, **grid** 일족
- Inheritance: `color`, `font-size`, `text-align`, `line-height`, `text-shadow` 등

### JS 엔진 (Phase 2)
- Boa 0.21 임베드, 페이지 단위 globals 보존, `<script>` inline + external src 자동 로드 + 실행
- 글로벌: `console.log/warn/error`, `globalThis`/`window`/`self` 동치 alias, `document`
- DOM read: `document.getElementById`, `document.querySelector` (descendant + child combinator), `tagName`, `textContent`, `children`, `getAttribute`, `nodeType`
- DOM mutate: `document.createElement` / `createTextNode`, `appendChild` / `removeChild` / `insertBefore` / `replaceChild` / `cloneNode`, `setAttribute`, `textContent=` (stale handle 시 throw)
- 이벤트: `addEventListener('click', fn)` / `removeEventListener` (WHATWG dedup), bubble dispatch (target → root, text node 클릭은 nearest Element ancestor 로 retarget)
- 비동기: `setTimeout` / `setInterval` / `clearTimeout` / `clearInterval` / `requestAnimationFrame` / `cancelAnimationFrame`. Promise microtask 도 `execute` / `dispatch_event` 끝에 자동 drain
- 모든 mutation 은 `Rc<RefCell<Document>>` 공유로 layout 단에 즉시 반영

### 네트워크 / 리소스
- HTTP/HTTPS GET, redirect 추적, **HTTP keep-alive 풀** + chunked transfer decoding
- `<link rel="stylesheet">`, `<img src>`, `<script src>` **병렬 fetch** (`thread::scope`)
- web font (`@font-face`) 로드 + macOS 시스템 폰트 fallback (한글 표시)
- 에러 페이지, `text/plain` 페이지 처리

### Chrome UI
- 상단 chrome (88px = 32 tab strip + 56 toolbar)
- 단일 탭 (위쪽 corner만 둥근 RoundedRect)
- pill 모양 주소창 (focus 시 blue ring, placeholder 색 차별화)
- chevron back/forward + 12-stop ring refresh + 3-dot 메뉴 아이콘
- 히스토리 navigation (Alt+←/→ 또는 `Cmd/Ctrl+[ ]`), back/forward stack 에 fetched bytes/이미지/script body 까지 stash 해서 재페치 없이 즉시 복원

### 시작 페이지 (NTP)
URL 인자 없이 실행하면 Chrome NTP 스타일 페이지가 보임 — 가운데 큰 로고, pill 검색창, 단축 타일 4개.

## What's Next (Phase 3 — Backlog)

- **3A Event 확장**: `event.preventDefault` / `stopPropagation` / `currentTarget`, `keydown`/`keyup`, `focus`/`blur`
- **3B Form & Input**: `<input type=text>` 텍스트 박스 + caret, `<textarea>`, `<button>` + `<form>` submit, `input`/`change` 이벤트
- **3C DOM API 잔여**: `.innerHTML write`, `classList`, `closest`/`matches`, sibling 탐색
- **3D Network from JS**: `fetch` GET/POST + `Response.text/json`, `XMLHttpRequest`, `async/await` (`NativeAsyncJob` 활성화)
- **3E Browser State Stubs**: `navigator` / `location` / `history`, `window.addEventListener` / `document.addEventListener`
- **3H Refactor & Polish**: UA defaults `script`/`style` `display: none`, `main.rs` 분할

## Documents

- [Project Spec](docs/spec.md)
- [Architecture](docs/architecture.md)
- [Data Model](docs/data-model.md)
- [Roadmap](docs/roadmap.md)
- [Understanding Guide](docs/understanding-guide.md)

## Run

```bash
cargo run
```

원격 HTML을 불러오려면:

```bash
cargo run -- http://example.com
```

또는:

```bash
cargo run -- https://example.com
```

원격 문서의 외부 stylesheet (`<link rel="stylesheet">`), 이미지 (`<img src>`), 스크립트 (`<script src>`) 는 자동으로 같이 다운로드된다. `text/plain` 문서는 읽기용 페이지로 렌더되고, 로드 실패나 unsupported content type은 간단한 에러 페이지로 떨어진다.

앱 안에서는 상단 주소창을 클릭하거나 `Cmd+L`/`Ctrl+L`로 포커스한 뒤 URL을 입력하고 `Enter`로 이동할 수 있다. `Alt+Left/Right` 또는 `Cmd/Ctrl+[ ]`, 그리고 상단 뒤로/앞으로 버튼 클릭으로 이동할 수 있다. `Up/Down`, `PageUp/PageDown`, 마우스 휠로 스크롤할 수 있고, 문서 안의 `<a href>` 링크는 밑줄로 표시되며 hover 시 강조되고 클릭으로 이동할 수 있다. 페이지 안 element 에 `addEventListener('click', ...)` 가 등록되어 있으면 같은 클릭이 JS 핸들러로도 디스패치된다. `Esc`를 누르면 종료된다.

## Verification

```bash
cargo build
cargo test --lib
cargo test --bin mini-browser
cargo clippy --all-targets
```

현재 baseline: **lib 294 + bin 41 tests passing**, clippy clean.

## Package macOS

macOS용 `.app` 및 `.dmg`를 만들려면:

```bash
./scripts/package-macos.sh
```

산출물:

- `dist/mini-browser.app`
- `dist/mini-browser.dmg`

현재 스크립트는 unsigned app을 생성한다. 외부 배포용이면 이후 `codesign`과 notarization을 추가해야 한다.

## Notes

- 단계(Phase)별 task 단위로 commit을 작게 끊는다.
- 각 Phase 종료 시 [docs/roadmap.md](docs/roadmap.md)와 README/AGENTS의 status를 갱신한다.
- "정확하게 동작하는 작은 브라우저"가 1차 목표이고, 표준 100% 호환은 목표가 아니다.
