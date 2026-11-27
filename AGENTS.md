# AGENTS.md

이 문서는 `/Users/leejun/Desktop/Projects/mini-browser` 이하 전체에 적용된다. 작업하는 에이전트가 layout 을 빨리 파악하고 빌드/테스트/커밋 컨벤션에 맞춰 움직이도록 한다.

## Project Goal

학습용 토이 브라우저 → **에이전트용 브라우저** ([docs/concept.md](docs/concept.md)). 6-crate Cargo workspace (mb-dom / mb-engine / mb-runtime / mb-agent / mb-mcp / mb-shell) 위에 mature crate 들 (html5ever, taffy, cosmic-text, tiny-skia, rquickjs, ...) 을 글루로 엮는다. 작업의 무게중심은 "from-scratch 구현" 이 아니라 **글루 + 기능 보강**.

## Workspace Layout

```
[workspace]                                 외부 dep
├── crates/mb-dom      (no I/O, pure data)  html5ever, cssparser, selectors,
│                                           image, url, precomputed-hash
│   - dom              NodeId arena + tombstone subtree
│   - dom_select       selectors::Element 구현 (matches/closest 진입)
│   - html             html5ever 브릿지 (parse_document / parse_fragment 두 모드)
│   - css              cssparser 기반 stylesheet AST + 토큰 파서 (`css::media` = `@media` 파서·평가)
│   - style            cascade + computed values + StyledNode
│   - resource         LoadedImage 데이터 + decode_image (loaders 는 mb-runtime)
│   - url              Url + NetworkError + parse/resolve/Display
│
├── crates/mb-engine   (layout + paint)     cosmic-text, taffy, tiny-skia
│   - layout/          taffy 브릿지 + boundary block/inline/flex/grid/table
│   - render/          DisplayList paint commands + tiny-skia + swash (`render/raster/` 별도)
│   - chrome           탭 줄 / 주소창 / 뒤로앞 / 메뉴 페인트 명령 + ChromeExtras (임베더 알약 자리)
│   - palette          디자인 토큰 (역할 색·중립색·글자 폭 헬퍼, LIGHT/DARK 두 벌 + 스레드별 테마) — 크롬과 mb-mcp 오버레이가 공유
│   - view             DocumentView 빌더 + hit-testing (per-frame entry)
│   - font_system      shared FontSystem + SwashCache OnceLock 슬롯
│   - input            WindowInput 데이터 + edit_line(주소창·대화상자 공용 한 줄 편집) (winit driver 는 mb-shell)
│   - pub(crate) use mb_dom::{css, dom, resource, style, url} 로 짧은 경로 보존
│
├── crates/mb-runtime  (orchestrator + JS)  rquickjs, ureq, url, selectors, tokio
│   - state/           BrowserState (per-frame loop driver, 거의 모든 모듈 의존)
│   - js/              rquickjs runtime + DOM/event/timer/fetch/xhr/storage 브릿지
│   - net              ureq fetch + Url/NetworkError re-export
│   - resource         parallel scoped fetch + LoadedImage re-export
│   - navigation       URL → Document 로더 (네트워크 → parse → install)
│   - async_runtime    OnceLock<tokio::Runtime> — blocking I/O 를 spawn_blocking 으로 위임
│   - pub use mb_dom::{...} + mb_engine::{...} 으로 외부에 단일 namespace 노출
│
├── crates/mb-agent    (agent interface)    mb-runtime 만
│   - index            DOM·layout 한 바퀴 → 문서 순서 / id·label 맵 / NodeId→rect
│   - role             role 판정 + accessible name (accname 축약판)
│   - observation      observe() → Observation → to_text() (trusted/untrusted 블록, changed/console/dialog)
│   - browser          AgentBrowser — 행동 → 합성 WindowInput(from_agent) → settle → 관찰
│   - error            AgentError (code / message / hint)
│   - text             페이지 문자열 정규화·따옴표 (위조 방지)
│
├── crates/mb-mcp      (binary [[bin]] mb-mcp)  rmcp 3.x, tokio, hyper, image(png), base64, arboard
│   - main             tokio 스레드 = rmcp stdio 또는 HTTP 서버, 메인 스레드 = 브라우저
│   - cli              --viewport / --start-url / --allow-origin / --approve auto|deny|ask / --clock / --seed / --window / --http --port / --theme / --preview(-menu/-pause/-agent-view/-answer/-sites/-sites-add)
│   - http             --http: 127.0.0.1 바인딩, Bearer 토큰·Origin·경로 게이트 → rmcp StreamableHttpService
│   - prompts          서버 instructions + tool 설명 (LLM 이 읽는 텍스트, 영어)
│   - worker           Job 처리, repeat_count, 패닉 시 재생성, 창 모드의 park(승인·넘겨받기·"Your turn" 대기)/answer(넘겨주기 hand back), 거절 영수증·잊기, 일시정지 보관(held)·재개, 런타임 승인 모드, 허용 사이트 편집(목록 밖이 된 카드 → navigation_blocked), 기록 켜기/끄기, 마지막 받은 관찰(seen)
│   - server           #[tool_router] tool 10개 → worker::Request, 결과 → CallToolResult
│   - window_mode      --window: 메인 스레드 winit 루프 + 프레임마다 Job 처리, ⋮ 메뉴·"What the agent sees" 뷰어·"Allowed sites" 대화상자(모달, 칸이 모든 키)·패널/좁은 창 바·시트(route_panel_input) 입력 라우팅, --preview PNG
│   - session_log      --log: 세션 로그(JSONL) 기록 형식 — target·결과 요소 목록·redact (⋮ 메뉴 Record 도 같은 코드, 기본 폴더 mb-runs/)
│   - replay           mb-mcp replay FILE: Replayer(한 단계씩) — worker::execute 로 결정적 재생 + 회귀 지점 diff
│   - replay_window    replay --window: 단계별 재생 뷰 (Next/Back/Run all/Restart), --preview PNG
│   - eval             mb-mcp eval SCENARIO LOGS: 시나리오 판정 + 성공률·에러·실패 지점·안전 위반 집계 (에이전트는 안 돌림)
│   - overlay          ID 배지·대상 테두리·크롬 알약(Pill, REC)·에이전트 패널·카드(승인·Your turn·거절 영수증 등)·좁은 창 바(agent_bar)·시트(sheet)·⋮ 메뉴·관찰 뷰어(agent_view)·허용 사이트(allowed_sites)·재생 패널 (DisplayCommand 순수 함수, palette 색)
│
└── crates/mb-shell    (lib + [[bin]] mb-shell)  anyhow, softbuffer, winit
    - lib.rs           pub mod window — mb-mcp --window 도 같은 창 코드를 씀
    - main.rs          진입점 — install_fonts → window::run(closure)
    - window.rs        winit 이벤트 루프 + softbuffer 프레임 출력 (run = Escape 로 종료, run_with(escape_quits=false) = Escape 를 입력으로)
```

각 crate 는 위에서 아래로만 의존한다. 순환 dep 없음.

## Build / Test / Lint Commands

```sh
# 빠른 빌드 체크
cargo build --workspace 2>&1 | tail -10

# 테스트
cargo test --workspace 2>&1 | grep "test result"

# mb-mcp: 실제 바이너리 e2e (JSON-RPC over stdio / HTTP) + release 빌드
cargo test -p mb-mcp --test e2e
cargo test -p mb-mcp --test http
cargo test -p mb-mcp --test replay   # --log 기록 → mb-mcp replay
# 평가: 시나리오를 claude -p 로 N번 (사용자 터미널) → mb-mcp eval 집계
scripts/eval-run.sh eval/scenarios/todo-add.json 5
cargo build --release -p mb-mcp      # → target/release/mb-mcp (claude mcp add 대상)
# 관전 창 한 프레임을 PNG 로 (GUI 없이 오버레이 확인)
./target/debug/mb-mcp --start-url http://127.0.0.1:8765/ --preview-click e31 --preview /tmp/frame.png

# mb-agent 관찰 골든 (의도한 변경이면 UPDATE_GOLDEN=1 로 갱신 후 diff 를 눈으로 확인)
cargo test -p mb-agent --test golden
UPDATE_GOLDEN=1 cargo test -p mb-agent --test golden

# 통합 테스트만
cargo test --test main_tests -p mb-runtime
cargo test --test html_tests -p mb-runtime

# 린트 + 포맷
cargo clippy --workspace --all-targets
cargo fmt --all
```

테스트 baseline 은 997 (workspace 합계, 2026-11-28 호환성 3차 2.2·2.3 P probe +1; 1c 커스텀 속성·`var()` 는 996, 1b 값 파서는 980). `#[ignore]` 된 퍼저 긴 실행 3개는 합계에 없음. 작업 중 카운트가 변하면 의도된 건지 확인하고 commit message 에 명시.

Rust 1.94.1 핀 (`rust-toolchain.toml`).

## Hot Paths (자주 만지는 파일들)

| File | Lines | 역할 |
|---|---:|---|
| `crates/mb-runtime/src/state/mod.rs` | ~2320 | BrowserState 본체 — 프레임 루프, 입력 디스패치, 캐럿/포커스, view cache 캐싱 |
| `crates/mb-engine/src/layout/mod.rs` | ~2920 | 박스 모델 + boundary 알고리즘 진입점 (`layout/absolute.rs`, `layout/properties.rs` 추출) |
| `crates/mb-engine/src/render/mod.rs` | ~2090 | DisplayCommand 정의 + 페인트 파이프라인 (rasterizer 본체는 `render/raster/` 로 분할) |
| `crates/mb-dom/src/style/mod.rs` | ~1910 | cascade + 상속 + computed values (`style/cascade.rs`, `style/variables.rs` 추출) |
| `crates/mb-dom/src/css/mod.rs` | ~1710 | CSS 토큰 파서 + value 파서 (`css/color.rs`, `css/selector.rs` 추출) |
| `crates/mb-runtime/src/js/mod.rs` | ~1670 | rquickjs runtime 부트스트랩 + 글로벌 등록 |
| `crates/mb-runtime/src/js/element.rs` | ~1640 | Element/Node prototype 접근자 + DOM mutate 훅 |

각 모듈은 `dir/mod.rs` 진입점 + 같은 디렉터리의 sibling 파일로 분할되어 있다. grep 시 옛 flat 경로 (`state.rs`, `css.rs`, `style.rs`) 는 더 이상 존재하지 않음.

## Conventions

### 코드 스타일
- 작은 함수 + 좋은 이름 우선, 주석은 *왜* 만 (무엇은 코드가 말함).
- 모든 변경은 cargo fmt 통과. clippy clean.
- `unsafe` 는 한 곳뿐: `mb-dom/src/resource.rs` `as_bytes_mut`(디코더가 `Vec<u32>` 에 바로 쓰게 하는 바이트 보기, 호환성 3차 R4). 새로 쓰려면 같은 수준의 안전 근거 주석과 리뷰.
- 새 dep 추가는 의식적으로 — crate 분할의 dep 그래프를 깨지 말 것 (mb-dom 에 IO 금지, mb-engine 에 네트워크 금지, mb-agent 는 mb-runtime 만 등).
- 한 crate 안에서 다른 crate 의 데이터 타입을 참조할 때:
  - mb-engine 내부에서는 `crate::css::Color` 처럼 짧게 (`pub(crate) use mb_dom::*` 덕분).
  - mb-runtime 도 동일.
  - 외부 (테스트 / shell) 에서는 `mb_runtime::*` 또는 `mb_dom::*` 로 명시적으로.

### 테스트
- inline `#[cfg(test)] mod tests` (lib unit) + `tests/*.rs` (integration). 둘 다 baseline 에 포함.
- integration tests 는 mb-runtime 의 `pub use` re-export 를 통해 모든 surface 에 접근 (`use mb_runtime::{chrome::..., css, layout, ...}`).
- 새 기능은 가능한 한 unit test 로 커버하고 integration tests 는 cross-cutting 행동 (e.g. 입력 → 화면) 만.

### Commit
- Conventional-commit 형식: `feat(scope): ...` / `refactor(scope): ...` / `chore(scope): ...` / `docs(scope): ...`.
- 한 단위 작업 끝에 cargo test green + clippy clean 확인 후 단일 commit.
- 본문은 *왜* 위주 — 변경의 의도, 트레이드오프, 잔존 이슈.
- Co-Authored-By 트레일러는 자동.

## Gotchas / 함정 모음

라이브러리 통합 중 만났던 함정 — 다시 만나지 않도록 기록.

### 테스트 HTML 과 루트
- `html::parse` 는 `<html>`/`<body>` 가 없으면 fragment 로 파싱해서 **최상위 형제가 각각 루트**가 되고, 화면에 그리는 건 `roots().last()` 뿐이다. `<input>…<script>` 처럼 최상위에 여러 개를 두면 마지막(`<script>`)만 레이아웃 루트가 되어 클릭·getElementById 가 다 빗나간다 → 테스트 HTML 은 `<div>` 하나로 감싸거나 완전한 문서로 쓸 것.
- 완전한 문서(원문에 `<html`)의 루트는 **`<html>` 하나**다 — `<head>`·`<body>` 는 그 자식(호환성 3차 1r 에서 고침; 전엔 `<html>` 을 버려 `:root` 가 `<head>` 에만 맞았다). `<body>`/`<head>` 는 인덱스가 아니라 `Document::body()`/`head()`/`document_element()` 로 찾을 것. `<html` 없이 `<head>`/`<body>` 만 쓴 원문은 옛 동작(쓴 래퍼가 루트, 문서 요소 없음) — 픽스처에서 `:root`·`html{}`·rem 을 시험하려면 `<html>` 을 쓸 것. 커스텀 속성(`--*`)은 `StyledNode` 의 맵에 없다(`style/variables.rs::CustomScope` 사슬로만 상속).

### 퍼센트 높이는 확정 포함 블록 높이 기준 (호환성 3차 1p)
- 레이아웃 코드에서 `height`/`min-height`/`max-height` 를 `length_value(node, "height", <폭>)` 로 읽지 말 것 — `definite_height(node)`/`height_value`/`clamp_height`(`layout/properties.rs`)를 쓴다. 포함 블록 높이는 패스 범위 상태(`containing_block_height()`)로 내려온다: 상자가 자식을 배치하기 **전에** 자기 확정 높이를 `with_containing_block_height` 로 깔고, 그 안에서 컨테이너 알고리즘은 이 값을 "내 확정 높이"로 읽는다. 자식을 배치하는 새 경로를 만들면 같은 범위를 깔 것(안 깔면 % 가 바깥 상자 기준으로 풀린다). 루트의 기준은 뷰포트 높이(`layout_tree_with_viewport`), absolute 의 % 는 2패스(`reposition_absolutes`)가 포함 블록 패딩 상자로 다시 푼다. out-of-flow 자식은 `layout_out_of_flow`(블록화·shrink-to-fit)로 놓는다 — 인라인 상자로 놓지 말 것.
- 레이아웃이 뷰포트 높이를 읽는 건 루트에 % 높이가 있을 때뿐이고, 그때만 뷰 캐시가 높이를 비교한다(`DocumentView::reads_viewport_height`). 다른 경로로 높이를 읽게 되면 이 플래그를 같이 고칠 것.
- 단위 없는 `0` 은 길이 속성에서 파싱 때 `Length(0, Px)` 가 된다(1q, `css::unitless_zero_as_length` — `var()` 치환 뒤에도). 레이아웃·taffy 브릿지는 `Number` 를 길이로 읽지 않으니, 새 길이 속성을 전용 파서로 읽게 되면 같은 승격을 거칠 것(`line-height` 처럼 숫자가 뜻을 갖는 속성은 제외).
- 페이지 높이는 `DocumentView::document_height` 하나다(1q — 넘친 내용 포함, 캔버스 제외). 스크롤 상한·관찰 `page_height` 가 이것을 쓴다; 루트 상자 높이(`layout_root`)나 `view::document_height(commands)` 를 따로 쓰지 말 것.

### `element.rs` 에 NUL 바이트
- `crates/mb-runtime/src/js/element.rs` 주석에 `"\0STALE"` 이 리터럴 NUL 로 들어 있어(첫 8KB), 에디터·도구에 따라 바이너리로 취급된다 (Claude Code 의 Edit 이 거부, 일부 grep 은 조용히 건너뜀). 수정은 인코딩을 보존하는 스크립트로, 검색은 `grep -a`.

### 인라인 런은 펼쳐진다
- 블록 흐름 부모의 연속된 인라인 수준 자식(텍스트·`span`·`label`·`input`·`button`…)은 `inline::group_block_children` 가 런으로 묶어 줄 상자로 배치한다. 레이아웃 트리에 익명 래퍼는 없다 — 런의 박스들이 부모 children 에 바로 들어간다. taffy 브릿지에선 런 하나가 boundary leaf 하나(`BoundaryContent::InlineRun`)이고 `walk` 가 펼친다. `build_taffy_node` 와 `walk` 는 같은 `native_child_slots` 로 짝지어야 한다.
- 경계 잎(legacy 가 배치하는 서브트리)은 taffy 에게 `boundary_taffy_style` 로만 보인다 — flex/grid 부모가 읽는 항목 속성(`flex-grow/shrink/basis`·`align-self`·`grid-row/column`)을 거기 넘겨야 하고, taffy 가 measure 에 주는 확정 폭은 **테두리 상자**용(자기 여백을 이미 뺌)이라 legacy 에는 여백을 더해 넘긴다(`measure_boundary`, 호환성 3차 R2). 크기 탐색(MaxContent/MinContent)은 1e6/0 대리 폭으로 배치한 뒤 줄 폭으로 다시 잰다(`probe_sizes_to_lines`).

### JS 래퍼는 노드마다 하나
- `__mb_make_element(id)` 는 같은 id 에 항상 같은 객체를 돌려준다 (Preact 가 노드 expando 를 씀). 래퍼에 상태를 달아도 되지만, 문서가 바뀌면 런타임째 버려진다.

### 레이아웃 경로 ≠ DOM 경로
- 레이아웃 트리는 `display:none` 자식(`<head>`, `<script>`, `<style>`)과 공백 텍스트를 뺀다. 그래서 레이아웃 자식 인덱스로 만든 경로를 DOM 에서 풀면 엉뚱한 노드가 나온다 (G1 에서 hover/포커스가 이 버그였음). 노드를 가리킬 땐 **NodeId** 를, 경로가 필요하면 `dom_path_for_node` 로 DOM 기준 경로를 만든다.

### 창 크기는 CSS px (논리), 버퍼는 물리 픽셀
- `mb_shell::window::run` 의 콜백은 `Frame` 을 받는다: `frame.width/height` 와 `WindowInput.mouse_position` 은 **논리(CSS px)**, 그리기는 반드시 `frame.present(&commands)` 로 (기기 배율만큼 `scale_display_list` 후 물리 버퍼에 래스터). 물리 크기를 레이아웃에 넘기면 Retina 에서 글자가 반 크기가 되고 에이전트 관찰도 달라진다.

### NodeId 는 문서마다 재사용
- 이동·뒤로·새로고침마다 새 `Document` 가 생기고 NodeId 가 0 부터 다시 매겨진다. NodeId 를 문서 밖으로 들고 다니면(에이전트 요소 ID 등) 반드시 `BrowserState::document_generation()` 과 짝지을 것.

### Workspace split
- **orphan impl**: 타입을 다른 crate 로 옮기면 그 타입의 inherent impl 도 같이 옮겨야 함 (E0116). `impl Color { WHITE/BLACK }` 가 별도 파일에 있어서 마이그레이션 중 깨졌음.
- **unused-import warning**: `pub(crate) use mb_dom::html` 처럼 `#[cfg(test)]` 만 쓰는 경우 일반 빌드에서 경고. `#[cfg(test)] pub(crate) use ...` 로 분리.
- **`cargo build` vs `cargo build --tests`**: 일반 빌드만 통과하고 테스트 빌드에서 unresolved import 가 나는 케이스 있음. CI 전에 항상 `--tests` 로 한 번 더 확인.

### rquickjs
- closure 에서 `Ctx<'js>` + 다른 lifetime-bearing arg 를 같이 받으면 HRTB 통합 불가. 해결: **flat-hook 패턴** — 모든 Rust 콜백은 primitive (u32/String/bool/Vec) 만 받고 wrapper instance 는 JS bootstrap 이 `Object.defineProperty` 로 조립.
- `IntoJs` 가 plain tuple 미지원 — `convert::List<(...)>` wrapper 필요.

### cssparser 0.38 / selectors 0.42
- `Parser<'i, 't>` → `Parser<'i>`, `ParserInput` 제거 (`Parser::new(&str)` 직접). `ParseError`/`BasicParseError` 에서 lifetime 과 `location` 이 빠졌고, `UnexpectedToken`/`AtRuleInvalid` 는 payload 없는 unit variant → 우리 `css::ParseError.position` 은 basic 에러에서 0.
- `input.new_custom_error(e)` → `cssparser::ParseError::custom(e)`.
- selectors: `NonTSPseudoClass`/`PseudoElement` 의 `type Impl` 제거, `parse_non_ts_pseudo_class(&self, name: &str)` (location 인자 없음), `matches_selector` 는 element 를 by-value 로 받음 (`MatchingElement` 는 Copy).
- 둘은 버전이 묶여 있다 (selectors 가 cssparser 를 re-export 타입으로 노출) — 항상 같이 올릴 것.

### taffy 0.15
- `AlignItems`/`AlignContent`/`JustifyContent` 가 enum → struct + 상수 (`AlignItems::FLEX_START`). `align_items`/`align_content`/`justify_content` 필드는 `Option` 아님 (미지정 = `NORMAL`).
- `min_size`/`max_size` 는 `Size<LengthPercentageAuto>` (`size` 는 여전히 `Dimension`).
- `compute_layout_with_measure` 콜백이 `(LayoutInput, NodeId, ctx, &Style) -> LayoutOutput` — leaf 는 `taffy::compute_leaf_layout(inputs, style, |_, _| 0.0, measure)` 로 감싼다.

### tiny-skia
- `Paint::default()` 의 `anti_alias = true` (docs 와 다름). 픽셀 핀 테스트가 partial coverage 로 깨지므로 `paint.anti_alias = false` 명시 필요.
- `gradient_transform` 은 `local → canvas` 방향.

### winit
- `mb_shell::window::run` 은 **Escape = 창 종료**다. 에이전트 세션을 쥔 창(`mb-mcp --window`)은 `run_with(…, escape_quits: false, …)` 로 Escape 를 `WindowInput::escape_pressed` 로 받는다 — 안 그러면 Escape 한 번에 이후 호출이 전부 `browser_closed`.
- winit `KeyEvent.text` 가 control char 도 흘려보냄 — `is_control()` 필터 안 걸면 input 박스에 invisible char 박힘.
- GUI 검증은 메인 스레드 blocking 이라 백그라운드 task 로 못 돌림. `cargo run --bin mb-shell` 로 직접 확인.

### ureq
- ureq 는 헤더 이름을 wire 에 lowercase 로 보냄 (RFC 9110 §5.1 적합). 헤더 contains 검증 테스트는 `to_ascii_lowercase()` 후 비교.
- redirect 정책: 301/302/303 → GET 다운그레이드 + body drop, 307/308 → method+body 유지.
- body cap 50 MB (`limit().read_to_vec`).

### 디자인 토큰
- 크롬·오버레이 색은 `mb_runtime::palette` 에서만 가져온다 (시안 "Agent Browser" 보드 1). 주황(사람) 위 글자는 늘 ink. 새 색이 필요하면 palette 에 이름을 붙여 추가 — `Palette` 필드 + `LIGHT`·`DARK` 두 값 + `tokens!` 목록 (docs/concept.md "다크 모드").
- 토큰은 함수다(`p::ink()`): 지금 스레드의 테마 값. `const`/`static` 에 담거나 프레임을 넘겨 캐시하지 말 것(테마 전환이 안 먹는다) — 상태 문구처럼 오래 들고 있을 색은 `palette::Tone` 으로. 역할 색 채우기 위 글자는 `on_fill`(`page` 는 다크에서 검정). 에이전트에게 가는 픽셀(스크린샷)은 `with_theme(Light)` 안에서 그린다.
- 관전 창의 대화상자(뷰어·허용 사이트)는 모달이다 — 열린 프레임엔 `page_input` 을 비워 클릭·키가 페이지·주소창에 안 가게 한다. 새 대화상자를 만들면 같은 자리(`window_mode` 의 모달 라우팅)에 넣고, 글자 칸은 `input::edit_line` 을 쓸 것.
- 오버레이 글자에 `⋮` 같은 기호를 쓰지 말 것 — 패널 폰트에 글리프가 없어 깨진다. ✓/✗ 도 글리프 대신 `overlay` 의 그린 아이콘.
- ID 배지 자리는 `overlay::place_badges`(순수 함수)가 정한다 — 배지를 그리는 새 경로를 만들면 `text_obstacles` 와 `page_area` 를 같이 넘길 것 (안 넘기면 배지가 글자·크롬을 덮는다). 프레임마다 도는 코드라 문서 전체를 훑는 일(화면 밖 글자 측정 등)은 먼저 위치로 거를 것. 규칙은 docs/concept.md "배지 자리".
- `CHROME_HEIGHT` 는 에이전트 클릭 좌표·스크린샷 자르기·창 크기와 묶여 있다. 크롬 줄을 창 종류마다 다르게 하거나 아래쪽 바를 두지 말 것 (docs/concept.md "디자인 적용"). 페이지 위에 띠를 겹쳐 그리는 것도 안 된다(에이전트만 보는 영역이 생김). 크롬에 임베더 것을 넣을 자리는 툴바 `accessory_width`(알약)와 탭 줄 `strip_reserved`(좁은 창의 에이전트 바) 둘 — docs/concept.md "좁은 창".

### CSS 파서는 조용히 버린다
- 선언 값은 `parse_entirely` 안에서 읽혀서, 전용 파서가 없는 속성은 토큰이 둘 이상이면(`border: 1px solid`, 쉼표 목록, `calc()`) **선언 통째로** 사라진다. `!important` 는 1a 부터 파싱된다 — 값 파서는 `!` 앞까지만 보고(`parse_until_before(Bang)`), 플래그는 `Declaration::important`, 캐스케이드는 두 패스(작성자 일반 < 인라인 일반 < 작성자 중요 < 인라인 중요). 새 값 파서도 `!` 를 신경 쓸 필요 없다. 지원 안 하는 의사 클래스 하나가 있으면 **규칙 통째로**(콤마로 묶인 셀렉터까지), `@media` 외 at-rule 은 **본문 통째로**(`@layer`·`@supports` 포함 — 2.2·2.3 전까지). 실사이트에서 "왜 이 스타일이 안 먹지?" 는 먼저 `css::parse_with_report` 로 확인.

### 테두리 키와 CSS 전역 키워드 (호환성 3차 1b)
- 테두리는 표준 longhand 이름이 아니다: 폭 = `border-{side}`(레이아웃의 `edge_sizes(node, "border")` 가 읽음), 스타일 = `border-{side}-style`, 색 = `border-{side}-color`(페인터가 단일 `border-color` 보다 먼저). `border`·`border-{side}`·`border-width/-style/-color`·`border-{side}-width` 파서가 이 키로 펼친다. 스타일 패스가 명시적 `none`/`hidden` 면의 폭을 0 으로 만든다 — 스타일 키가 없는 폭(UA 기본)은 그대로 그려진다. 진짜 longhand 체계로의 전환은 4차 후보.
- `inherit/initial/unset/revert/revert-layer` 와 `all` 은 스타일 패스(`resolve_css_wide_keywords`)와 캐스케이드(`apply_all`)가 푼다 — 소비자 코드가 `Keyword("inherit")` 를 볼 일은 없다. "초기값" = 맵에 값 없음(소비자의 기본값). 상속 속성 목록은 `style::INHERITED_PROPERTIES` 하나 — 상속을 추가하면 거기에.
- 새 shorthand 를 파싱하면 `css::longhands_of` 에도 넣을 것(전역 키워드가 longhand 마다 가도록).

### `var()` 는 계산값 시점에 (호환성 3차 1c)
- `--*` 와 `var(` 가 든 값은 `Value::Unparsed { shorthand, raw }`(원문)로 캐스케이드를 지나고, 스타일 패스가 치환 → 재파싱한다(`style/variables.rs`). 소비자 코드는 `Unparsed`·`Var` 를 볼 일이 없다. 실패는 `unset`(앞선 선언으로 안 돌아감). 커스텀 속성은 정의한 노드에서 치환된 글로 상속(`CustomScope`). 새 shorthand 파서를 만들면 `css::longhands_of` 에 넣어야 `var()` 가 든 그 shorthand 가 longhand 마다 펼쳐진다.
- cssparser 의 `parse_nested_block` 은 클로저를 **`parse_entirely`** 로 돌린다 — 블록의 남은 토큰을 다 소비하지 않고 `Ok` 를 돌려주면 에러가 된다(1c 의 `var(--x, 폴백)` 버그). 일찍 끝내는 클로저는 나머지를 `next_including_whitespace_and_comments` 로 비울 것. 또 파서마다 중첩 블록 상한(`REASONABLE_NESTED_BLOCK_LIMIT` 75)이 있지만 **새 `Parser` 는 0 부터** 다시 센다 — 글을 잘라 새 파서로 재귀하는 걸음(폴백 치환)은 그 상한의 보호를 못 받으니 깊이를 직접 넘길 것.
- 치환 결과는 `MAX_SUBSTITUTED_LEN`(64KiB)을 넘으면 invalid — 두 배씩 커지는 `var()` 사슬 방어. 치환·`var(` 탐지 걸음은 블록·폴백·사슬 고리마다 재귀하므로 깊이를 넘겨 받아 `css::MAX_VAR_NESTING` 에서 멈춘다 — 새 재귀 걸음을 더하면 같은 깊이를 지나게 하고 `crates/mb-dom/tests/var_depth.rs`(자식 프로세스·2MB 스레드)에 모양을 더할 것.

### `@media` 는 평가된다 (호환성 3차 2.1)
- 규칙은 `Rule::media`(감싼 블록, 중첩은 `parent` 사슬 = AND)를 달고, 스타일 패스가 시작할 때 `media::evaluate_blocks` 로 블록마다 한 번 평가한다 — 노드마다 평가하지 말 것. 평가 입력은 `MediaContext`(페이지 뷰포트 = 창 − `CHROME_HEIGHT`)뿐, 나머지는 상수(라이트·hover·fine·1dppx·scripting) — 관전 창의 Retina·OS 테마가 페이지에 새면 안 된다(결정성). `matchMedia` 를 만들면 `css::media::evaluate` 를 쓸 것.
- `<link media>`·`<style media>` 는 시트가 한 문자열로 합쳐지므로 `css::media::scope_sheet` 로 `@media` 블록이 되어 들어간다 — 시트를 합치는 새 경로를 만들면 같은 함수를 지날 것.
- 높이는 뷰 캐시 키에 직접 안 들어간다: 키는 `media_results`(블록 결과 비트셋)·`vh_height`(5단계 전엔 0). 스타일이 높이를 새로 읽게 되면(`vh` 해석 등) 이 키를 같이 고칠 것.

### 스크립트 에러 위치
- 페이지 스크립트는 `EvalOptions::filename` 에 **라벨**을 달고 평가된다: 외부 = 원래 `src` 속성 값, 인라인 = `{쿼리 뺀 문서 URL}#inline-{n}`(0부터, 문서 순서). 에러 줄은 `js/error_location.rs` 한 곳에서 만든다 — `{message} (near "callee") at {url}:{line}:{col}`(URL 은 쿼리·프래그먼트 제거, `#inline-n` 만 유지). 최상위 에러는 `execute_with_url` 의 `ScriptError` → `install_document` 가 콘솔 버퍼에 `Uncaught in script: …`, 리스너·타이머·rAF 는 `__mb_report_error` 가 `err.stack` 을 Rust 로 넘겨 같은 함수를 탄다. 새 에러 경로를 만들면 이 둘 중 하나로 — `String(err)` 만 남기지 말 것.
- quickjs-ng 의 `not a function` 열은 호출 모양마다 다른 토큰을 가리킨다(최상위 `a.b()` → `.`, 함수 안 → 속성 이름, `foo()` → 앞 공백, `foo( 1)` → `(` 뒤). `near` 는 셋 다 처리한다 — 열 위치를 한 가지로 가정하지 말 것.
- `near`·메시지는 페이지 문자열이다: page 블록 `console:` 에만, trusted `note:` 엔 절대 넣지 않는다.

### settle 의 두 예산
- `AgentBrowser::settle` 은 프레임 상한(`MAX_SETTLE_FRAMES` 600)과 벽시계(`settle_timeout`) 둘로 끝난다. **이동 대기 프레임은 프레임 상한에 안 센다** — 로드 스레드를 기다리는 건 실제 네트워크 시간이라 벽시계만 자를 수 있다. 세면 ~1.5초보다 느린 사이트가 로드 전 빈 화면(URL 없음, `settled: false`)으로 관찰된다(3차 코퍼스 daum·yna). 테스트 서버는 빠르니 이 차이는 실사이트나 `/slow` 같은 지연 라우트로만 보인다.

### 부트스트랩 JS 심은 페이지가 바꿀 수 있는 것을 읽지 않는다
- 엔진이 JS 로 만든 내장(`queueMicrotask` 등)은 페이지 스크립트와 같은 전역을 공유한다. core-js 같은 폴리필은 `globalThis.Promise` 를 바꾸고, 네이티브 `Promise.prototype` 의 `constructor` 를 지우고 프로토타입을 다시 잇는다. 심이 호출 때 `Promise` 를 찾거나(3차 K3) `then` 의 species 를 거치면(K4) 폴리필로 들어가 무한 고리가 된다(daum·hani·bbc 120초 멈춤). 필요한 내장은 **부트스트랩 때 붙잡고**, 프로미스를 쓸 땐 species 가 안 새게(`constructor: undefined` 를 자기 소유로) — `js/window.rs` 의 `queueMicrotask` 참고.

### rquickjs 기본 eval 은 strict
- `EvalOptions::default()` 는 `strict: true` 다. 페이지 스크립트는 `js::classic_script_options()`(sloppy)로 평가할 것 — 안 그러면 암묵 전역(`RLQ = window.RLQ || []`)이 터진다.

### Font System
- `font_system::shared_font_system()` 가 `None` 이면 unit test 환경 — `font_size * 0.75` fallback 으로 토이 측정.
- `install_fonts()` 가 SwashCache 도 같이 재생성 — 글리프 캐시 키가 FontSystem 의 font id 에 의존하므로.

## Roadmap

- **메인**: [docs/concept.md](docs/concept.md) — 에이전트용 브라우저 (MCP). 로드맵 G0~G5, 결정 기록, 열린 결정이 여기 있다. Phase 를 끝내면 상단 상태 줄 + 로드맵 표 + 결정 기록을 같이 갱신.
- **호환성 트랙**: [docs/spa-roadmap.md](docs/spa-roadmap.md) — SPA 렌더링 Tier 0~3. 에이전트 데모가 깨지는 지점이 나올 때 우선순위를 올린다.

## Agent-track Rules

[concept.md](docs/concept.md) 설계에서 코드 규칙으로 승격된 것들. 어기면 보안·프로토콜이 깨진다.

- **stdout 금지**: stdio MCP 에서 stdout 은 JSON-RPC 전용. crate 의 lib·bin 코드에서 `println!`/`print!` 쓰지 말고 `eprintln!` 사용 (examples·tests 는 예외). `mb-agent`/`mb-mcp` 는 `#![deny(clippy::print_stdout)]`.
- **네비게이션 fetch 는 리다이렉트를 직접 따라간다**: 문서 로드는 `navigation::load_remote_document_guarded`(내부적으로 `net::fetch_without_redirects`)를 쓰고 hop 마다 게이트를 묻는다. 네비게이션에 자동 리다이렉트 fetch(`net::fetch` / `fetch_with_request`)를 쓰면 게이트가 첫 URL 만 보게 된다. (서브리소스 — CSS/이미지/스크립트 — 는 이동이 아니라 해당 없음.)
- **이동은 반드시 정책 게이트를 지난다**: 새 이동 경로(`location.href =`, `window.open`, meta refresh 등)를 추가할 때 `navigate` / `navigate_to_href` / `try_submit_form` 과 같은 `policy::check` 를 통과시킨다. 우회 경로를 만들지 말 것.
- **에이전트 전용 DOM 조작 API 금지**: 에이전트 행동은 합성 `WindowInput` 으로만. 편의를 위해 DOM 을 직접 바꾸는 단축로를 만들지 않는다.
- **사람의 조종은 MCP 로 열지 않는다**: 일시정지·재개·승인 모드 변경·기록 켜기/끄기·거절 잊기(`overlay::Control` → `Worker::{pause, resume, set_approve_mode, start_recording, stop_recording, forget_refusal}`), 허용 사이트 편집(`overlay::WindowAction` → `Worker::set_allow_origins`)과 카드 답(`Worker::answer` — Approve·Deny·넘겨받기·hand back)은 창 입력 전용. `worker::Request`·MCP tool 에 대응하는 경로를 만들지 말 것 — 에이전트가 스스로 멈춤을 풀거나 승인을 Allow 로 바꾸거나 자기 기록을 끄거나 사람의 거절을 지우거나 자기 울타리(허용 사이트)를 넓힐 수 있게 된다.
- **사람이 들여다보는 것은 에이전트의 세계를 바꾸지 않는다**: "What the agent sees" 의 Whole page 같은 창 전용 관찰은 `AgentBrowser::full_snapshot`(ID 발급·console/dialog 소비·`changed_by_person` 기준점 이동 없음)만 쓴다. `observe` 를 부르면 에이전트가 안 본 ID 가 유효해진다.
- **`WindowInput::from_agent` 는 mb-agent 만 켠다**: 셸(사람 입력)은 항상 false. 정책 게이트는 이 플래그로 판정 대상을 가른다.
- **trusted 블록의 `note:` 는 브라우저만 쓴다**: `Observation::notes` 에는 우리가 만든 문장만. 페이지 문자열(이름·텍스트·URL 일부라도)을 넣으면 위조 방지가 깨진다.
- **비밀번호 값은 어디에도 남기지 않는다**: 관찰(길이만)·로그(`<redacted>`)·화면(마스킹). 에이전트가 입력한 텍스트도 stderr 로그에 남기지 않는다(`worker::Request` 의 Debug 가 가림), URL 로그는 쿼리 제외. 예외는 사용자가 켜는 `--log` 세션 로그뿐 — 재생에 필요해 텍스트를 남기되 비밀번호 칸 입력은 `<redacted>`, 값은 재생 때 `--secret` 으로만.
- **재생은 MCP 와 같은 경로**: `replay` 는 `worker::execute` 를 그대로 쓴다. 재생 전용 단축 행동을 만들지 말 것 (사람 행동 재현 `fill_as_person` 만 예외 — 사람 경로 입력).
- **ID 는 관찰만 발급한다**: `AgentBrowser` 의 발급 집합(`issued`)에 넣는 건 관찰을 돌려줄 때뿐. 다른 경로(wait_for 등)가 ID 를 유효하게 만들면 stale 가드가 무너진다.
- **HTTP 는 127.0.0.1 + 토큰 + Origin 거절**: `--http` 의 모든 요청은 `http::gate`(경로·Origin·Bearer)와 rmcp `allowed_hosts`(Host)를 지난다. 외부 인터페이스 바인딩·토큰 없는 경로·Origin 허용을 만들지 말 것. 토큰 원문은 실행 시 stderr 한 번과 클립보드에만 — 화면·`Debug`·타임라인에는 가린 형태로.
- **`panic = "abort"` 로 바꾸지 말 것**: 엔진 패닉 복구가 unwind 에 의존한다.
- **브라우저는 결정적이어야 한다**: 관찰·행동·정책 출력에 `HashMap` 순회 순서, 벽시계, 비시드 난수가 새어 나가면 안 된다. 시간은 엔진 시계(`ClockSource`)만, 순서는 문서 순서로. 비결정성은 LLM 판단에만 허용 ([concept.md#결정성](docs/concept.md#결정성)). **명시적 예외 하나**: `[mb-budget]` 워치독(`state/budget.rs`)이 스타일+레이아웃 시간을 벽시계로 재지만 결과는 stderr 한 줄뿐 — 관찰·행동·정책·세션 로그엔 절대 안 들어간다(리뷰 체크 항목).

## Robustness Rules

[compat-round3.md 0.1](docs/compat-round3.md#01-불변식을-규칙으로) 에서 승격. 2차 실사이트 조사에서 에이전트를 막은 버그는 세 종류였다 — 이동 전체 실패, 패닉, 끝없는 시간. 페이지 HTML·CSS·JS·이미지 바이트·속성 값은 전부 **신뢰할 수 없는 입력**이다.

1. **하위 리소스·요소 하나가 이동 전체를 실패시키지 않는다.** 문서 로드가 실패할 수 있는 건 문서 응답 자체(네트워크 에러, 상태 코드, 지원 안 하는 Content-Type, 인코딩)뿐. CSS·스크립트·이미지·폰트·파비콘·속성 값 하나의 실패는 그 리소스만 건너뛴다. 코드 규칙: `resource.rs` 의 수집·로드 함수는 `Result` 가 아니라 `Vec`/`HashMap` 을 돌려준다(0a 에서 시그니처를 바꿈) — `?` 로 `ResourceError` 를 다시 올리지 말 것. 속성은 `resolve_attr` 로(없음·빈 값·해석 실패 = 건너뜀).
2. **어떤 입력에도 패닉하지 않는다.** 레이아웃·스타일·페인트·파서에서 인덱싱은 `get()`, 정수 나눗셈은 0 검사, `unwrap()`/`expect()` 는 불변식이 코드로 보장될 때만 — 그 자리에 이유를 주석으로. 자주 밟는 것: **문자열을 바이트 인덱스로 자르기**(`&s[0..2]`, `s.len() == 3` 후 슬라이스 — 멀티바이트 글자 한가운데서 패닉; 먼저 `is_ascii()` 또는 `char_indices`), **페이지가 주는 개수로 할당**(`colspan`, `grid-row: 2000000000`, `span N` — HTML/브라우저 상한으로 자를 것: colspan 1000, 그리드 줄 1000), 덧셈 오버플로(debug 에서 패닉). 0c 퍼저로 정한 상한: 계산된 길이·수 ±33,554,431px(`style::MAX_CSS_LENGTH`, Blink `LayoutUnit`)와 `layout_tree` 출력 포화(비유한 좌표 금지), DOM 깊이 256(`dom::MAX_DOM_DEPTH` — 붙은 트리든 떼어 낸 트리든; 파서·`innerHTML` 은 더 깊은 내용을 형제로 **평탄화**, 스크립트 변경(`append_child`·`insert_before`·`replace_child`·`replace_with_text`)은 `TreeError::TooDeep` 로 **거절**·고리는 `TreeError::Cycle` — 새 변경 경로는 이 메서드들을 지나게 하고, 떼어 내기 전에 `check_insert`; 재귀 패스의 스택 넘침은 **abort** 라 `catch_unwind` 로 못 막는다), SVG 래스터 긴 변 4096·요소 깊이 256·내용 범위 ±1e6px·격리 층 넓이 합, 래스터 글자 크기 2048, `@media` 중첩·쿼리 괄호 깊이 32(`css::media::MAX_MEDIA_NESTING` — 넘으면 블록 규칙 버림 / unknown), 값 안 블록·함수·`var()` 폴백 중첩과 커스텀 속성 사슬 32(`css::MAX_VAR_NESTING` — 파싱 때 넘으면 선언 버림, 치환 때 넘으면 IACVT; 일반 값의 함수 중첩도 `parse_function` 의 깊이 카운터로 같은 상한), `var()` 치환 결과 64KiB(`style::variables::MAX_SUBSTITUTED_LEN`). 최후 방어선 `mb-mcp/src/worker.rs` 의 `catch_unwind`(브라우저 재생성)는 **세션 손실**이라 해결책이 아니다.
3. **스타일·레이아웃은 시간 예산 안에서 끝난다.** 노드 수에 대해 거의 선형 — 자식을 두 번 훑는 재귀 금지(2차 C3 의 2^깊이), 같은 서브트리를 다시 재는 측정은 메모하거나 한 번에(예: 인라인 크기는 `layout_tree` 한 번 동안 `(NodeId, 폭)` 으로 메모 — 호환성 3차 R1; 키에 `StyledNode` 주소를 쓰지 말 것, 사본이 패스 중에 해제·재사용된다). 예산 초과는 감지해서 보고한다: `BrowserState` 가 `build_document_view` 를 재고 release 2초(debug ×10)를 넘으면 stderr 에 `[mb-budget] style+layout took {ms}ms on {쿼리 뺀 URL} ({n} nodes, {µs}µs/node)`. 중단하지 않는다(반쪽 화면이 더 혼란스럽다).

검토 기록: [compat-round3.md 부록 A](docs/compat-round3.md#부록-a--04-표적-코드-검토-표-0a) — 지점별 안전/수정/보류. 새 파서·레이아웃 코드를 쓰면 같은 기준으로.

## Tooling Notes

- 퍼저 ([compat-round3.md 0.3](docs/compat-round3.md#03-퍼징), `crates/mb-engine/tests/fuzz_layout.rs`): 기본 `cargo test` 에 시드 200(레이아웃)·500(CSS 토큰 — `@media` 프렐류드 수프·상한 넘는 중첩 포함)·300(이미지 바이트)·깊이 10,000 한 케이스·`@media` 극단 한 케이스(`media_extremes_stay_bounded`)가 들어 있다. 긴 실행은 `MB_FUZZ_CASES=100000 cargo test --release -p mb-engine --test fuzz_layout -- --ignored --nocapture --test-threads=1`(`MB_FUZZ_SEED` 시작 시드, `MB_FUZZ_FONTS=1` 실제 셰이핑, `MB_FUZZ_DUMP=1` 이미지 입력을 미리 찍기 — abort 추적용). 실패하면 시드와 **축소된** HTML/CSS 가 찍힌다 → 손으로 정리해 해당 모듈에 증상 이름의 단위 테스트로(시드를 테스트로 쓰지 말 것). 생성기·판정을 바꾸면 `fuzz_generator_covers_its_spec` 가 아직 깊이·값 풀을 다 내는지 본다.
- `crates/mb-dom/examples/css_diag.rs` — CSS 파서 진단 (`cargo run -p mb-dom --example css_diag -- <file.css>`).
- 실사이트 호환성 조사 ([docs/compat-survey.md](docs/compat-survey.md)): `MB_NET_CACHE=DIR` 이면 모든 fetch 가 디스크 캐시를 거친다 (첫 실행만 네트워크, `MB_NET_CACHE_OFFLINE=1` 은 재생 전용). `crates/mb-agent/examples/compat_probe.rs URL OUT_DIR` 가 관찰·CSS 버림(`css::parse_with_report`)·레이아웃 이상을 덤프. 캐시와 출력은 반드시 git-ignored `compat-cache/` 에 — 실사이트 HTML/CSS/이미지/스크린샷은 커밋 금지(저작권, 공개 저장소). 테스트는 손으로 쓴 작은 페이지로.
- 3차 코퍼스(~25곳, [compat-round3.md 0.2](docs/compat-round3.md#02-코퍼스-점검)): `scripts/compat-sites.tsv`(이름·URL·기대 컨트롤, 커밋됨) + `scripts/compat-corpus.sh record|run [라벨]|diff A B`. 녹화는 이미 끝났고 캐시 `compat-cache/net/` 는 **3차 동안 동결** — 단계 전후 측정은 `MB_NET_CACHE=$PWD/compat-cache/net scripts/compat-corpus.sh run <라벨>`(항상 오프라인, release) → `compat-cache/corpus/<라벨>/corpus-report.tsv`. `record` 는 녹화된 사이트를 다시 방문하지 않는다(보충은 `--supplement`, 규칙은 문서). 캐시가 켜져 있으면 GET 이 아닌 요청은 보내지 않는다(`net/cache.rs`). 버림 사유·ours/browser-too 분류는 `css/report.rs` `DropReason` + `css/properties.rs`(Chrome 이 아는 속성 표) — 동결된 표는 문서 "0b 분류표".
- `crates/mb-agent/tests/fixtures/compat3.html` — 호환성 3차 **추적용** 골든(손으로 쓴 sr-only·`@media`·`!important`·체크박스 해킹·캐러셀). 골든은 오늘의 **틀린** 렌더링을 일부러 기록한다 — 3차 단계가 들어갈 때마다 diff 를 파일 맨 위 주석의 "Chrome 이라면" 목록과 대조. `examples/agent-demo/compat3.html` 은 이 파일의 심볼릭 링크(eval `compat3-menu` 가 씀, 오늘은 실패가 정상).
- `scripts/package-macos.sh` — `mb-shell` release 빌드를 `dist/mini-browser.app` + `.dmg` 로 패키징.
