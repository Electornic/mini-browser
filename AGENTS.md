# AGENTS.md

이 문서는 `/Users/leejun/Desktop/Projects/mini-browser` 이하 전체에 적용된다. 작업하는 에이전트가 layout 을 빨리 파악하고 빌드/테스트/커밋 컨벤션에 맞춰 움직이도록 한다.

## Project Goal

학습용 토이 브라우저. Phase 0–3 까지는 모든 단계를 직접 구현했고, **Phase 4 에서 학습 가치를 다 뽑은 뒤 mature crate 들로 통합**, 4.9 에서 단일 crate 를 4-crate Cargo workspace 로 쪼갰다 (mb-dom / mb-engine / mb-runtime / mb-shell). 이후 작업의 무게중심은 "from-scratch 구현" 이 아니라 **글루 + 기능 보강** 이다.

phase 단위 history 는 [docs/legacy/](docs/legacy/) 의 옛 문서로 archived. 현재 코드와 1:1 대응되진 않으니 의도 파악용으로만 쓸 것.

## Workspace Layout

```
[workspace]                                 외부 dep
├── crates/mb-dom      (no I/O, pure data)  html5ever, cssparser, selectors,
│                                           image, url, precomputed-hash
│   - dom              NodeId arena + tombstone subtree
│   - dom_select       selectors::Element 구현 (matches/closest 진입)
│   - html             html5ever 브릿지 (parse_document / parse_fragment 두 모드)
│   - css              cssparser 기반 stylesheet AST + 토큰 파서
│   - style            cascade + computed values + StyledNode
│   - resource         LoadedImage 데이터 + decode_image (loaders 는 mb-runtime)
│   - url              Url + NetworkError + parse/resolve/Display
│
├── crates/mb-engine   (layout + paint)     cosmic-text, taffy, tiny-skia
│   - layout/          taffy 브릿지 + boundary block/inline/flex/grid/table
│   - render/          DisplayList paint commands + tiny-skia + swash
│   - chrome           주소창 / 뒤로앞 / 메뉴 페인트 명령
│   - display_list     DocumentView 빌더 + hit-testing (per-frame entry)
│   - font_system      shared FontSystem + SwashCache OnceLock 슬롯
│   - input            WindowInput 데이터 (winit driver 는 mb-shell)
│   - pub(crate) use mb_dom::{css, dom, resource, style, url} 로 짧은 경로 보존
│
├── crates/mb-runtime  (orchestrator + JS)  rquickjs, ureq, url, selectors
│   - state            BrowserState (per-frame loop driver, 거의 모든 모듈 의존)
│   - js/              rquickjs runtime + DOM/event/timer/fetch/xhr/storage 브릿지
│   - net              ureq fetch + Url/NetworkError re-export
│   - resource         parallel scoped fetch + LoadedImage re-export
│   - navigation       URL → Document 로더 (네트워크 → parse → install)
│   - pub use mb_dom::{...} + mb_engine::{...} 으로 외부에 단일 namespace 노출
│
└── crates/mb-shell    (binary [[bin]] mb-shell)  anyhow, softbuffer, winit
    - main.rs          진입점 — install_fonts → window::run(closure)
    - window.rs        winit 이벤트 루프 + softbuffer 프레임 출력
```

각 crate 는 위에서 아래로만 의존한다. 순환 dep 없음.

## Build / Test / Lint Commands

```sh
# 빠른 빌드 체크
cargo build --workspace 2>&1 | tail -10

# 테스트 (446 = mb-dom 109 + mb-engine 162 + mb-runtime 86 lib + 80 main + 9 html)
cargo test --workspace 2>&1 | grep "test result"

# 통합 테스트만
cargo test --test main_tests -p mb-runtime
cargo test --test html_tests -p mb-runtime

# 린트 + 포맷
cargo clippy --workspace --all-targets
cargo fmt --all
```

테스트 baseline 은 446. 작업 중 카운트가 변하면 의도된 건지 확인하고 commit message 에 명시.

Rust 1.94.1 핀 (`rust-toolchain.toml`).

## Hot Paths (자주 만지는 파일들)

| File | Lines | 역할 |
|---|---:|---|
| `crates/mb-runtime/src/state.rs` | ~1900 | BrowserState 본체 — 프레임 루프, 입력 디스패치, 캐럿/포커스, view cache 캐싱 |
| `crates/mb-engine/src/layout/mod.rs` | ~2800 | 박스 모델 + boundary 알고리즘 진입점 |
| `crates/mb-dom/src/css.rs` | ~2470 | CSS 토큰 파서 + value 파서 (gradient/transform/grid/flex 모두) |
| `crates/mb-engine/src/render/mod.rs` | ~1750 | rasterizer 본체 + DisplayCommand 정의 |
| `crates/mb-dom/src/style.rs` | ~1820 | cascade + 상속 + computed values |
| `crates/mb-runtime/src/js/mod.rs` | ~1020 | rquickjs runtime 부트스트랩 + 글로벌 등록 |

`src/main.rs` 같은 옛 root 경로는 더 이상 존재하지 않음 — 모든 코드가 `crates/` 아래.

## Conventions

### 코드 스타일
- 작은 함수 + 좋은 이름 우선, 주석은 *왜* 만 (무엇은 코드가 말함).
- 모든 변경은 cargo fmt 통과. clippy clean.
- 새 dep 추가는 의식적으로 — 4-crate 분할의 dep 그래프를 깨지 말 것 (mb-dom 에 IO 금지, mb-engine 에 네트워크 금지 등).
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
- Phase 작업은 `(Phase X.Y)` suffix.
- 매 sub-phase 끝에 cargo test green + clippy clean 확인 후 단일 commit.
- 본문은 *왜* 위주 — 변경의 의도, 트레이드오프, 잔존 이슈.
- Co-Authored-By 트레일러는 자동.

## Gotchas / 함정 모음

이전 phase 들에서 만났던 함정 — 다시 만나지 않도록 기록.

### 4.9 (workspace split)
- **orphan impl**: 타입을 다른 crate 로 옮기면 그 타입의 inherent impl 도 같이 옮겨야 함 (E0116). 4.9b 에서 `impl Color { WHITE/BLACK }` 가 별도 파일에 있어서 마이그레이션 중 깨졌음.
- **unused-import warning**: `pub(crate) use mb_dom::html` 처럼 `#[cfg(test)]` 만 쓰는 경우 일반 빌드에서 경고. `#[cfg(test)] pub(crate) use ...` 로 분리.
- **`cargo build` vs `cargo build --tests`**: 일반 빌드만 통과하고 테스트 빌드에서 unresolved import 가 나는 케이스 있음. CI 전에 항상 `--tests` 로 한 번 더 확인.

### 4.8 (rquickjs)
- closure 에서 `Ctx<'js>` + 다른 lifetime-bearing arg 를 같이 받으면 HRTB 통합 불가. 해결: **flat-hook 패턴** — 모든 Rust 콜백은 primitive (u32/String/bool/Vec) 만 받고 wrapper instance 는 JS bootstrap 이 `Object.defineProperty` 로 조립.
- `IntoJs` 가 plain tuple 미지원 — `convert::List<(...)>` wrapper 필요.

### 4.5 (tiny-skia)
- `Paint::default()` 의 `anti_alias = true` (docs 와 다름). 픽셀 핀 테스트가 partial coverage 로 깨지므로 `paint.anti_alias = false` 명시 필요.
- `gradient_transform` 은 `local → canvas` 방향.

### 4.6 (winit)
- winit `KeyEvent.text` 가 control char 도 흘려보냄 — `is_control()` 필터 안 걸면 input 박스에 invisible char 박힘.
- GUI 검증은 메인 스레드 blocking 이라 백그라운드 task 로 못 돌림. `cargo run --bin mb-shell` 로 직접 확인.

### 4.7 (ureq)
- ureq 는 헤더 이름을 wire 에 lowercase 로 보냄 (RFC 9110 §5.1 적합). 헤더 contains 검증 테스트는 `to_ascii_lowercase()` 후 비교.
- redirect 정책: 301/302/303 → GET 다운그레이드 + body drop, 307/308 → method+body 유지.
- body cap 50 MB (`limit().read_to_vec`).

### Font System
- `font_system::shared_font_system()` 가 `None` 이면 unit test 환경 — `font_size * 0.75` fallback 으로 토이 측정.
- `install_fonts()` 가 SwashCache 도 같이 재생성 — 글리프 캐시 키가 FontSystem 의 font id 에 의존하므로.

## Stale / Known Issues

- `examples/css_diag.rs` — `mini_browser::css` import 라 더 이상 컴파일 안 됨. 4-crate 분할에 맞게 위치 이동 + `mb_dom::css` 로 import 갱신 필요.
- `scripts/package-macos.sh` — `APP_NAME=mini-browser`, `target/release/${APP_NAME}` 가정. 현재 바이너리는 `mb-shell`. 패키징 시 갱신 필요.
- `packaging/macos/Info.plist` — 마찬가지로 `mini-browser` 가정 가능성.

## Knowledge Hub Pointer

작업 중 얻은 재사용 가능한 인사이트는 회사·프로젝트 기밀이 아닌 한 `~/Desktop/Projects/knowledge-hub-private/<domain>/<kebab-case>.md` 에 한 번 물어보고 저장한다.
