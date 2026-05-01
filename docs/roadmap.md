# Roadmap

이 프로젝트는 학습 목적의 Rust 미니 브라우저로, 단계(Phase) 단위로 범위를 늘려간다. 각 Phase는 독립적인 commit/PR 단위로 진행 가능한 task로 쪼개져 있고, 각 task는 학습 가치(Learning Value)와 예상 작업량(Effort)을 적어 우선순위를 가늠하기 쉽게 한다.

## Phase Overview

| Phase | Status | Theme | Outcome |
|---|---|---|---|
| 0 | Done | Block layout + Chrome-style UI | URL 인자 없이 실행 시 Chrome NTP 모양의 시작 페이지가 보인다 |
| 1 | Backlog | CSS Expansion | 실제 웹페이지에 가까운 CSS 표현력 확보 (selectors, units, advanced layout) |
| 2 | Backlog | JS Engine Integration | Boa 엔진 임베드 + DOM 바인딩, 단순 JS 동작 페이지 렌더 |

## Working Principles

- 작게 구현하고 바로 검증한다 (각 task = 1~3 commit).
- 각 단계는 독립적으로 디버그 가능해야 한다.
- 한 Phase가 끝날 때마다 문서를 갱신한다.
- 의존이 큰 task는 Dependencies 컬럼에 명시한다.

## Phase 0 — Block Layout + Chrome UI (Done)

다음 항목은 모두 main 브랜치에 반영됨.

### 렌더링 파이프라인
- DOM 자료구조, HTML parser, CSS parser, Style Engine
- Block layout engine, inline 흐름 (`a`/`span`/`img`)
- Display command (SolidRect / Text / Image)
- 라스터라이저, 윈도우 이벤트 루프 (`minifb`)

### 네트워크 / 리소스
- HTTP/HTTPS GET + redirect 추적
- `<link rel="stylesheet">`, `<img src>` 자동 로드
- web font (`@font-face`) 로드 + system font fallback (AppleSDGothicNeo)
- 에러 페이지, `text/plain` 페이지 처리

### Chrome v2 (이번 Phase에서 추가됨)
- `RoundedRect` 디스플레이 프리미티브 (4-corner radii)
- pill 모양 주소창 (focus-aware blue ring)
- chevron back/forward 아이콘 + 3-dot 메뉴 (rect-composed)
- 단일 탭 strip (위쪽 corner만 둥근 RoundedRect)
- layout `margin: auto` (가로 정렬)
- layout `text-align: center` (inline-flow alignment)
- CSS `border-radius` 파싱 + 렌더 hookup

### NTP (New Tab Page)
- URL 인자 없이 실행 시 Chrome NTP 모양: 가운데 로고 + pill 검색창 + 4 단축 타일

## Phase 0 Carryover (Done)

Phase 0에서 의도적으로 미뤘던 polish 항목. Phase 1 시작 직전 일괄 정리됨.

| Task | Status | Notes |
|---|---|---|
| 인라인-of-인라인 alignment (`<a>` 안쪽 텍스트 가운데 정렬) | Done | `layout_inline_sequence_no_wrap`이 부모의 `text-align`을 적용하도록 변경. NTP 타일 width를 96px로 키워 가시화 |
| `border-radius` 4-value shorthand (`8px 12px ...`) | Done | css 파서에서 1/2/3/4-value를 4개 코너 프로퍼티(`border-top-left-radius` 등)로 expansion |
| 주소창 placeholder 색 차별화 | Done | 비어 있을 때 회색(154,160,166), 입력 시 BLACK |
| 메뉴 버튼 클릭 hit-test | Done | `ChromeAction::Menu` + hover wash + 임시 status 메시지 (드롭다운 자체는 별도 작업) |
| refresh 아이콘 / 액션 | Done | 12-stop ring + 화살표 합성 아이콘, `reload_current()`로 history 건드리지 않고 재페치. 홈 버튼은 향후 polish로 이월 |

남은 후속 polish 후보 (Phase 1과 병렬 진행 가능, 우선순위 낮음):
- 메뉴 드롭다운 실제 구현
- 홈 버튼
- refresh 진행 중 spinner 상태

## Phase 1 — CSS Expansion (Backlog)

목표: 실제 웹페이지에서 흔히 보는 CSS 기능을 학습 단위로 직접 구현해본다. Phase 1 종료 시 단순 React 랜딩페이지 정도가 거의 깨지지 않고 렌더 가능해야 한다.

### 1A. Values & Selectors

| Task | Files | Effort | Value | Dependencies |
|---|---|---|---|---|
| Length units: `%`, `em`, `rem` | css.rs, layout.rs | 1-2d | ★ | computed/used value 개념 |
| Color formats: `rgb()`, `rgba()`, named (red/blue) | css.rs | 2-3d | ★ | |
| Descendant selector (`.a .b`) | style.rs | 1w | ★★ | |
| Child selector (`.a > .b`) | style.rs | 3d | ★★ | descendant 먼저 |
| `:hover` pseudo-class | style.rs, render.rs | 1w | ★★★ | interaction state, style invalidation |
| `:focus`, `:active` | style.rs | 3d | ★★ | `:hover` 먼저 |

### 1B. Layout Modes

| Task | Files | Effort | Value | Dependencies |
|---|---|---|---|---|
| `display: inline-block` | layout.rs | 3-5d | ★★★ | layout mode dispatch 도입 |
| `position: relative` | layout.rs | 1w | ★★★ | |
| `position: absolute` | layout.rs | 1-2w | ★★★ | containing block 추적 |
| `position: fixed` | layout.rs, main.rs | 3d | ★★ | absolute 먼저 |
| Stacking context / `z-index` | render.rs, layout.rs | 1w | ★★★ | absolute 먼저 |
| Float layout (`float: left/right`) | layout.rs | 1w | ★★ | |
| Margin collapse | layout.rs | 1w | ★★ | |
| `line-height` / `vertical-align` (inline) | layout.rs, render.rs | 1w | ★★ | |

### 1C. Paint

| Task | Files | Effort | Value | Dependencies |
|---|---|---|---|---|
| Linear gradient | render.rs, css.rs | 1w | ★★ | |
| Radial gradient | render.rs | 3-5d | ★ | linear 먼저 |
| `box-shadow` | render.rs | 1w | ★★ | |
| `text-shadow` | render.rs | 3d | ★ | |
| `opacity` / alpha compositing | render.rs | 3-5d | ★★ | |
| `transform: translate/scale/rotate` | render.rs, layout.rs | 2-3w | ★★★ | affine matrix, hit-test 재설계 |

### 1D. Big Layout (마지막)

| Task | Files | Effort | Value | Dependencies |
|---|---|---|---|---|
| **Flexbox 최소 (justify/align)** | layout.rs | 2-4w | ★★★ | Phase 1B 완료 후 |
| Flexbox: `flex-grow/shrink/basis` | layout.rs | 1-2w | ★★★ | minimal 먼저 |
| **Grid: track sizing** | layout.rs | 1-2m | ★★★ | flexbox 완료 후 |
| Grid: areas, line names | layout.rs | 2w | ★★★ | track sizing 먼저 |

## Phase 2 — JS Engine Integration (Backlog)

목표: 정적 HTML/CSS만 그리던 브라우저에 동적 동작을 넣는다. 자체 구현 대신 [Boa](https://crates.io/crates/boa_engine) 임베드 — DOM 바인딩과 reflow trigger가 진짜 학습 포인트.

### 2A. Engine Embed

| Task | Files | Effort | Value | Dependencies |
|---|---|---|---|---|
| `boa_engine` 의존성 추가 + hello-world | Cargo.toml, 새 모듈 | 1-2d | ★ | |
| `<script>` 태그 실행 (HTML 파싱 → script 추출 → 실행) | html.rs, 새 모듈 | 3-5d | ★★ | engine embed 먼저 |
| `console.log` → stderr 바인딩 | js.rs (가칭) | 1d | ★ | engine embed 먼저 |

### 2B. DOM Bindings (read-only first)

| Task | Files | Effort | Value | Dependencies |
|---|---|---|---|---|
| `document`, `window` globals | js.rs, dom.rs | 3-5d | ★★ | engine embed |
| `document.querySelector` | js.rs, style.rs (selector 재사용) | 1w | ★★★ | descendant selector 필요 (Phase 1) |
| `document.getElementById` | js.rs | 2d | ★ | |
| `.textContent` (read) | js.rs | 2d | ★ | |
| `.getAttribute()` | js.rs | 2d | ★ | |

### 2C. DOM Mutation + Reflow

| Task | Files | Effort | Value | Dependencies |
|---|---|---|---|---|
| `document.createElement` | js.rs, dom.rs | 3d | ★★ | |
| `.appendChild`, `.removeChild` | js.rs, dom.rs | 3d | ★★ | createElement |
| Mutation → reflow trigger (스타일/레이아웃 재계산) | main.rs | 1-2w | ★★★ | mutation API 먼저 |
| `.innerHTML` write (fragment HTML 파싱) | html.rs (fragment 모드 추가), js.rs | 1w | ★★★ | |

### 2D. Events & Async

| Task | Files | Effort | Value | Dependencies |
|---|---|---|---|---|
| Event loop (microtask + macrotask 큐) | main.rs, js.rs | 1w | ★★★ | engine embed |
| `addEventListener('click', ...)` | js.rs, main.rs | 1w | ★★★ | event loop |
| `setTimeout`, `setInterval` | js.rs, main.rs | 3-5d | ★★ | event loop |
| `requestAnimationFrame` | js.rs, main.rs | 3d | ★★ | event loop |

### 2E. Network from JS (선택)

| Task | Files | Effort | Value | Dependencies |
|---|---|---|---|---|
| `fetch()` basic GET | js.rs, net.rs | 1w | ★★ | event loop, async |
| `XMLHttpRequest` minimal | js.rs, net.rs | 3-5d | ★ | fetch 먼저 |

## Phase Beyond — 명시적 비포함

다음은 학습 가치 대비 범위 폭증이 너무 커서 현 시점 로드맵에서 제외:

- WebGL / Canvas 2D context
- WebSocket
- IndexedDB, ServiceWorker, Web Workers
- HTTP/2, HTTP/3, gzip/brotli compression
- 멀티탭 UI (단일 탭 유지)
- 히스토리 페이지 (chrome://history 같은)
- 실제 캐시 / 쿠키 정교화
- 폰트 셰이핑 (HarfBuzz 수준), BiDi, line breaking 정교화

이 항목이 필요해지는 시점이 오면 그때 별도 Phase 3+ 로드맵을 작성한다.

## Suggested Work Order

Phase 1은 위 표의 1A → 1B → 1C → 1D 순서가 자연스럽다. 1A는 다른 모든 항목의 기반(단위, 셀렉터)이라 제일 먼저 깔아야 한다. 1D(flex/grid)는 가장 큼.

Phase 2는 2A → 2B → 2C → 2D → 2E 순서.

## Verification Strategy

- parser 계층 (`html`, `css`): unit test 중심
- style/layout 계층: snapshot 또는 구조 비교 중심
- renderer/window 계층: 수동 실행 검증 + display command 비교
- network 계층: integration test 또는 샘플 서버 기반
- JS 계층 (Phase 2): JS 코드를 입력으로 받아 DOM 변경 결과를 비교

각 task 완료 시 다음 4개 커맨드로 검증:

```
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build
```

## Related Documents

- [README.md](../README.md)
- [AGENTS.md](../AGENTS.md)
- [Project Spec](spec.md)
- [Architecture](architecture.md)
- [Data Model](data-model.md)
