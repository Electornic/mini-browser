# Project Spec

## Goal

이 프로젝트는 학습과 실험을 목적으로 한 미니 브라우저 구현이다. 목표는 다음 파이프라인을 실제 코드로 연결하는 것이다.

```text
URL
  -> document download (HTTP/HTTPS, redirect, keep-alive)
  -> HTML parse
  -> DOM tree build (NodeId arena)
  -> CSS parse
  -> style resolve (cascade + inheritance + pseudo-state)
  -> JS execution (Boa engine, document/window globals, 이벤트)
  -> layout (block / inline / inline-block / position / float / flex / grid)
  -> render (rect / rounded-rect / text / image / gradient / shadow / transform)
  -> window
```

## In Scope (Phase 0–2 Done)

### Runtime

- 단일 프로세스 브라우저 실행
- 단일 창(Window) 기반 렌더링 (`minifb`)
- redraw 중심 event loop, 매 프레임마다 입력 처리 + JS job drain + layout/paint

### Document

- 잘 형식이 맞는 HTML 입력 + 자주 쓰는 태그 (`<a>`, `<span>`, `<img>`, `<script>`, `<link>`, …)
- NodeId 기반 Document arena (tombstone subtree, JS mutation 공유)
- 속성 파싱 (`id`, `class`, `href`, `src`, `rel`, `style`, `data-*`, …)

### CSS

- Selector: tag, `.class`, `#id`, descendant (` `), child (`>`), pseudo-class (`:hover`/`:focus`/`:active`)
- 단위: `px`, `%`, `em`, `rem`
- 색상: hex (`#RGB`/`#RRGGBB`), `rgb()`/`rgba()`, named (HTML4 16색 + transparent)
- Property set: `display`, `width`/`height`, `margin-*`/`padding-*`/`border-*` (+ `border-radius` 4-value), `background-color`, `background-image` (gradient), `color`, `font-size`, `text-align`, `line-height`, `position`/`top`/`right`/`bottom`/`left`, `z-index`, `float`/`clear`, `opacity`, `box-shadow`, `text-shadow`, `transform`, **flex** 일족, **grid** 일족
- Inheritance + cascade (specificity: id/class/tag), pseudo-state (`:hover`/`:focus`/`:active`)
- 일부 기본 태그 스타일 (`body`, `p`, `h1`, `a`, …) — UA defaults

### Layout

- block / inline / inline-block 기본 흐름
- `position: relative` / `absolute` / `fixed` (containing-block 추적, percent resolve)
- Stacking context + `z-index`, float + `clear`, margin collapse, `line-height` (inline)
- **Flexbox**: `flex-direction`, `justify-content`, `align-items`, `flex-grow`/`shrink`/`basis`
- **Grid**: `grid-template-columns/-rows` (`<length>` / `<n>fr` / `auto`), `grid-area`, named lines

### Rendering

- Display primitive: `SolidRect` / `RoundedRect` / `Text` / `Image` / `LinearGradient` / `RadialGradient` / `BoxShadow` / `TransformGroup`
- `opacity` (alpha compositing), `transform: translate / scale / rotate` (per-pixel inverse transform sampling)

### Networking

- HTTP/HTTPS GET, redirect 추적, **keep-alive 풀**, chunked transfer decoding
- HTML / stylesheet / 이미지 / 스크립트 / web font (`@font-face`) 병렬 fetch (`thread::scope`)
- 상대 URL 해석, system font fallback (한글 표시)

### JavaScript (Phase 2)

- Boa 0.21 임베드, `<script>` inline + external `src` 자동 실행, 페이지 단위 globals 보존
- 글로벌: `console.log/warn/error`, `globalThis`/`window`/`self` alias, `document`
- DOM read: `getElementById`, `querySelector` (descendant + child), `tagName`, `textContent`, `children`, `getAttribute`, `nodeType`
- DOM mutate: `createElement`, `createTextNode`, `appendChild`, `removeChild`, `insertBefore`, `replaceChild`, `cloneNode`, `setAttribute`, `textContent=`
- 이벤트: `addEventListener('click', …)` + bubble dispatch (text → Element retarget, mid-bubble removal 안전)
- 비동기: `setTimeout` / `setInterval` / `requestAnimationFrame` + Promise microtask drain. 커스텀 `FrameJobExecutor` 가 비블로킹 — due 인 timeout 만 발사

## Phase 3 — Backlog (아직 미구현)

다음 항목은 Phase 3 으로 분리되어 backlog 에 둠:

- `event.preventDefault` / `stopPropagation` / `currentTarget`
- 키보드 / 포커스 이벤트 (`keydown` / `keyup` / `focus` / `blur` / `input` / `change`)
- Form input (`<input type=text>` / `<textarea>` / `<button>` / `<form>` submit)
- DOM API 잔여물 (`.innerHTML write`, `classList`, `closest`/`matches`, sibling 탐색)
- Network from JS (`fetch` / `XMLHttpRequest`, `async/await`)
- Browser state stubs (`navigator` / `location` / `history`, `window.addEventListener` / `document.addEventListener`)

## Out of Scope (현재 시점, Phase 4+ 에서 재평가 가능)

- WebGL / Canvas 2D context
- WebSocket, IndexedDB, ServiceWorker, Web Workers
- HTTP/2, HTTP/3, gzip/brotli compression
- 멀티탭 UI, 히스토리 페이지 (chrome://history 같은)
- 폰트 셰이핑 (HarfBuzz 수준), BiDi 정교화
- 실제 캐시 / 쿠키 정교화
- accessibility tree
- malformed HTML 의 본격 복구 (현재는 단순 실패 + 에러 페이지)

## Assumptions

- 초기 입력은 비교적 단순하고 예측 가능한 HTML/CSS다.
- malformed document recovery는 초기에 다루지 않는다.
- 네트워크는 우선 단순 성공 케이스 위주로 구현한다.
- 렌더러는 pixel-perfect보다 파이프라인 연결이 우선이다.

## Non-Goals For First Version

- 웹 표준 완전 준수
- 성능 최적화
- 멀티스레드 로딩
- 복잡한 텍스트 shaping
- 고급 폰트 관리

## Success Criteria

- 로컬 문자열 HTML/CSS를 파싱해 창에 텍스트와 배경을 렌더할 수 있다.
- 원격 URL에서 HTML을 다운로드하고 stylesheet를 추가 로드할 수 있다.
- DOM, style, layout, render 단계가 명확히 분리되어 있다.
- 각 단계는 독립 테스트 또는 디버그 출력으로 검증 가능하다.

## Related Documents

- [Architecture](architecture.md)
- [Data Model](data-model.md)
- [Roadmap](roadmap.md)
