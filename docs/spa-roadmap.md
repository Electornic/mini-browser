# SPA Rendering Roadmap

> 상태: **Tier 0·1 코드 완료 (593 green, 2026-06-14)** · **Tier 1.5 Preact 실증 ✅ (720 green, 2026-11-21)** · **호환성 2차 = 실사이트 조사 ✅ (803 green, 2026-11-26) → [compat-survey.md](compat-survey.md)** · 다음 = 조사의 [호환성 3차 순서](compat-survey.md#호환성-3차-제안-순서) (CSS 파서·`@media` 먼저, Tier 2 모듈은 8번째 — naver 본문)

이 문서는 mini-browser가 **SPA(React/Vue/Svelte 류 client-rendered 앱)를 화면에 그리기까지**의 단계별 로드맵이다. 각 Tier는 *눈에 보이는 산출물*을 하나씩 만들도록 쪼갰다. 무게중심은 "from-scratch"가 아니라 기존 글루(rquickjs 브릿지 + mutate→재렌더 루프) 위에 빠진 표면을 메우는 것.

## TL;DR

- **mutate→같은 프레임 재렌더 루프**가 원래 동작했다 = SPA 엔진의 심장은 있었다.
- 막혀 있던 4개 층: ① wrapper prototype 미상속 ② 읽기 API 부재 ③ 실 history 라우팅 부재 ④ `type=module` 미실행 (+ 숨은 함정: 런타임 주입 `<style>` 미반영).
- **①②③ + `<style>` 은 Tier 0·1에서 해결 (✅)**. 남은 건 ④ 모듈 로더(Tier 2)와 Observer/CSSOM(Tier 3).
- **Tier 1.5 (2026-11-21)**: 실제 Preact 10 + hooks UMD 앱([examples/agent-demo/preact.html](../examples/agent-demo/preact.html))이 렌더되고 에이전트 도구(입력·클릭·체크·해시 라우팅·뒤로)로 끝까지 조작된다. 막혔던 건 ⑤ 래퍼 정체성(`el === el`, expando) ⑥ 같은 문서 해시 이동 — 둘 다 해결. 같은 날 인라인 줄 묶기(레이아웃)와 IDL 속성 반사도 고침.

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

## Tier 1.5 — Preact 실증 ✅ (2026-11-21, 720 green)

Tier 1은 **API 갭을 메운 것**이지 실제 프레임워크를 띄워 본 건 아니었다. 산출물("실제 프레임워크가 화면에 렌더 + 갱신")을 눈으로 확인하는 단계.

**데모**: [examples/agent-demo/preact.html](../examples/agent-demo/preact.html) — index.html 과 같은 할 일 앱을 Preact 10.29.8 + hooks UMD(`vendor/`, MIT, 빌드·모듈 없음)로. `useState`·`useEffect`, 제어 입력(`value` + `onInput`), Enter 로 추가(`onKeyDown`), key 있는 리스트, 체크박스 `checked` prop, `hashchange` 해시 라우터(All / Active / Done). 컴포넌트는 `h()` 로 썼다 — htm 은 태그드 템플릿을 같은 `h()` 호출로 바꿀 뿐이라 엔진이 겪는 DOM 표면은 같다(로컬에 htm 이 없어 다운로드 없이 진행; 넣으려면 `vendor/htm.umd.js` 한 파일).

**검증 방식**: 스크립트로 `mb-mcp` 를 stdio JSON-RPC 로 몰고(navigate → type submit → click checkbox → click "Done" → back) 관찰을 읽었다. 정답지가 필요한 지점은 같은 페이지를 실제 브라우저에 띄워 비교했다.

**실측에서 깨진 것 → 고친 것** (깨진 순서대로):

| # | 증상 | 원인 | 수정 |
|---|---|---|---|
| 17 | 첫 렌더는 되는데 Add 클릭·입력 핸들러가 아무 일도 안 함 | **래퍼 정체성 없음**: `makeElement(id)` 가 접근마다 새 객체 → `el !== el`, 노드에 단 expando 소실. Preact 는 리스너 표를 노드(`dom.l`)에 두고 이벤트 프록시에서 `this.l` 로 읽는다 | `element.rs` 부트스트랩에 `NodeId → 래퍼` 캐시(`wrapperFor`). 문서 안에서 NodeId 는 재사용되지 않고(tombstone) 문서마다 런타임이 새로 생기므로 무효화 불필요 |
| 18 | 리스너 안 예외가 흔적 없이 사라짐 (위 버그를 찾기 어렵게 만든 원인) | `catch (err) { /* swallow */ }` (리스너·타이머·rAF) | 브라우저처럼 계속 진행하되 `console.error("Uncaught in click listener: …")` 로 보고 → 관찰의 `console:` 줄에 보임. 페이지가 `console` 을 바꿔도 못 숨기게 원래 싱크에 묶음 (`__mb_report_error`) |
| 19 | 해시 링크(`#/done`) 클릭 = **문서 통째 재로드** → 앱 상태 소실, `hashchange` 없음, 관찰 URL 에 해시 없음 | 링크 이동이 항상 cross-document fetch | 같은 문서 프래그먼트 이동: `navigate_to_href` 가 (게이트 통과 후) 현재 URL 과 `#` 앞이 같으면 재로드 대신 JS history 항목 push + `popstate` → `hashchange` (Chrome 순서) |
| 20 | 크롬(에이전트) 뒤로 = 해시·pushState 항목을 건너뛰고 이전 문서로 | 크롬 스택(`back_stack`)과 JS `history` 스택이 분리 | joint session history 축약판: `go_back`/`go_forward` 가 같은 문서 항목을 먼저 되돌리고(`popstate`, 해시만 다르면 `hashchange`), 없을 때만 문서 교체. `can_go_back` 도 둘 다 봄 |
| 21 | `pushState` 후에도 주소창·관찰 `url:` 이 옛 URL | `current_url` 이 JS `location` 을 안 따라감 | 프레임마다 JS 큐를 비운 뒤 `sync_url_from_js`. **보안**: 관찰의 trusted `url:` 에 그대로 실리므로 `pushState`/`replaceState` 는 **같은 출처만** (아니면 `SecurityError`, 스펙과 동일) + Rust 쪽 출처 재확인 2중. 스킴 상대 `//host/x` 를 경로로 풀던 버그도 같이 수정 |
| 22 | (참고) 초기 `<p>Loading…</p>` 가 안 지워짐 | **엔진 버그 아님** — Chrome 도 같음 (Preact `render` 는 컨테이너 기존 자식을 지우지 않음) | 데모에서 자리표시자 제거 |

함께 메운 것: `localName`·`namespaceURI`(Preact 의 기존 노드 재사용 판정), `hasAttribute`·`toggleAttribute`, IDL 속성 반사(`type`·`checked`·`disabled`·`htmlFor` 등 — [호환성 #2](#idl-속성-반사-2026-11-21)).

**고정**: capstone 통합 테스트 `preact_demo_adds_toggles_filters_and_goes_back` (mb-agent `tests/actions.rs`, 픽스처 서버가 `examples/agent-demo/` 를 그대로 서빙) + eval 시나리오 [`preact-todo`](../eval/scenarios/preact-todo.json) (스크립트로 만든 잘한 실행 ✓ / 필터 안 바꾼 실행 ✗ 를 판정이 가르는 것 확인).

**남은 갭 (실측에선 안 막혔지만 다음에 막힐 것)**:
- `location.href = …` / `location.assign` / `location.hash = …` setter 없음 — JS 발 이동. 추가 시 반드시 `policy::check` 를 지나게 (AGENTS.md 규칙). 해시 setter 는 위 #19 경로 재사용.
- 프래그먼트 이동 시 `id`/`name` 대상 요소로 스크롤 안 함.
- 다른 문서로 갔다 돌아오면 그 문서의 같은 문서 항목(해시·pushState)은 잃는다 (스냅숏 복원 = 새 런타임, 항목 1개).
- `a.href` 는 절대 URL 이 아니라 속성 원문을 돌려준다 (라우터가 `a.href === location.href` 로 비교하면 틀림).
- `el.checked = true` 로 radio 를 켤 때 같은 그룹의 다른 radio 를 끄지 않음 (클릭 경로는 끔).
- 이벤트 완성도: capture phase, `MouseEvent.clientX/Y`, `CustomEvent`/`dispatchEvent` — Preact 데모엔 불필요했음.
- stretch: React UMD (스케줄러 `MessageChannel` 의존), htm.

### IDL 속성 반사 (2026-11-21)

프레임워크는 `setAttribute` 대신 속성 대입을 쓴다(`el.type = 'checkbox'`, `el.checked = true`). 스타일 매칭·레이아웃·폼 제출·에이전트 관찰은 전부 **속성**을 읽으므로 속성 setter 가 속성으로 바로 쓰도록 반사했다: 문자열(`name`·`placeholder`·`title`·`alt`·`href`·`src`·`rel`·`target`·`lang`·`dir`·`action`·`autocomplete`·`pattern`·`accept`·`min`·`max`·`step`, `htmlFor`→`for`), 불리언(`disabled`·`hidden`·`required`·`multiple`·`autofocus`·`checked`·`selected`·`open`, `readOnly`→`readonly`, `noValidate`→`novalidate`, false 면 속성 제거), 열거형 `type`(읽을 때 소문자, 없으면 input=`text`·button=`submit`).
- **스펙과 다른 점**: `checked`/`selected` 는 스펙에선 속성이 *기본값*일 뿐이고 현재 상태는 별도 슬롯이다. 이 엔진은 클릭 토글도 속성에 쓰므로(현재 상태 = 속성) 속성 setter 도 속성으로 맞췄다. 나중에 `defaultChecked` 를 구분해야 하면 그때 분리.

### Tier 2 결정 (2026-11-21)

> **2026-11-26 갱신**: [실사이트 조사](compat-survey.md) 에서 naver.com 본문(뉴스·메뉴·바로가기)이 전부 `<script type="module">` 로 그려진다는 게 확인됐다 — 5곳 중 1곳이지만 조사 계기가 된 사이트. 다만 다른 사이트들을 무너뜨리는 건 CSS 파서·`@media` 쪽이라 3차 순서에선 8번째로 둔다.

**지금은 Tier 2(모듈 로더)를 하지 않는다.** classic/UMD 번들 프레임워크는 Tier 1.5 로 동작이 확인됐고, 에이전트 데모 대상 페이지(로컬 픽스처·데모)는 모듈을 안 쓴다. Tier 2 는 "실제 `vite build` 산출물을 에이전트로 테스트하고 싶다"는 수요가 생길 때 연다 — 그때 선행은 그대로 `features = ["macro", "loader"]` 한 줄. 그 전에 위 잔여 갭 중 `location` setter(게이트 경유)와 `a.href` 절대 URL 이 실제 SPA 에서 먼저 막힐 가능성이 높다.

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
Tier 1.5: ✅ Preact + hooks 실증 · #17 래퍼 정체성 · #18 예외 보고 · #19 해시 이동 · #20 joint history · #21 pushState URL 동기화(같은 출처만)
          잔여: location setter(게이트 경유) · a.href 절대 URL · 프래그먼트 스크롤 · React/htm stretch   ← 다음 후보
Tier 2  : (보류 — 결정 참고) rquickjs `loader` feature → #8 모듈 로더(sync set_loader) → #9 import maps
Tier 3  : #10~13 observer/CSSOM (IntersectionObserver > ResizeObserver)
```

---

## 설계 메모 (2026-06-14 검증 결과 요약)

- **Tier 2 모듈 로더는 sync Runtime으로 가능.** rquickjs의 sync `Runtime::set_loader(Resolver, Loader)`와 동기 `Loader::load`가 QuickJS 모듈 콜백 모델과 맞아서, 로더 안에서 기존 blocking `net::load_script`로 fetch하면 된다(async rewrite 불필요). 단 `mod loader`는 `#[cfg(feature = "loader")]` 게이트 → [Cargo.toml](../crates/mb-runtime/Cargo.toml)에 `"loader"` feature 추가가 선행. (Cargo.lock의 `relative-path`는 proc-macro 호스트측 활성화일 뿐 facade에 노출 안 됨 — 컴파일 프로브로 확인.) rquickjs 0.14 (2026-11-10 업그레이드) 에서도 sync `Runtime::set_loader<R: Resolver, L: Loader>` 시그니처 유지 확인.
- **#14 동적 `<style>`**: 원래 인라인 `<style>`은 정적이든 동적이든 수집된 적이 없었다. `parsed_stylesheets: Vec<Stylesheet>` = `[0]` 외부 `<link>` CSS(install 시 1회) + `[1]` live DOM `<style>` 재수집(`collect_inline_style_css`, revision 메모로 DOM 변경 시에만 재파싱). styled-components/emotion의 `textContent` 주입 경로 지원. **scope 외**: 동적 `<link rel=stylesheet>` fetch, CSSOM `insertRule`(emotion speedy 모드).
- **#7**: Preact/React differ는 element-centric이 아니라 node-level DOM(텍스트노드 포함 순회)과 직접 프로퍼티 대입(`el.className=`, `el.value=`, `el.onclick=`)에 의존한다.
- **타깃**: Preact + htm(UMD) 우선 — 스케줄러 없고 코어 DOM 요구가 작다. React는 stretch.

**Sources**: [rquickjs Runtime::set_loader](https://docs.rs/rquickjs/latest/rquickjs/runtime/struct.Runtime.html), [rquickjs Loader trait](https://docs.rs/rquickjs/latest/rquickjs/loader/trait.Loader.html), [preactjs/preact](https://github.com/preactjs/preact), [A mostly vanilla JS way to use Preact](https://gomakethings.com/a-mostly-vanilla-js-way-to-use-preact/).
