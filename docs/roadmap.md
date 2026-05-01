# Roadmap

이 프로젝트는 학습 목적의 Rust 미니 브라우저로, 단계(Phase) 단위로 범위를 늘려간다. 각 Phase는 독립적인 commit/PR 단위로 진행 가능한 task로 쪼개져 있고, 각 task는 학습 가치(Learning Value)와 예상 작업량(Effort)을 적어 우선순위를 가늠하기 쉽게 한다.

## Phase Overview

| Phase | Status | Theme | Outcome |
|---|---|---|---|
| 0 | Done | Block layout + Chrome-style UI | URL 인자 없이 실행 시 Chrome NTP 모양의 시작 페이지가 보인다 |
| 1A | Done | Values & Selectors | %/em/rem 단위, named/rgb/rgba 색상, descendant/child 셀렉터, :hover/:focus/:active |
| 1B | Done | Layout Modes | inline-block ✅, position: relative/absolute/fixed ✅, stacking/z-index ✅, margin collapse ✅, line-height ✅, float ✅ |
| 1C | In progress | Paint | opacity ✅, linear-gradient ✅, radial-gradient ✅, box-shadow ✅, text-shadow ✅, transform |
| 1D | Backlog | Big Layout | Flexbox / Grid |
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

### 1A. Values & Selectors (Done)

| Task | Status | Notes |
|---|---|---|
| Length units: `%`, `em`, `rem` | Done | em/rem은 style 단계에서, %는 layout 단계에서 resolve. 높이 % 는 parent_width로 근사 (TODO) |
| Color formats: `rgb()`, `rgba()`, named (red/blue) | Done | HTML4 16색 + 자주 쓰이는 몇 개 + transparent. legacy comma-separated rgb/rgba |
| Descendant selector (`.a .b`) | Done | `Selector` 구조체 + 우측에서 좌측 매칭, ancestor chain plumbing |
| Child selector (`.a > .b`) | Done | `Combinator` enum. `>`(공백 무관) 인식, immediate parent만 검사 |
| `:hover` pseudo-class | Done | DOM path 식별자 기반, 1-frame lag, hover ancestor 전파 |
| `:focus`, `:active` | Done | `PseudoState` 구조체 통합. focus는 비전파, active는 hover처럼 전파. WindowInput에 `left_mouse_held` 추가 |

### 1B. Layout Modes

| Task | Status | Files | Effort | Value | Dependencies |
|---|---|---|---|---|---|
| `display: inline-block` | Done | layout.rs | 3-5d | ★★★ | inline-flow 안에서 size/placement 분기. 미지정 width는 shrink-to-fit (자식 너비 합 + parent cap) |
| `position: relative` | Done | layout.rs | 1w | ★★★ | normal layout 후 subtree 전체를 (dx, dy) 평행이동. sibling cursor·line packing은 unoffset 좌표 그대로. 양쪽 set 시 left/top 우선. block/inline/inline-block 모두 지원 |
| `position: absolute` | Done | layout.rs | 1-2w | ★★★ | 2-pass: pass 1은 static layout(흐름 cursor 안 움직임), pass 2는 containing-block 스택을 들고 트리를 다시 걸어 outer edge를 (cb_x+left, cb_y+top) 또는 right/bottom 기준으로 이동. 가장 가까운 positioned 조상의 padding box가 cb, 없으면 viewport. percent left/right는 cb.width, top/bottom은 cb.height에 resolve. block/inline 양쪽 + 중첩 absolute 지원 |
| `position: fixed` | Done | layout.rs | 3d | ★★ | absolute의 분기 — `is_out_of_flow`로 inflow 제외 통합, pass 2가 `initial_cb`(viewport)도 함께 들고 다니며 fixed 노드만 그쪽으로 resolve. percent도 viewport 기준 |
| Stacking context / `z-index` | Done | render.rs, css.rs | 1w | ★★★ | render에 stacking-context paint pass 도입: 자기 bg/border/text → 음의 z-index → in-flow 자손(positioned subtree skip) → zero/auto z-index → 양의 z-index. z-layer 정렬은 stable sort라 동일 z 안에서는 tree order. CSS 파서에 `Value::Number` + 음수/단위없는 숫자 파싱 추가. positioned 박스마다 자체 stacking context 생성 (auto와 0이 사실상 동일하게 취급되는 것은 toy 단순화) |
| Float layout (`float: left/right`) | Done | layout.rs | 1w | ★★ | block-children 루프에 left/right column 트래커 추가 — float은 in-flow cursor 안 움직이고 같은 쪽 끼리 가로 stack. `clear: left/right/both`은 다음 block의 cursor를 해당 쪽 float bottom으로 점프시키고 column 리셋. 부모 height auto-extend로 float 포함. **미구현**: inline content wrap-around (line shortening), float on inline parent (block 부모 안에서만 동작) |
| Margin collapse | Done | layout.rs | 1w | ★★ | adjacent in-flow block sibling 사이의 vertical margin을 spec대로 결합: 둘 다 양수면 max, 둘 다 음수면 min, 혼합이면 sum. block-children 루프가 이전 in-flow 자식의 `margin-bottom`을 추적하다가 다음 자식의 `margin-top`을 collapse 후 cursor에서 overlap 만큼 빼준다. out-of-flow 자식은 chain 끊지 않음. parent-child collapse(rule #2)는 미구현 |
| `line-height` (inline) | Done | layout.rs, render.rs, style.rs | 3-5d | ★★ | inline 텍스트 높이가 글리프 크기 대신 line-height 박스로 확장. number는 자식의 own font-size에 곱(상속), length는 절대값, percent는 own font-size 기준. line box는 max child line-height. render에서 글리프를 half-leading만큼 내려 박스 안에 시각적 가운데 정렬. `vertical-align`은 미구현 |

### 1C. Paint

| Task | Status | Files | Effort | Value | Dependencies |
|---|---|---|---|---|---|
| `opacity` / alpha compositing | Done | render.rs | 3-5d | ★★ | paint pass에 inherited alpha 스레딩, 각 노드의 effective = inherited × own. positioned 자손은 collect 시점에 ancestor chain의 cumulative alpha를 함께 저장해 stacking context 분리에도 chain 보존. fill_rect/fill_rounded_rect에 source-over 블렌딩 추가, fontdue 글리프 coverage에 color.a 곱해 글리프 자체도 attenuate. **간이화**: 부모 단위 compositing group(offscreen buffer)이 아닌 노드별 곱셈이라 겹친 자식들은 진짜 한 그룹으로 합쳐지지 않음 |
| Linear gradient | Done | render.rs, css.rs | 1w | ★★ | css 파서에 `linear-gradient(...)` 함수 인식 + `Value::Gradient` 추가. `LinearGradient { direction, stops }`, stop은 `(color, Option<f32> position)`. 방향은 `to top/bottom/left/right`(default bottom). render에 `DisplayCommand::LinearGradient` + `gradient_command` emitter (background-image 읽음). 파라미터 alpha까지 적용. raster는 픽셀별 progress 계산 → 인접 stop 사이 RGB lerp → source-over 블렌드. auto position은 paint emit 시점에 spec 룰대로 분배 (1번/마지막 0/1, 사이 빠진 곳 균등). **간이화**: angle/corner direction, conic/radial은 미구현 |
| Radial gradient | Done | render.rs, css.rs | 3-5d | ★ | css에 `radial-gradient(<stops>)` 파서 추가, 기존 `LinearGradient` 타입을 `Gradient { kind, stops }` + `GradientKind { Linear(direction), Radial }`로 일반화. render의 `DisplayCommand::LinearGradient`도 `Gradient`로 통합되어 single fill_gradient 함수가 kind에 따라 progress 계산 분기. radial은 ellipse(rect 중앙, 반지름 rect/2) 기준 normalised distance √((nx)²+(ny)²)를 progress로 사용. **간이화**: shape/size/position 인자는 미파싱 — 항상 ellipse-at-center, farthest-corner |
| `box-shadow` | Done | render.rs, css.rs | 1w | ★★ | css에 `Value::BoxShadow`(offset/blur/spread/color) + `parse_box_shadow_value`. parse_declaration에서 `box-shadow` 이름이면 special-case로 dispatch (border-radius와 동일 패턴). render는 `DisplayCommand::BoxShadow` + `shadow_command` (border-box를 offset+spread로 변형) + paint_self가 shadow → bg → gradient → border → text 순서로 emit. raster는 픽셀별 distance-to-rect 기반 linear-ramp coverage, source-over blend. **간이화**: outset only(no inset), single shadow only(no comma list), Gaussian 대신 linear-ramp 근사, border-radius와 무관한 sharp corner |
| `text-shadow` | Done | render.rs, css.rs, style.rs | 3d | ★ | css에 `Value::TextShadow`(offset/blur/color) + `parse_text_shadow_value`. style 인헤리턴스 화이트리스트에 `text-shadow` 추가 (자식 텍스트가 부모 declaration 받음). render는 `paint_self`에 `text_shadow_command`를 `text_command` 직전에 호출 — 텍스트 노드일 때만 두 번째 Text 커맨드를 offset 위치 + shadow 색으로 push. **간이화**: `blur_radius`는 파싱하지만 라스터에선 무시 (글리프 blur는 별도 raster-then-blur 패스 필요), single shadow only |
| `transform: translate/scale/rotate` | Backlog | render.rs, layout.rs | 2-3w | ★★★ | affine matrix, hit-test 재설계 |

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
