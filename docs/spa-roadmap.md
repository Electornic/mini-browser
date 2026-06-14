# SPA Rendering Roadmap

> 상태: 계획 (2026-06-14 작성, 코드 대조 2차 검토 반영) · 선행: SPA bootstrap **Step 1** 완료 (`cb60964`)

이 문서는 mini-browser가 **SPA(React/Vue/Svelte 류 client-rendered 앱)를 화면에 그리기까지**의 단계별 로드맵이다. 각 Tier는 *눈에 보이는 산출물*을 하나씩 만들도록 쪼갰다. 무게중심은 "from-scratch"가 아니라 기존 글루(rquickjs 브릿지 + mutate→재렌더 루프) 위에 빠진 표면을 메우는 것.

## TL;DR

- 이미 **mutate→같은 프레임 재렌더 루프**가 동작한다 = SPA 엔진의 심장은 있다.
- 못 그리는 이유는 그 위 4개 층의 공백: ① `type=module` 미실행 ② wrapper가 prototype을 상속 안 함 ③ 읽기 API(`querySelectorAll`/`getBoundingClientRect`/`element.style` 등) 부재 ④ 실 history 라우팅/Observer 부재.
- **권장 순서**: Tier 0(바닐라) → Tier 1 #5(prototype 상속, *키스톤*) → Tier 1 나머지 → Tier 2(모듈 로더) → Tier 3(폴리시).
- **최고 레버리지 단일 작업**: prototype 상속(#5) — 모든 프레임워크를 한 번에 해금, Step 1이 프로토타입을 깔아둬서 이미 절반.
- **가장 큰 신규 시스템**: ES module 로더(#8) — 진짜 빌드 산출물 실행의 관문 (단 rquickjs `loader` feature 플래그가 선행, Tier 2 참조).
- **숨은 함정**: 런타임 주입 CSS(#14) 미반영 — styled-components/emotion/MUI류가 "DOM은 맞는데 스타일 빠짐"으로 깨지는 주범. 위 4개 층과 별개인 CSS 파이프라인 갭.

---

## 0. 이미 동작하는 SPA 엔진

핵심은 [`BrowserState::display_list`](../crates/mb-runtime/src/state/mod.rs)가 **layout pass 직전에 JS job을 drain**한다는 것. JS 핸들러가 mutate한 DOM은 같은 프레임에 반영된다. 즉 SPA가 의존하는 다음이 **이미 됨**:

- `createElement` / `createTextNode` / `appendChild` / `removeChild` / `insertBefore` / `replaceChild` / `innerHTML` / `setAttribute` / `getAttribute` / `classList` / `textContent` / `value` / `cloneNode` / `matches` / `closest` / `querySelector`(단수) / `getElementById` / `getElementsByClassName`
- `addEventListener` + 버블 dispatch + `preventDefault`/`stopPropagation` + 키보드/클릭 이벤트
- `setTimeout` / `setInterval` / `requestAnimationFrame` / Promise microtask / `queueMicrotask`
- `fetch`(동기 백킹) / `XMLHttpRequest` / `localStorage` / `sessionStorage`
- DOM 변경 → `revision` 카운터 bump → view cache 무효화 → 재렌더 ([dom.rs](../crates/mb-dom/src/dom.rs) revision, [state/mod.rs](../crates/mb-runtime/src/state/mod.rs) cache key)

**drain 깊이 (과소평가 주의)**: [`drain_pending_jobs`](../crates/mb-runtime/src/js/mod.rs)는 프레임당 job 하나가 아니라 **마이크로태스크 큐를 빌 때까지 비우고 → due 타이머 실행 → 둘 다 멈출 때까지 최대 1024회 교대**한다. 즉 `fetch().then(r=>r.json()).then(render)` 같은 Promise 체인이 **한 프레임 안에 끝까지 resolve**된다 → async 데이터 로딩 SPA가 starvation 없이 오늘도 동작한다. 이건 갭이 아니라 셀링 포인트.

→ "JS가 DOM을 만들고 화면에 반영"은 오늘도 된다. 로드맵은 그 위 4개 층을 메우는 일이다.

---

## Tier 0 — 바닐라 JS SPA (proof of life) ✅ 완료 (575 green)

> **상태: 완주.** #1 qsa + #2 style (`cf49167`), #15 fragment (`b36b787`), #3 history + #4 win/doc 이벤트 + #16 readyState (`e31bee7`), 데모+capstone (`974d435`). 데모: [examples/spa-demo.html](../examples/spa-demo.html) — `python3 -m http.server` 로 로드.

**목표**: 직접 짠 client router + 동적 뷰 + fetch 데이터. 프레임워크/빌드 없이 classic `<script>`. 갭이 가장 작고, 막히는 지점을 실측으로 드러낸다.

| # | 갭 | 손댈 곳 | 난이도 |
|---|---|---|---|
| 1 | `querySelectorAll` (현재 단수 `querySelector`만) | `__mb_doc_query_selector_all` / `__mb_elem_query_selector_all` → `Vec<u32>`. selectors crate가 이미 매칭하므로 전부 수집만. `js/document.rs`, `js/element.rs` | 쉬움 |
| 2 | `element.style` 읽기/쓰기 | inline `style` 속성을 백킹하는 `CSSStyleDeclaration` shim. `el.style.display='none'` → style attr mutate → revision bump → 재렌더. `js/element.rs` | 쉬움~중 |
| 3 | 실 `history.pushState`/`replaceState` + `popstate`/`hashchange` | **현재 silent no-op stub** ([window.rs:195](../crates/mb-runtime/src/js/window.rs), length=1). 단 `location.pathname`/`search`/`hash` **읽기 게터는 이미 있음**(`install_url_getter`, [window.rs:106](../crates/mb-runtime/src/js/window.rs))이라 실제 잔여작업은 ① pushState가 **document 재fetch 없이** location 버퍼만 갱신 + JS history 엔트리 push ② back/forward가 `popstate` 발화 ③ same-document(pushState) vs cross-document(navigate) 분기 — `state/lifecycle.rs`가 현재 전부 재fetch | 중 (게터 재사용으로 체감 하향) |
| 4 | window/document 레벨 이벤트 (`DOMContentLoaded`/`load`/`popstate`/`hashchange`) **+ `document.readyState` 전이** | window/document 리스너 등록 + 프레임 루프/lifecycle에서 dispatch. **`document.readyState`('loading'→'interactive'→'complete')도 함께** — 라우터/프레임워크가 `if (document.readyState === 'loading')`로 부팅을 게이트한다(현재 document엔 미정의, XHR에만 readyState 있음). `js/event.rs`, `js/window.rs`, `state/lifecycle.rs` | 중 |

**산출물**: 라우팅되는 client-rendered 앱. 엔진이 SPA임을 증명.

**데모**: net 레이어가 http/https만 지원(`file://` ❌, `mb_dom::url::NetworkError::UnsupportedScheme`)하므로 `python3 -m http.server` 로 띄워 `http://localhost:8000/spa.html` 로드.

---

## Tier 1 — 프레임워크 (classic/UMD 번들, 모듈 불필요) ⭐ 키스톤

> **진행중.** #5 prototype 상속(키스톤) ✅ `6ec3c59` — wrapper = `Object.create(HTMLElement/Text/DocumentFragment.prototype)`, 접근자/메서드를 프로토타입에 1회 정의(`this._nodeId`), `instanceof` + polyfill `Element.prototype.*=fn` 패칭 작동(+5 테스트). #14 동적 `<style>` ✅ — live DOM `<style>` 재수집(아래 ⚠️ 참조). **다음 = #6 getBoundingClientRect.**

**목표**: Preact + htm UMD (초소형, JSX/빌드 불필요) `render(html` `<App/>` `, root)`. stretch: React UMD (`react.production.min.js` + `react-dom`).

| # | 갭 | 손댈 곳 | 난이도 |
|---|---|---|---|
| 5 | **Step 2 — prototype 상속** (최우선) | `register_dom_bootstrap`의 `__mb_make_element`를 **`Object.create(<tagProto>)`** 로 변경하고, 접근자/메서드를 `HTMLElement.prototype` 등(Step 1이 깐 [dom_globals.rs](../crates/mb-runtime/src/js/dom_globals.rs) 프로토타입)에 **한 번만** 정의해 `this._nodeId`를 읽게 한다. → 프레임워크의 prototype 가정 + polyfill의 `Element.prototype.* = fn` 패칭이 **wrapper에 보이게** 됨. 현재는 인스턴스마다 `defineProperty`라서 안 보임 ([element.rs](../crates/mb-runtime/src/js/element.rs) 헤더 주석 참조) | 중~높음 |
| 6 | `getBoundingClientRect` / `offsetWidth/Height` / `clientWidth/Height` | layout이 JS 뒤에 도므로 **직전 프레임** box를 제공 (1프레임 stale, 허용). `cached_view.layout_root`에서 `NodeId→Rect` 맵 구성 후 `__mb_elem_bounding_rect`. `state/mod.rs` | 중 |
| 7 | 이벤트 완성도 | `MouseEvent.clientX/Y`, `el.dispatchEvent(new Event(...))`, capture phase, `CustomEvent` detail. `js/event.rs` | 중 |

**산출물**: 실제 프레임워크가 화면에 렌더 + 갱신.

**왜 키스톤인가**: #5는 *모든* 프레임워크의 전제이며, Step 1(`cb60964`)이 프로토타입 사다리를 이미 깔아서 **절반 완료** 상태다. 이것만 끝내면 classic 번들로 배포되는 프레임워크 다수가 해금된다.

---

## Tier 2 — 진짜 빌드 산출물 (`<script type=module>`)

**목표**: `vite build` / CRA 출력. 빌드 후엔 상대경로 모듈 그래프(`./assets/index-abc.js`)라 npm/bare-specifier resolve가 불필요 → 상대 URL 리졸버 + fetch 로더면 그래프가 로드된다.

| # | 갭 | 손댈 곳 | 난이도 |
|---|---|---|---|
| 8 | ES module 로더 | rquickjs `Resolver`(importer 기준 상대경로 join) + `Loader`([net::load_script](../crates/mb-runtime/src/net.rs)로 fetch → `Module::declare`), `Runtime::set_loader(...)`. `type=module` 스크립트는 `Module::evaluate`로 실행(Promise 반환 → 기존 drain 루프가 처리). `is_classic_script_type` 게이트 해제 ([state/mod.rs](../crates/mb-runtime/src/state/mod.rs)). `js/mod.rs` + 신규 `js/modules.rs` | 높음 |
| 9 | import maps + 동적 `import()` (선택) | bare specifier / CDN ESM 대응. 로더 확장 | 중 |

**rquickjs 실현가능성**: rquickjs 0.11이 `Module::declare`/`Module::evaluate`(Promise) + `Runtime::set_loader(Resolver, Loader)`를 제공하고 QuickJS는 ES 모듈을 네이티브 지원 → API는 존재. **단, `loader` feature가 현재 꺼져 있다** — `mod loader`/`set_loader`가 `#[cfg(feature = "loader")]`로 게이트되는데 [Cargo.toml:9](../crates/mb-runtime/Cargo.toml)가 `features = ["macro"]`만 켠다. (Cargo.lock에 `relative-path`가 보이는 건 proc-macro인 `rquickjs-macro`가 *호스트 쪽*에서 켠 것 — resolver v2가 런타임 의존성과 분리 해석하므로 facade로 안 샌다. `features=["macro"]`로 `use rquickjs::loader::*` 컴파일 시 `E0432: could not find loader` 확인.) → **고치는 건 한 줄**: `features = ["macro", "loader"]`. 즉 Tier 2는 실현 가능하지만 *이 플래그 추가가 선행조건*이다.

**주의**: 빌드 산출물도 Tier 0~1의 API를 전부 쓴다. **모듈 로딩은 필요조건이지 충분조건이 아니다** — Tier 0/1을 건너뛰고 Tier 2만 하면 부팅은 되도 렌더가 안 된다.

---

## Tier 3 — Observer / CSSOM 폴리시

| # | 갭 | 메모 |
|---|---|---|
| 10 | 실 `MutationObserver` | **우아한 적합**: 기존 `revision` 카운터 / mutation을 MutationRecord로 흘리면 됨. 현재 no-op stub ([dom_globals.rs](../crates/mb-runtime/src/js/dom_globals.rs)) |
| 11 | `ResizeObserver` / `IntersectionObserver` | 프레임별 layout geometry로 구동. 현재 미정의 → `new` 시 throw |
| 12 | `getComputedStyle` | 프레임마다 도는 StyledNode computed 값을 `NodeId→computed`로 JS에 노출 |
| 13 | `dataset` / `insertAdjacentHTML` / variadic `append`·`prepend` / `requestIdleCallback` | 소소 |

---

## 권장 실행 순서

```
Tier 0 (바닐라, 갭 최소, 막히는 곳 실측)
  → Tier 1 #5  prototype 상속  ← 키스톤 (이미 절반)
  → Tier 1 #6,#7
  → Tier 2 #8  모듈 로더       ← 진짜 빌드 실행 관문
  → Tier 3 (필요 시)
```

첫 발 추천: **Tier 0** — 작은 바닐라 SPA 데모 HTML을 만들어 *현재 무엇이 깨지는지 실측* → 깨지는 순서대로 #1~#4를 메운다. 이미 있는 mutate→재렌더 루프가 SPA 엔진임을 눈으로 확인하면서, 정확한 공백을 데이터로 확보하는 게 목적.

---

## 재검토 / 구현가능성 검증 (2026-06-14)

로드맵 작성 후 핵심 가정을 rquickjs 문서 + 코드 + 웹 검색(1차) + 코드 대조(2차)로 재검증. **전체 결론: 구현 가능.** 단 (a) 갭 3건 추가 — #14 동적 style, #15 fragment, #16 readyState, (b) 정정 2건 — #7 node-level 구체화, Tier 2 loader feature 오류, (c) **Tier 2는 `loader` feature 플래그가 선행조건**(아래 ❌ 참조).

### ✅ 확정 (리스크 해소)

- **Tier 2 모듈 로더 — sync Runtime으로 가능**. rquickjs 0.11 **sync** `Runtime::set_loader(Resolver, Loader)`가 존재(AsyncRuntime 전용 아님)하고, `Loader::load(&mut self, ctx, name) -> Result<Module>`가 **동기 시그니처**다. 즉 현재 sync `Runtime`/`Context` 구조([js/mod.rs](../crates/mb-runtime/src/js/mod.rs))를 async로 갈아엎지 않고, 모듈 인스턴스화 중 loader 안에서 기존 **동기 blocking** `net::load_script`로 fetch하면 된다. QuickJS의 모듈 로더 콜백 자체가 동기라 모델이 정확히 맞는다. → **async rewrite 리스크는 해소**(단 `loader` feature가 꺼져 있는 별개 이슈는 아래 ❌ 참조).

### ❌ 정정 (1차 검토 오류 — loader feature)

- **이전 판본의 "loader feature가 컴파일됨" 단정은 거짓이었다.** `mod loader`/`set_loader`는 `#[cfg(feature = "loader")]` 게이트인데 [Cargo.toml](../crates/mb-runtime/Cargo.toml)는 `features = ["macro"]`만 켠다. Cargo.lock의 `relative-path`는 proc-macro `rquickjs-macro`의 호스트측 활성화일 뿐(resolver v2 분리 해석 → facade 미노출). 컴파일 프로브(`features=["macro"]` + `use rquickjs::loader::*`)가 `E0432: could not find loader`로 확정. **→ Tier 2 선행조건: [Cargo.toml:9](../crates/mb-runtime/Cargo.toml)를 `features = ["macro", "loader"]`로 (한 줄). 그 후 신뢰도 상향이 유효.**

### ⚠️ 추가된 갭 (누락분, Tier 1 우선순위)

- **#14 동적 `<style>` 미반영 — ✅ DONE.** 진단보다 갭이 더 컸다: 인라인 `<style>`은 **정적이든 동적이든 애초에 한 번도 수집된 적이 없었다** — `load_stylesheets`는 외부 `<link>`만 fetch하고(`stylesheets.join("\n")` → `parsed_stylesheet`), `build_document_view`는 그 외부 시트만 cascade에 넣었다. 인라인 `<style>` 엘리먼트 텍스트를 모으는 경로가 어디에도 없었음.
  - 해결(구현): `parsed_stylesheet: Stylesheet` → `parsed_stylesheets: Vec<Stylesheet>`로 바꿔 `[0]`=외부 링크 CSS(install 시 1회), `[1]`=live DOM `<style>` 텍스트 재수집(`collect_inline_style_css`). `build_or_reuse_view`가 cache miss마다 `refresh_inline_stylesheet(revision)` 호출 — **revision 메모**로 hover/focus-only 리빌드에선 재파싱 skip, DOM 변경(=`<style>` 주입/편집, revision bump) 시에만 재수집. `build_document_view`는 `&[Stylesheet]` 슬라이스를 받아 그대로 `style_tree_with_state`로 전달. styled-components/emotion/MUI의 `createElement('style')`+`textContent` 패턴이 다음 프레임에 적용됨(+2 테스트). **scope 외**: 동적 `<link rel=stylesheet>` fetch(async resource 배관 필요), CSSOM `sheet.insertRule`(Tier 3 CSSOM) — emotion "speedy" 모드는 insertRule이라 미적용, textContent 경로만 지원.

### ✏️ 정정 (Tier 1 #7 구체화)

- Preact/React differ는 **element-centric**가 아니라 **node-level DOM**에 의존한다: `parentNode` / `firstChild` / `nextSibling` / `childNodes` / `nodeType`, 텍스트노드 `.data`·`.nodeValue`, 직접 프로퍼티 대입(`el.value=`, `el.onclick=`, **`el.className=`** — React DOM은 class를 classList가 아니라 className 직접 대입으로 쓴다), `removeAttribute`, (SVG 시) `createElementNS`. 현재 브릿지는 `parentElement`/`children`(element-centric) 위주라 **node-level이 빠졌을 가능성이 높다**(element.rs가 long-line이라 grep 미검출 — 구현 전 직접 확인 필요. `nodeType` *상수*(ELEMENT_NODE=1/TEXT_NODE=3/DOCUMENT_FRAGMENT_NODE=11)는 Step 1이 [dom_globals.rs](../crates/mb-runtime/src/js/dom_globals.rs)에 깔아둠 — 누락된 건 인스턴스 `.nodeType` 게터). Tier 1 #7을 "이벤트 완성도" → **"node-level DOM 트리 순회 API + 텍스트노드 + 직접 프로퍼티 대입(`className`/`value`/`on*`)"** 으로 교체.

### ⚠️ 추가된 갭 (2차 코드 대조)

- **#15 `document.createDocumentFragment()` 팩토리 부재** — `DocumentFragment` *생성자*와 `DOCUMENT_FRAGMENT_NODE=11`은 Step 1이 [dom_globals.rs:157](../crates/mb-runtime/src/js/dom_globals.rs)에 깔았지만, 정작 **만드는 팩토리 메서드가 document.rs에 없다**. 배치 삽입(`frag.appendChild ×N` → 1회 insert)·일부 innerHTML/template 라이브러리가 쓴다. 프로토타입만 있고 생성 경로가 빠진 상태. 손댈 곳: `js/document.rs`(`__mb_doc_create_document_fragment` + `__mb_make_node` 경로). 난이도 쉬움. (Preact 자체는 거의 안 쓰므로 Tier 0~1 마이너.)
- **#16 `document.readyState` 부재** — `readyState`는 [xhr.rs](../crates/mb-runtime/src/js/xhr.rs)에만 있고 **document엔 없다**. 라우터/프레임워크가 `if (document.readyState === 'loading') defer else init()`로 부팅을 게이트하는 게 흔함. #4와 한 묶음으로 'loading'→'interactive'→'complete' 전이를 lifecycle에 얹는다. 손댈 곳: `js/document.rs` + `state/lifecycle.rs`. 난이도 쉬움~중.

### 🎯 타깃 권고

- **Tier 1 = Preact + htm(UMD) 우선**. React 18은 스케줄러가 `MessageChannel`에 의존(미구현 가능성 → `setTimeout` fallback은 있으나 동작 보장 약함)하고 합성이벤트 시스템이 무겁다. Preact는 스케줄러 없고 코어 DOM 요구가 작아 임베디드 엔진에서 검증된 선택 (웹 검색 confirm). React는 Tier 1 통과 후 stretch.

### 의존성 정리

```
Tier 0  : ✅ DONE — #1 qsa, #2 style, #3 history, #4 win/doc 이벤트+#16 readyState, #15 fragment
Tier 1  : ✅ #5 prototype(키스톤, 6ec3c59) → ✅ #14 동적 style → #6 rect → #7 node-level(+className/value/on*)   ← 다음=#6
Tier 2  : (선행: Cargo.toml `loader` feature 추가) → #8 모듈 로더(sync set_loader) → #9 import maps
Tier 3  : #10~13 observer/CSSOM (IntersectionObserver > ResizeObserver 우선순위)
```

**Sources**: [rquickjs Runtime::set_loader](https://docs.rs/rquickjs/latest/rquickjs/runtime/struct.Runtime.html), [rquickjs Loader trait](https://docs.rs/rquickjs/latest/rquickjs/loader/trait.Loader.html), [rquickjs Module](https://docs.rs/rquickjs/latest/rquickjs/module/struct.Module.html), [preactjs/preact](https://github.com/preactjs/preact), [A mostly vanilla JS way to use Preact](https://gomakethings.com/a-mostly-vanilla-js-way-to-use-preact/).
