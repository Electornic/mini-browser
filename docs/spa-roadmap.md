# SPA Rendering Roadmap

> 상태: **Tier 0·1 코드 완료 (593 green, 2026-06-14)** · 다음 = **Tier 1.5 Preact 실증** → Tier 2 모듈 로더

이 문서는 mini-browser가 **SPA(React/Vue/Svelte 류 client-rendered 앱)를 화면에 그리기까지**의 단계별 로드맵이다. 각 Tier는 *눈에 보이는 산출물*을 하나씩 만들도록 쪼갰다. 무게중심은 "from-scratch"가 아니라 기존 글루(rquickjs 브릿지 + mutate→재렌더 루프) 위에 빠진 표면을 메우는 것.

## TL;DR

- **mutate→같은 프레임 재렌더 루프**가 원래 동작했다 = SPA 엔진의 심장은 있었다.
- 막혀 있던 4개 층: ① wrapper prototype 미상속 ② 읽기 API 부재 ③ 실 history 라우팅 부재 ④ `type=module` 미실행 (+ 숨은 함정: 런타임 주입 `<style>` 미반영).
- **①②③ + `<style>` 은 Tier 0·1에서 해결 (✅)**. 남은 건 ④ 모듈 로더(Tier 2)와 Observer/CSSOM(Tier 3).
- **단, 실제 프레임워크(Preact)를 띄워 본 적은 아직 없다** → 다음은 Tier 1.5 실증.

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

> **✅ 완주 (593 green).** #5 prototype 상속(키스톤) `6ec3c59` — wrapper = `Object.create(HTMLElement/Text/DocumentFragment.prototype)`, 접근자/메서드를 프로토타입에 1회 정의(`this._nodeId`), `instanceof` + polyfill `Element.prototype.*=fn` 패칭 작동(+5). #14 동적 `<style>` `f18c839` — live DOM `<style>` 재수집(아래 ⚠️). #6 getBoundingClientRect/offset*·client* `a5d187c` — `JsRuntime`가 `NodeId→ElementRect` 맵 보유, `BrowserState`가 layout마다 push, content-viewport 상대·1프레임 stale(+3). #7 node-level DOM — `Node.prototype`에 parentNode/firstChild/lastChild/next·previousSibling/childNodes(text 포함, makeNode 래핑), 텍스트노드 `.data`/`.nodeValue`/`.nodeName`, `className`/`id`/`nodeName`/`removeAttribute`, `on*` 핸들러(리스너 레지스트리 경유), `createElementNS`(SVG 미렌더, no-throw)(+8). **다음 = Tier 2 (모듈 로더, `loader` feature 선행).**

**목표**: Preact + htm UMD (초소형, JSX/빌드 불필요) `render(html` `<App/>` `, root)`. stretch: React UMD (`react.production.min.js` + `react-dom`).

| # | 갭 | 손댈 곳 | 난이도 |
|---|---|---|---|
| 5 | **Step 2 — prototype 상속** (최우선) | `register_dom_bootstrap`의 `__mb_make_element`를 **`Object.create(<tagProto>)`** 로 변경하고, 접근자/메서드를 `HTMLElement.prototype` 등(Step 1이 깐 [dom_globals.rs](../crates/mb-runtime/src/js/dom_globals.rs) 프로토타입)에 **한 번만** 정의해 `this._nodeId`를 읽게 한다. → 프레임워크의 prototype 가정 + polyfill의 `Element.prototype.* = fn` 패칭이 **wrapper에 보이게** 됨. 현재는 인스턴스마다 `defineProperty`라서 안 보임 ([element.rs](../crates/mb-runtime/src/js/element.rs) 헤더 주석 참조) | 중~높음 |
| 6 | `getBoundingClientRect` / `offsetWidth/Height` / `clientWidth/Height` | layout이 JS 뒤에 도므로 **직전 프레임** box를 제공 (1프레임 stale, 허용). `cached_view.layout_root`에서 `NodeId→Rect` 맵 구성 후 `__mb_elem_bounding_rect`. `state/mod.rs` | 중 |
| 7 | node-level DOM + 직접 프로퍼티 대입 (✅) | `Node.prototype`에 parentNode/firstChild/lastChild/next·previousSibling/childNodes(text 포함), 텍스트노드 `.data`/`.nodeValue`, `className`/`id`/`removeAttribute`/`on*`, `createElementNS`. 남은 이벤트 완성도(`MouseEvent.clientX/Y`, capture phase, `CustomEvent`)는 Tier 1.5 실측에서 필요하면 메운다. `js/element.rs` | 중 |

**산출물**: 실제 프레임워크가 화면에 렌더 + 갱신.

**왜 키스톤인가**: #5는 *모든* 프레임워크의 전제이며, Step 1(`cb60964`)이 프로토타입 사다리를 이미 깔아서 **절반 완료** 상태다. 이것만 끝내면 classic 번들로 배포되는 프레임워크 다수가 해금된다.

---

## Tier 1.5 — Preact 실증 (다음 작업)

Tier 1은 **API 갭을 메운 것**이지 실제 프레임워크를 띄워 본 건 아니다. 산출물("실제 프레임워크가 화면에 렌더 + 갱신")을 눈으로 확인하는 단계.

- `examples/`에 Preact + htm UMD를 vendoring한 데모(카운터 + 리스트 + 라우팅 정도) → `python3 -m http.server`로 로드.
- 깨지는 지점이 곧 진짜 남은 갭 → 깨지는 순서대로 메우고, 데모를 capstone 테스트로 고정.
- 통과하면 stretch로 React UMD (스케줄러가 `MessageChannel` 의존 → 미구현 시 `setTimeout` fallback 확인).

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

## 권장 실행 순서 (현재)

```
Tier 0  : ✅ #1 qsa · #2 style · #3 history · #4 win/doc 이벤트 · #15 fragment · #16 readyState
Tier 1  : ✅ #5 prototype(6ec3c59) · #14 동적 style(f18c839) · #6 rect(a5d187c) · #7 node-level(5e2fddf)
Tier 1.5: Preact + htm 실증                                    ← 다음
Tier 2  : rquickjs `loader` feature → #8 모듈 로더(sync set_loader) → #9 import maps
Tier 3  : #10~13 observer/CSSOM (IntersectionObserver > ResizeObserver)
```

---

## 설계 메모 (2026-06-14 검증 결과 요약)

- **Tier 2 모듈 로더는 sync Runtime으로 가능.** rquickjs의 sync `Runtime::set_loader(Resolver, Loader)`와 동기 `Loader::load`가 QuickJS 모듈 콜백 모델과 맞아서, 로더 안에서 기존 blocking `net::load_script`로 fetch하면 된다(async rewrite 불필요). 단 `mod loader`는 `#[cfg(feature = "loader")]` 게이트 → [Cargo.toml](../crates/mb-runtime/Cargo.toml)에 `"loader"` feature 추가가 선행. (Cargo.lock의 `relative-path`는 proc-macro 호스트측 활성화일 뿐 facade에 노출 안 됨 — 컴파일 프로브로 확인.) rquickjs 0.14 (2026-11-10 업그레이드) 에서도 sync `Runtime::set_loader<R: Resolver, L: Loader>` 시그니처 유지 확인.
- **#14 동적 `<style>`**: 원래 인라인 `<style>`은 정적이든 동적이든 수집된 적이 없었다. `parsed_stylesheets: Vec<Stylesheet>` = `[0]` 외부 `<link>` CSS(install 시 1회) + `[1]` live DOM `<style>` 재수집(`collect_inline_style_css`, revision 메모로 DOM 변경 시에만 재파싱). styled-components/emotion의 `textContent` 주입 경로 지원. **scope 외**: 동적 `<link rel=stylesheet>` fetch, CSSOM `insertRule`(emotion speedy 모드).
- **#7**: Preact/React differ는 element-centric이 아니라 node-level DOM(텍스트노드 포함 순회)과 직접 프로퍼티 대입(`el.className=`, `el.value=`, `el.onclick=`)에 의존한다.
- **타깃**: Preact + htm(UMD) 우선 — 스케줄러 없고 코어 DOM 요구가 작다. React는 stretch.

**Sources**: [rquickjs Runtime::set_loader](https://docs.rs/rquickjs/latest/rquickjs/runtime/struct.Runtime.html), [rquickjs Loader trait](https://docs.rs/rquickjs/latest/rquickjs/loader/trait.Loader.html), [preactjs/preact](https://github.com/preactjs/preact), [A mostly vanilla JS way to use Preact](https://gomakethings.com/a-mostly-vanilla-js-way-to-use-preact/).
