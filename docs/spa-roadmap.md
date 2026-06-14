# SPA Rendering Roadmap

> 상태: 계획 (2026-06-14 작성) · 선행: SPA bootstrap **Step 1** 완료 (`cb60964`)

이 문서는 mini-browser가 **SPA(React/Vue/Svelte 류 client-rendered 앱)를 화면에 그리기까지**의 단계별 로드맵이다. 각 Tier는 *눈에 보이는 산출물*을 하나씩 만들도록 쪼갰다. 무게중심은 "from-scratch"가 아니라 기존 글루(rquickjs 브릿지 + mutate→재렌더 루프) 위에 빠진 표면을 메우는 것.

## TL;DR

- 이미 **mutate→같은 프레임 재렌더 루프**가 동작한다 = SPA 엔진의 심장은 있다.
- 못 그리는 이유는 그 위 4개 층의 공백: ① `type=module` 미실행 ② wrapper가 prototype을 상속 안 함 ③ 읽기 API(`querySelectorAll`/`getBoundingClientRect`/`element.style` 등) 부재 ④ 실 history 라우팅/Observer 부재.
- **권장 순서**: Tier 0(바닐라) → Tier 1 #5(prototype 상속, *키스톤*) → Tier 1 나머지 → Tier 2(모듈 로더) → Tier 3(폴리시).
- **최고 레버리지 단일 작업**: prototype 상속(#5) — 모든 프레임워크를 한 번에 해금, Step 1이 프로토타입을 깔아둬서 이미 절반.
- **가장 큰 신규 시스템**: ES module 로더(#8) — 진짜 빌드 산출물 실행의 관문.

---

## 0. 이미 동작하는 SPA 엔진

핵심은 [`BrowserState::display_list`](../crates/mb-runtime/src/state/mod.rs)가 **layout pass 직전에 JS job을 drain**한다는 것. JS 핸들러가 mutate한 DOM은 같은 프레임에 반영된다. 즉 SPA가 의존하는 다음이 **이미 됨**:

- `createElement` / `createTextNode` / `appendChild` / `removeChild` / `insertBefore` / `replaceChild` / `innerHTML` / `setAttribute` / `getAttribute` / `classList` / `textContent` / `value` / `cloneNode` / `matches` / `closest` / `querySelector`(단수) / `getElementById` / `getElementsByClassName`
- `addEventListener` + 버블 dispatch + `preventDefault`/`stopPropagation` + 키보드/클릭 이벤트
- `setTimeout` / `setInterval` / `requestAnimationFrame` / Promise microtask / `queueMicrotask`
- `fetch`(동기 백킹) / `XMLHttpRequest` / `localStorage` / `sessionStorage`
- DOM 변경 → `revision` 카운터 bump → view cache 무효화 → 재렌더 ([dom.rs](../crates/mb-dom/src/dom.rs) revision, [state/mod.rs](../crates/mb-runtime/src/state/mod.rs) cache key)

→ "JS가 DOM을 만들고 화면에 반영"은 오늘도 된다. 로드맵은 그 위 4개 층을 메우는 일이다.

---

## Tier 0 — 바닐라 JS SPA (proof of life)

**목표**: 직접 짠 client router + 동적 뷰 + fetch 데이터. 프레임워크/빌드 없이 classic `<script>`. 갭이 가장 작고, 막히는 지점을 실측으로 드러낸다.

| # | 갭 | 손댈 곳 | 난이도 |
|---|---|---|---|
| 1 | `querySelectorAll` (현재 단수 `querySelector`만) | `__mb_doc_query_selector_all` / `__mb_elem_query_selector_all` → `Vec<u32>`. selectors crate가 이미 매칭하므로 전부 수집만. `js/document.rs`, `js/element.rs` | 쉬움 |
| 2 | `element.style` 읽기/쓰기 | inline `style` 속성을 백킹하는 `CSSStyleDeclaration` shim. `el.style.display='none'` → style attr mutate → revision bump → 재렌더. `js/element.rs` | 쉬움~중 |
| 3 | 실 `history.pushState`/`replaceState` + `popstate`/`hashchange` | **현재 stub** (`js/window.rs`, length=1·silent). pushState는 **document 재fetch 없이** `location`만 갱신 + JS history 엔트리 push, back/forward가 `popstate` 발화. same-document(pushState) vs cross-document(navigate)를 구분해야 함 — `state/lifecycle.rs`가 현재 전부 재fetch | 중 |
| 4 | window/document 레벨 이벤트 (`DOMContentLoaded`/`load`/`popstate`/`hashchange`) | window/document 리스너 등록 + 프레임 루프/lifecycle에서 dispatch. `js/event.rs`, `js/window.rs`, `state/lifecycle.rs` | 중 |

**산출물**: 라우팅되는 client-rendered 앱. 엔진이 SPA임을 증명.

**데모**: net 레이어가 http/https만 지원(`file://` ❌, `mb_dom::url::NetworkError::UnsupportedScheme`)하므로 `python3 -m http.server` 로 띄워 `http://localhost:8000/spa.html` 로드.

---

## Tier 1 — 프레임워크 (classic/UMD 번들, 모듈 불필요) ⭐ 키스톤

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

**rquickjs 실현가능성 (확인됨)**: rquickjs 0.11이 `Module::declare`/`Module::evaluate`(Promise) + `Runtime::set_loader(Resolver, Loader)` 제공. `loader` feature가 컴파일됨(`rquickjs-core`의 `relative-path` 의존성으로 확인). QuickJS는 ES 모듈을 네이티브 지원.

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
