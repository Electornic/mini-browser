# 호환성 4차 설계 — 에이전트 정합성 + 작은 JS 표면 + 레이아웃·CSS 잔여

> 상태: **설계 확정 (2026-11-28)** — 마지막 확인이던 1b 거절 범위의 숫자 임계값([1단계 위험](#위험-1단계))도 사용자가 문서 값 그대로 승인했다. D1–D8 은 사용자가 2026-11-28 에 권고안대로 정했다. 같은 날 세 갈래 리뷰(코드 대조·웹 표준/Chrome·실행/측정)를 반영했고, 그 리뷰가 낳은 **새 결정 N1–N6 도 같은 날 사용자가 정했다**(N1 나 · N2 수정안 · N3 가 · N4 권고 · N5 4단계 P 뒤 · N6 가 — [리뷰 뒤 새 결정](#리뷰-뒤-새-결정-사용자--2026-11-28-결정)). 이어 2차 리뷰(사실 확인·일관성)를 반영했다. 4차 진행 중에 올 결정 지점은 N5(4단계 P 측정 뒤)와 N2 의 ② 목표 창(`r4-base` 직후)이다. 출발점은 `main` `cef51c5`(테스트 1173, 호환성 3차 완료 — [compat-round3.md](compat-round3.md)).
>
> 이 문서의 수치는 전부 3차 동결 코퍼스를 오프라인으로 재생해 얻었다: 3차 마지막 라벨 `final` 의 probe 출력, 그리고 이 문서를 쓰며 같은 커밋으로 다시 돌린 `r4-check`(시간 열을 빼면 모든 열과 요소 목록이 `final` 과 같다 — [부록 D](#부록-d--측정-재현과-명령)). 사이트는 다시 방문하지 않았다. 진행은 [진행 표](#진행-표)에 적는다.

이 문서만 읽고 어느 단계든 이어받을 수 있게 쓴다. 3차와 같은 틀이다: 단계마다 **목표 → 세부 항목 → 설계 결정 → 테스트 → 측정·완료 기준 → 위험 → 노력**. 용어(슬리버, 버린 선언, ours/browser-too, IACVT, `ok*`, 라벨)와 측정 프로토콜은 3차의 [공통 규칙](compat-round3.md#범위와-순서)·[0b 분류표](compat-round3.md#0b-분류표-동결--바꾸려면-이-표를-먼저-고친다)를 그대로 잇는다. 여기에는 4차에서 **달라지는 것**만 적는다.

**3차와 다른 점**: 3차의 주 지표는 "CSS 가 얼마나 버려지나"(선언 −90.4%)였다. 남은 버림은 대부분 페인트 전용 값이라 에이전트가 보는 것을 거의 바꾸지 않는다. 그래서 4차의 주 지표는 **에이전트가 보는 것과 누를 수 있는 것이 Chrome 과 같은가**다. CSS 버림 수는 보조 지표로 둔다.

**리뷰가 바로잡은 사실 셋**(초안이 틀렸던 것 — 아래 절들은 이미 고쳤다):
1. 가로로 뷰포트 밖에 있는 요소의 클릭은 "조용한 헛클릭"이 **아니라 오늘 성공한다**. 히트 테스트(`view.rs` 533행 `compute_hovered_hit`)와 링크 판정(`state/events.rs` 265행 `hovered_link`)은 창 범위로 자르지 않고 "크롬 아래인가"만 본다. 창에 맞춰 당기는 것은 `click_point`(`mb-agent/src/browser.rs` 1088행)의 **y 뿐**이고 x 는 상자 중심 그대로다. 그래서 문제는 "사람·스크린샷이 못 보는 것을 에이전트가 누른다"이고, 1c 를 WebDriver 의 in-view centre point 로 다시 설계했다.
2. "첫 에러가 없는 API" 인 사이트는 15곳이 **아니라 10곳**이다. 초안은 콘솔 줄을 정렬한 뒤 세서 순서가 틀렸다. 나머지 6곳의 첫 에러는 "page-bug" 모양(`… of undefined`)이다. `document.cookie` 는 그 모양으로만 나타나서 P4 규칙으로는 절대 `missing-api` 로 잡히지 않는다.
3. 링크 이동은 오늘 히트 테스트가 아니라 **`LinkTarget` 목록의 기하 판정**(`state/events.rs` 248행 `clicked_link`)으로 정해진다. 그래서 덮개는 링크 이동을 막지 못하고 JS 클릭 대상만 가린다. 2차 리뷰의 결론(N3 의 귀결): **링크도 Chrome 처럼 히트 테스트를 따른다** — 히트된 노드의 조상 `href` 로 이동을 정한다(1b-링크). 숨은·`pointer-events:none`·`inert` 링크와 덮개 아래 링크가 같은 규칙 하나로 처리된다. 그리고 히트 테스트가 z-index 를 모르던 것도 1b 에서 고친다(1b-z).

---

## 리뷰 뒤 새 결정 (사용자 — 2026-11-28 결정)

리뷰가 드러낸 것 중 사용자가 정한 것. 각 결정은 선택지 → **권고** → 이유 → **결정** 순서다(각 절 끝 "결정" 줄). 단계 절·목표·진행 표는 이 결정에 맞춰 적었다.

### N1. crates.io 를 어떻게 넣을까 (D1 보충 ①의 범위)

문서만 다시 받으면 앱 껍데기만 생긴다. 그 껍데기가 내는 API·청크 요청은 캐시에 없어 미스가 되고, 기대 컨트롤(`textbox:search`·`link:browse all crates`)은 계속 누락일 가능성이 크다. 지금은 측정 대상이 20곳인지 21곳인지도 정해져 있지 않다.

| 선택지 | 내용 |
|---|---|
| 가 | **사이트 전체 보충**: K5 뒤 crates.io 를 3차 녹화와 같은 방식으로 한 번 녹화한다(문서 + 그 실행이 낸 GET 전부, 2초 간격). 21곳 측정 |
| **나 (권고)** | 문서만 보충해 견고성(nav-fail 0)만 확인하고, **에이전트 ①(기대 컨트롤)의 분모에서 뺀다**. 측정 20곳 유지. crates.io 는 "nav-fail → ok\*" 로만 보고 |
| 다 | 보충하지 않고 4차 내내 nav-fail 로 둔다(K5 엔진 수정은 테스트로만 확인) |

권고 이유: crates.io 는 SPA(앱이 전부 JS)라 2단계 양파의 대상이 하나 더 늘 뿐이고, 3차와의 비교 가능성도 없다. K5 의 가치(문서 요청이 HTML 을 받음)는 문서만 받아도 확인된다.

**결정(2026-11-28, 사용자)**: **나** — crates.io 는 K5 뒤 **문서만** 보충해 견고성(nav-fail 0)만 확인한다. 기대 컨트롤(①)의 분모에서 빼고, 측정은 20곳 그대로다. crates.io 결과는 `ok*`(앱 껍데기, 미스 있음)로만 보고한다.

### N2. D4 목표 고쳐 쓰기 (D4 를 다시 정함)

사용자는 D4 권고안을 승인했지만, 리뷰가 그 표의 근거 셋을 무너뜨렸다:
- ④ 의 "15곳"은 틀린 수다(실제 10곳).
- ② 는 아직 재지 않은 지표에 비율을 걸었다(3차가 −95% 를 나중에 조정한 함정과 같다).
- ③ 은 스크립트가 더 멀리 가면 오히려 **늘 수 있고**, bbc 처럼 같은 줄이 13번 반복된 것도 센다.

수정안(**권고**):

| 지표 | 정의(수정) | 지금 값(`final`) | 목표 |
|---|---|---:|---|
| 견고성 | panic/timeout/nav-fail(blocked·robots 제외, crates.io 포함 — N1: 문서 보충으로 nav-fail 이 아니게) · `[mb-budget]` | 0/0/1 · 0 | 0/0/0 · 0 |
| ① 기대 컨트롤 누락 사이트 | 분모 20(N1-나: crates.io 제외) | 7(D6 철자를 고치면 6) | **≤3** |
| ② `obs_unreachable` | 요소마다 범주 하나인 **합집합**(0단계 P2), ours 몫만. **단위 = 관찰 요소당 비율**(절대 수도 같이 적음). **기준 `r4-base` → 판정 `r4-final`**, 단계별 값은 참고 | 미측정 → `r4-base` **0.1800 (796)** | 구성 요소마다 **`r4-base` 에서 한 번만 정하는 창**(아래 "목표 창"), 구성 요소마다 맡는 단계가 있다. **결정(2026-11-28)**: 구성 요소는 절대 수 — `hidden` 0 · `intercepted` ours ≤135 · `offscreen_x` ours ≤28 · `clipped` 목표 없음 |
| ③ `Uncaught` 줄 | 메시지 + 위치로 **중복 제거**한 줄 수. 따로 `js_scripts_completed/total`(끝까지 돈 클래식 스크립트 비율)도 본다 | 83(중복 포함 96) | 줄 수 판정 대신 **완주 비율 상승 + 아래 ④** |
| ④ API 판정 | 첫 에러가 `missing-api` 인 사이트(10) + 어떤 줄이든 `missing-api` 가 있는 사이트(14). 기준선은 **사이트별 수동 판정표**(부록 B, 동결), 이후 라벨의 새 줄은 아래 "④ 판정 규칙". ①과 같이 판정한다 | 10 / 14 | **첫 에러 ≤3 · 어떤 줄이든 ≤6**(절대값) |
| 보조: 합산 버린 선언 ours | `decl_drop_total_ours` | 1,356 | **−50%**. 기준 라벨은 `r4-base-sup`(D1 보충 ② 뒤 옛 엔진), 판정 라벨은 `r4-final`(5단계 끝) |
| 참고: 버린 규칙 ours | — | 1,506 | **판정하지 않음**(거의 다 `:lang()` 이라 에이전트 의미가 없음 — 참고로만 적는다) |

- **목표 창(②)**: `r4-base` 를 잰 직후, **엔진 커밋이 하나라도 들어가기 전에** ②의 구성 요소마다 목표를 한 번 정한다(사용자 확인). 그 뒤로는 조정하지 않고 "못 맞춤"으로 적는다. 권고 출발점과 맡는 단계(2차 리뷰 반영):

  | 구성 요소 | 출발점 | 맡는 단계 |
  |---|---|---|
  | `hidden`(`visibility`·`inert`) | −100%(정의상) | 1a |
  | `intercepted`(ours — `pointer_none_overlay`·`z_order`·`other`) | −60% | 1b(1b-z 가 z-order 를 고치므로 `z_order` 몫도 목표 안) |
  | `offscreen_x`(ours) | −80% | 3(1c 는 거절로 알릴 뿐 수를 줄이지 않음) |
  | `clipped` | **목표 없음 — 참고만** | 없음. `hidden` 이 우선이라 숨은 드롭다운은 이미 그쪽에 들고, 남는 것은 대부분 Chrome 에서도 못 누르는 것(캐러셀 다른 슬라이드·sr-only) |

  - **목표 창 결정(2026-11-28, 사용자) — 이후 조정 없음, 못 맞추면 "못 맞춤"**: 구성 요소 목표는 **절대 수**(총합은 비율 + 절대 수로 적음). `hidden` **0** · `intercepted`(ours) **≤135**(`r4-base` 270 의 −50%) · `offscreen_x`(ours) **≤28**(94 의 −70%, kowiki 포함) · `clipped` **목표 없음(참고)**. 판정은 `r4-final`. 근거는 [0단계 ⑥ "② 목표 창"](#-목표-창-r4-base-2026-11-28--결정).
- **④ 판정 규칙**(2차 리뷰 반영 — 숫자 맞추기 방지):
  - 기준선(`r4-base`) 뒤 라벨의 **새 줄은 P4 자동 규칙**으로 판정한다. 수동 판정은 양파 기록의 "판정 근거" 열에 이유를 적은 경우에만 쓴다.
  - 우리 새 심이 돌려준 값 위에서 던진 page-bug 모양 에러(예: 새 `matchMedia` 가 준 객체의 빠진 메서드)는 **missing-api** 로 센다.
  - API 하나에는 의미 있는 증상 테스트가 있어야 한다. 빈 스텁으로 줄인 숫자는 무효다.
  - `scripts/compat-js-apis.tsv` 가 바뀌면 기준선의 분류를 다시 계산하되, **목표는 절대값(≤3·≤6) 그대로** 둔다.
- **crates.io 와 합계**(N1-나): 보충 ① 뒤 crates.io 는 견고성 지표에만 든다. ①의 분모, ②·③·④·보조 지표의 합계에서는 **뺀다**(20곳 그대로).

**결정(2026-11-28, 사용자)**: 위 수정안대로 — D4 를 대체한다. ②의 구성 요소별 목표는 `r4-base` 직후 목표 창에서 정한다(권고 출발점에서 시작, 엔진 커밋 전 사용자 확인). **2차 리뷰 반영**(결정의 범위 안에서 정의를 다듬음): ②의 단위·기준/판정 라벨·구성 요소별 맡는 단계, `clipped` 목표 없음, `intercepted` 출발점 −60%(z-order 가 1b 로 들어옴), ④ 판정 규칙, crates.io 합계 제외.

### N3. 가로 뷰포트 밖 클릭과 가로챈 클릭의 계약 (1b·1c)

에이전트 클릭의 결과를 바꾸는 계약 변경이다.
- **1c**: 오늘은 상자 중심이 창 밖이어도 클릭이 성공한다(사람은 못 본다). 리뷰 설계는 WebDriver 의 in-view centre point다. 클릭 점의 x 를 y 처럼 (요소 ∩ 뷰포트) 안으로 당기고, **교집합이 비면 거절**한다. 상자가 통째로 오른쪽에 있으면 오늘은 성공하던 클릭이 거절로 바뀐다. 엔진은 가로 스크롤이 없어서, Chrome 이 스크롤해서 누를 것을 우리는 누르지 못한다.
- **1b**: 클릭 점의 히트가 대상·자손·연결된 `<label>` 이 아니면(덮개가 가로챔) WebDriver "element click intercepted" 와 Playwright 처럼 `NotInteractable` 로 거절하고 **가로챈 요소의 역할·이름**을 알린다. 오늘은 가로챈 요소가 조용히 눌린다(JS 클릭 대상일 때).

| 선택지 | 1c | 1b |
|---|---|---|
| **가 (권고)** | in-view centre + 빈 교집합이면 거절. 힌트는 셋으로 가른다: `overflow:hidden` 조상(캐러셀 — "다음 버튼을 쓰라"), `overflow-x:auto/scroll` 조상(엔진 한계 — 컨테이너를 가로로 스크롤 못 함), 둘 다 아님(페이지가 뷰포트보다 넓음 — 엔진 한계) | 가로채면 거절 + 가로챈 요소 표시. 체크박스 해킹처럼 `<label>` 이 받는 경우는 힌트가 라벨을 가리킨다 |
| 나 | in-view centre 로 당기기만 하고, 빈 교집합도 오늘처럼 상자 중심으로 누른다(거절 없음) | 오늘처럼 아래 요소가 받는다(조용함) |

권고 이유: 에이전트의 클릭은 사람이 지켜보는 화면(관전 창·스크린샷)과 맞아야 한다(concept.md "사람은 에이전트가 보는 것과 하는 것을 그대로"). 거절은 이유와 다음 행동을 알려 주지만, 조용한 성공·오클릭은 아무것도 알려 주지 않는다.

**결정(2026-11-28, 사용자)**: **가** — 1c 는 in-view centre 로 당기고 가로 교집합이 비면 거절(힌트 셋). 1b 는 가로챈 클릭을 거절하고 가로챈 요소의 역할·이름을 알린다.

**결정의 귀결(2차 리뷰 — 새 사용자 결정 아님, "Chrome 같은 계약"을 끝까지 적용한 것)**:
- **링크도 히트 테스트를 따른다**: 링크 이동은 기하 `LinkTarget` 일치가 아니라 **히트된 노드의 조상 `href`** 로 정한다. 덮개가 가로채면 사람·에이전트 모두 링크 이동이 막힌다(Chrome 과 같음). 1b 는 가로챈 링크 클릭도 다른 클릭처럼 거절하고, P2 도 링크를 히트 규칙으로 센다.
- **히트 테스트의 z-order 를 1b 에서 고친다**: 페인터는 이미 positioned z-층을 z-index 로 정렬한다(`render/display_list.rs` 126–190행 `paint_stacking_context`, 511행 `z_index_of`). 그 순서를 모르는 것은 히트 테스트(`view.rs` 555행 `walk_for_hover`)뿐이다. 히트 테스트가 페인터와 같은 순서로 걷게 한다(S, 0.5~1일). 그래서 `z_order` 몫은 ②의 목표 **안**에 든다.
- 1b 의 거절이 "가로챈 요소"를 틀리게 고를 위험은 남는다. 거절 범위를 좁히는 숫자 임계값은 [1단계 위험](#위험-1단계)에 적었고, **사용자 확인 대상**이다.

### N4. D5 의 경계 사례 셋 (관찰 계약)

D5(`visibility:hidden` 은 관찰에서 뺌)와 같은 줄에서 정할 것:

| 사례 | Chrome a11y | **권고** |
|---|---|---|
| `inert` 속성(조상 포함) | 트리에 남지만 상호작용 불가·포커스 불가 | **관찰에서 뺀다**(D5 와 같이 — 에이전트가 할 수 있는 일이 없음). 클릭은 `NotInteractable "it is inert"` |
| `opacity: 0` | 트리에 남음, 클릭도 받음 | **관찰에 남긴다**(Chrome 과 같음 — 투명 버튼은 실제로 눌린다) |
| `aria-hidden="true"` | 트리에서 빠짐(자손 포함) | **4차는 지금처럼 무시**(관찰에 남김). 사이트가 열린 모달 뒤 본문 전체에 다는 경우가 많아 빼면 에이전트가 배경 링크를 잃는데, 그 링크는 실제로 눌린다. 5차에 관찰 플래그(`(aria-hidden)`)를 검토 |

**결정(2026-11-28, 사용자)**: 권고대로 — `inert` 는 관찰에서 빼고 행동을 거절, `opacity:0` 은 관찰에 남김, `aria-hidden` 은 4차에서 무시(5차 재검토).

### N5. `:defined` 와 모듈 문서 (4단계 P 에서 다시 결정할 지점)

4단계가 `:defined` 를 살리면 `:not(:defined){visibility:hidden}` 류 규칙이 살아나고, 1a 덕분에 실제로 숨긴다. 모듈을 지원하지 않는 한(D3), **모듈이 정의하는 커스텀 요소**(github·mdn 은 모듈 문서)는 영원히 정의되지 않아 계속 숨는다. 오늘은 그 규칙이 버려져 보인다. 그래서 4단계 P 가 그런 규칙 수와 맞는 요소 수(사이트별)를 잰 뒤 사용자가 고른다:
- 가: `:defined` 를 명세대로(정의 안 된 커스텀 요소는 거짓 — 숨음).
- 나: 4차 동안 `:defined` 를 **항상 참**으로(모듈이 생기면 바로잡음).

지금 권고는 측정 전이라 보류한다.

**결정(2026-11-28, 사용자)**: 쓴 대로 **4단계 P 측정 뒤로 미룬다** — 4단계 P 가 규칙·요소 수를 잰 뒤 가/나를 고른다(그 전에는 `:defined` 를 구현하지 않음).

### N6. 2단계 보충 ② 의 제3자 호스트 규칙 (녹화 윤리)

D1 보충 ②("`offline-miss` 로 잡힌 GET 하위 리소스만")는 미스 목록을 그대로 다시 받는다. 그런데 미스에는 광고·분석 호스트의 GET 비콘, 페이지 API(fetch/XHR)도 섞인다. 지금 보충 도구(`--supplement`)는 사이트 실행을 온라인으로 통째로 다시 돌려 **모든 GET 을 보낸다**(아래 0단계 S).

| 선택지 | 내용 |
|---|---|
| **가 (권고)** | 미스 목록 파일을 걸러서 받는다: (i) 종류가 css·js·font·img 면 호스트와 무관하게 받는다(3차 녹화가 이미 받은 종류). (ii) 그 밖(fetch/XHR·문서·알 수 없음)은 **문서와 같은 호스트일 때만** 받는다. (iii) 경로·쿼리에 비콘 흔적(`collect`·`beacon`·`pixel`·`track`·`log`)이 있으면 종류와 무관하게 받지 않는다. 걸러진 수는 `supp_skipped` 로 남긴다 |
| 나 | 미스 목록 전부(같은 호스트·제3자 구분 없음) |
| 다 | 같은 호스트만 |

권고 이유: 3차 녹화 윤리의 목적은 "제3자에게 가짜 노출을 최소화"였다. 미스 목록을 다시 받는 것은 사이트의 앱이 다시 도는 것보다 노출이 훨씬 작고, (i)은 3차 녹화에서 이미 같은 종류를 받았다.

**결정(2026-11-28, 사용자)**: **가** — css·js·font·img 는 호스트 무관, 그 밖은 문서와 같은 호스트만, 비콘 흔적 URL 은 제외, 걸러진 수는 `supp_skipped`.

---

## 결정 (사용자 — 2026-11-28)

각 결정은 선택지 → **권고** → 이유 → **결정** 순서다. 사용자가 2026-11-28 에 D1–D8 을 모두 권고안대로 정했다(각 절 끝 "결정" 줄). 선택지와 이유는 나중에 다시 볼 수 있게 남겨 둔다. 리뷰 뒤 다듬은 내용은 "리뷰 반영" 줄로 붙였다. 결정 자체를 다시 정해야 하는 것은 위 N 절로 옮겼다.

### D1. 코퍼스 캐시 — 3차 동결 캐시를 그대로 쓸까, 다시 녹화할까

지금 캐시(`compat-cache/net/`, 2026-11-26 녹화)의 알려진 결함은 둘이다:
- **kowiki 스킨 시트가 없다.** 녹화 때 `load.php?…skins.vector…` 시트 요청이 **HTTP 429**(요청 과다)를 받았다. enwiki 와 같은 Wikimedia 서버를 연달아 녹화해서다. 429 응답은 그대로 캐시됐다. 엔진은 200 이 아닌 CSS 를 버리므로(`net::load_css`) 동작은 정상이다. 3차 3a·5r 의 "kowiki 스킨 CSS 미적용"·"`--color-base` 선언 없음"은 **엔진 버그가 아니라 녹화의 결함**이다.
- **crates.io 문서가 404·빈 본문이다**(3차 K5). 문서 요청이 HTML 을 받는다고 밝히는 `Accept` 를 보내지 않아서로 추정한다. 캐시 키에 헤더가 없어서, 엔진을 고쳐도 그 항목을 지우고 다시 받아야 결과가 바뀐다.

이에 더해 **2단계(JS)가 새 요청을 낳는다.** 스크립트가 일찍 죽어 요청하지 않던 청크·API·이미지가, 스크립트가 더 멀리 가면 요청된다. 녹화에 없는 요청은 `offline-miss`(`ok*`)가 된다.

| 선택지 | 내용 | 득 | 실 |
|---|---|---|---|
| A | 완전 동결. 재방문 없음 | 3차와 바로 비교, 예의 비용 0 | kowiki·crates.io 를 4차 내내 잘못 잼. 2단계 효과가 `ok*` 에 가려짐 |
| **B (권고)** | 동결 유지 + **표적 보충 두 번**: ① 0단계에 kowiki 스킨 시트와 crates.io 문서(N1-나). ② 2단계 끝에 미스 목록(걸러 받기는 N6) | 결함 둘을 고치고 나머지의 비교 가능성은 지킨다 | 두 곳을 다시 방문. ②는 11-26 문서와 다른 날의 하위 리소스가 섞인다(해시 청크가 사라졌으면 404 로 기록하고 그대로 둔다) |
| C | 전체 재녹화(25곳, 새 날짜) | 결함 없는 출발 | 3차와 비교가 끊김. 25곳 재방문 |

권고 이유: 4차 목표는 새 기준선 대비 비율이라 C 가 꼭 필요하지 않고, 결함은 두 곳에만 있다. 방문은 3차 녹화 윤리를 그대로 따른다: robots.txt 1회 **새로 받아** 확인, `User-agent: *` 그룹, 차단이면 `blocked` 만 남기고 우회 안 함, GET 만, 식별 토큰 없는 기본 UA, 순차, 요청 사이 2초.

**결정(2026-11-28, 사용자)**: **B** — 동결 캐시 유지 + 표적 보충 두 번(0단계 ①: kowiki 스킨 시트·K5 뒤 crates.io 문서, 2단계 끝 ②: `offline-miss` 로 잡힌 GET 하위 리소스만). 사이트 재방문은 위의 3차 녹화 윤리 그대로 승인됨.

**리뷰 반영**: 지금 도구는 설계와 맞지 않는다. `--supplement` 는 **사이트 이름**을 받아 probe 를 온라인으로 통째로 다시 돌리고, 녹화된 robots 판정을 다시 쓰며(`compat-corpus.sh` 249–251행 — 다시 확인하지 않음), fetch/XHR·GET 비콘까지 모든 GET 을 보낸다(`net/cache.rs` 는 GET 이 아닌 것만 막음). 2초 간격도 사이트 사이에만 있다. 그래서 0단계에 새 도구 `--refetch` 를 만든다(0단계 S). crates.io 는 문서만(N1-나), 보충 ②는 제3자 규칙 N6-가로 걸러 받는다.

### D2. 사이트를 더할까

| 선택지 | 내용 |
|---|---|
| **가 (권고)** | 더하지 않는다. 25곳 그대로(측정 20곳) |
| 나 | 3~5곳을 "4차 추가분"으로 녹화하되 목표 비율에서는 빼고 따로 보고 |
| 다 | 에이전트 과제 모양의 둘째 쪽(검색 결과·문서 안 링크를 따라간 쪽 등)을 사이트마다 1장 |

권고 이유: 4차의 주 대상은 지금 20곳에서 이미 많이 잡힌다. 스크립트 에러가 16곳, 클릭이 거절될 관찰 요소가 10곳, 뷰포트보다 넓은 상자가 8곳에 있다. **다**는 가치가 크지만 따라갈 쪽을 고르는 일이 따로 필요해 5차 후보로 둔다.

**결정(2026-11-28, 사용자)**: **가** — 사이트를 더하지 않는다(25곳, 측정 20곳). **다**(둘째 쪽 녹화)는 5차 후보로 남긴다.

### D3. JS 항목을 4차에 넣을까, 따로 JS 차수로 뺄까

측정(리뷰 뒤 고침): 20곳 중 **16곳**에 최상위 `Uncaught` 가 있다. 그중 **10곳**의 *첫* 에러가 클래식 스크립트용 작은 API 가 없어서다(bbc·d2·example·google·hani·kakaocorp·microsoft·naver·velog·yna). 나머지 6곳(apple·daum·docsrs·enwiki·kowiki·korea-kr)은 첫 에러가 page-bug 모양이지만, 그 가운데 4곳은 뒤쪽 줄에 없는 API 가 있다. 그래서 **어떤 줄이든 없는 API 가 있는 사이트는 14곳**이다([부록 B](#부록-b--js-에러-사이트별)).

가장 흔한 것은 `getElementsByTagName` 이다 — 첫 에러 5곳(naver·hani·kakaocorp·bbc·microsoft) + 뒤쪽 줄에만 3곳(google·korea-kr·yna), 모두 8곳. jQuery 1.9.1 초기화도 여기서 죽는다. 그 밖은 `matchMedia`·`customElements`·`URL`·`TextEncoder`·`structuredClone`·`insertAdjacentHTML`·`IntersectionObserver`…다. 첫 page-bug 모양 가운데 enwiki·kowiki(`match` of undefined)와 korea-kr(`length` of undefined)는 `document.cookie` 부재로 추정한다.

ES 모듈이 필요한 것으로 확인된 곳은 naver 하나다(모듈 스크립트는 5곳에 있다).

| 선택지 | 내용 | 득 | 실 |
|---|---|---|---|
| 가 | JS 전부(작은 API + ES 모듈) 4차에 | 한 번에 끝 | 모듈 로더는 그 자체로 L 규모 — CSS·레이아웃 단계가 밀림 |
| **나 (권고)** | **작은 클래식 API 는 4차 2단계**, ES 모듈은 **별도 설계 문서**(spa-roadmap Tier 2 — 4차 다음) | 14곳의 없는 API 를 직접 겨냥 | naver 본문은 4차 뒤에도 비어 있음 |
| 다 | JS 는 빼고 CSS·레이아웃만 | 3차와 같은 성격 | 기대 컨트롤 누락 7곳 중 5곳이 JS 원인 — 에이전트 지표가 거의 안 움직임 |

권고 이유: 에이전트가 못 찾는 컨트롤의 대부분이 "스크립트가 일찍 죽어서 그 컨트롤을 안 그림"이다([부록 C](#부록-c--기대-컨트롤-누락-진단)). 작은 API 하나하나는 크지 않고, 3차 견고성 규칙을 따르면 된다. 다만 양파 구조라 끝이 불확실해서, 2단계는 **개수로 묶는다**(2단계 "진행 방식").

**결정(2026-11-28, 사용자)**: **나** — 작은 클래식 API 는 4차 2단계, ES 모듈은 별도 설계 문서(spa-roadmap Tier 2, 4차 다음).

### D4. 목표 수준

**N2 가 이 절을 대체한다**(근거가 틀렸다: ④ 15곳 → 10곳, ② 미측정, ③ 이 늘 수 있음). 아래 표는 기록으로만 남긴다 — 목표는 [N2](#n2-d4-목표-고쳐-쓰기-d4-를-다시-정함).

| 지표 (초안 정의) | 지금 값 | 보수 | 권고 | 공격 |
|---|---:|---:|---:|---:|
| 견고성: panic/timeout/nav-fail · `[mb-budget]` | 0/0/1 · 0 | 0/0/1 · 0 | 0/0/0 · 0 | 같음 |
| ① 기대 컨트롤 누락 사이트 | 7/20 | ≤5 | ≤3 | ≤2 |
| ② `obs_unreachable` | 미측정 | −50% | −75% | −90% |
| ③ 최상위 `Uncaught` 줄 | 96 | −40% | −60% | −80% |
| ④ "없는 API" 가 첫 에러인 사이트 | ~~15~~ **10** | ≤8 | ≤4 | ≤2 |
| 보조: 합산 버린 선언 ours | 1,356 | −30% | −50% | −70% |
| 보조: 버린 규칙 ours | 1,506 | −80% | −85% | −95% |

**결정(2026-11-28, 사용자)**: **권고** 열을 4차 목표로 한다. 수치는 0단계 끝 `r4-base` 로 다시 적되 비율은 그대로 둔다.

**대체됨(2026-11-28, 사용자 — N2)**: 이 절의 표와 결정은 [N2](#n2-d4-목표-고쳐-쓰기-d4-를-다시-정함)가 대체한다. 4차 목표는 N2 의 표 하나뿐이다(이 절은 기록으로만).

### D5. 관찰 계약: `visibility: hidden` 요소를 관찰에서 뺄까

지금 엔진은 `visibility` 를 **아무도 읽지 않는다.** `css::properties::RENDERED` 밖이고 상속 속성 목록에도 없다. 16곳에 선언 303개가 "파싱만"으로 남는다. 그래서 `visibility: hidden` 인 메뉴·대화상자·드롭다운이 그려지고, 관찰에 나오고, 클릭된다. Chrome 은 그리지 않고, 접근성 트리에서 빼고, 클릭이 통과한다.

| 선택지 | 내용 |
|---|---|
| **가 (권고)** | Chrome 처럼: 계산값이 `hidden`/`collapse` 인 상자는 그리지 않고, 히트 테스트·링크 목록이 건너뛰고(자손이 `visible` 이면 그 자손은 보임), **관찰에서도 뺀다** |
| 나 | 그리기·히트 테스트만 고치고 관찰엔 남긴다 |

권고 이유: 3차 3a 의 "관찰은 클립을 안 본다(sr-only 텍스트도 관찰)"는 sr-only 텍스트가 보조 기술에 읽히는 글이어서 정한 규칙이다. `visibility: hidden` 은 보조 기술에도 안 읽힌다. 관찰에서 빼는 쪽이 같은 원칙(a11y 식)이다.

**결정(2026-11-28, 사용자)**: **가** — 계산값 `visibility: hidden`/`collapse` 인 요소는 그리지 않고, 히트 테스트가 건너뛰고, 관찰에서도 뺀다(1a).

**리뷰 반영**(표준과 맞추며 붙는 것 — 결정의 범위 안):
- 접근 가능한 이름(accname 1.2 §2A/2B): 숨은 자손의 글은 이름에서 빠진다. 단 `aria-labelledby`/`aria-describedby` 가 **직접 가리킨** 숨은 요소의 글은 들어간다.
- 숨은 요소에는 클릭·`type`·`select`·포커스를 모두 거절한다. Tab 순서에서도 뺀다.
- `<col>`/`<colgroup>` 의 `collapse` 는 셀에 상속되지 않는다.
- `inert`·`opacity:0`·`aria-hidden` 은 [N4](#n4-d5-의-경계-사례-셋-관찰-계약)에서 정했다(`inert` 는 빼고, `opacity:0` 은 남기고, `aria-hidden` 은 4차에서 무시 — 1a).
- **잃는 것(위험)**: `:hover` 로만 열리는 메뉴(호버하면 `visibility:visible`)의 링크가 관찰에서 사라진다. 에이전트에게는 호버 도구가 없고, 부모가 링크면 부모를 누르는 순간 이동해 버린다. 0단계 P1 표본이 "`:hover`/`:focus-within` 규칙이 드러내는 것"을 따로 분류하고, **5차 후보 "hover 도구"**로 적는다.

### D6. 기대 컨트롤 철자 고치기 (측정 정의 변경)

d2 의 기대 컨트롤 `link:helloworld` 는 관찰에 실제로 있는 링크 이름 **"Hello world"** 와 맞지 않는다. 이름 부분 일치가 띄어쓰기에서 걸린다. **권고: `link:hello world` 로 고친다**(0단계, 이 문서의 표를 먼저 고친 뒤 tsv).

**결정(2026-11-28, 사용자)**: 고친다 — d2 기대 컨트롤 `link:hello world`(0단계 T).

### D7. 4차에서 일부러 뺄 것 확정

| 항목 | 권고 | 이유(측정) |
|---|---|---|
| ES 모듈(Tier 2) | **뺀다** → 별도 설계 문서 | D3 |
| `<body>` 를 taffy 고유 노드로(3차 5s 선택지 (a)) | **뺀다** → 5차 | 3차 5s: 오늘 효과 1상자. 4차 레이아웃 항목은 전부 레거시 쪽 수정 |
| `:is()` 매칭 메모(결합자 중첩 상한 2 올리기) | **뺀다** | 상한에 걸리는 규칙 13개(mdn 8·github 5). 3차 견고성 결정을 뒤집을 근거 없음 |
| `:has()` | **뺀다** → 5차(무효화 설계와 함께) | 125규칙·10곳(github 71). 일의 대부분은 무효화. 에이전트 영향은 주로 폼 꾸밈 |
| CSS 생성 콘텐츠 **그리기** | **뺀다** → 5차. 4차는 `content` **파싱**과 **접근 가능한 이름에 넣기**(5c — 리뷰 반영) | 파싱만 6,062선언·18곳, 대부분 아이콘 글꼴 글리프 |
| 네이티브 CSS 중첩(`&`) | **뺀다** → 5차 | velog 한 곳 73규칙 |
| `@import`·`@container` | **뺀다**(0단계 P6 에서 바이트만 잼) | `@import` 문 yna 3·d2 2, `@container` 원문 22 |
| `border` longhand 체계(3차 1b 후보) | **뺀다** | 동작 차이 없음 |
| R7 레이아웃 상자의 스타일 깊은 복사 | **뺀다** → 별도 작업 | 코퍼스 `[mb-budget]` 0 |
| 이미지 타일 캐시(3차 F3b 기록) | **뺀다** | 코퍼스 래스터 최장 36.5ms |

**결정(2026-11-28, 사용자)**: 위 표 그대로 전부 뺀다(5차 이후·별도 작업).

### D8. `document.cookie` 의 저장 모델 (2단계 세부)

enwiki·kowiki 의 첫 스크립트가 `document.cookie.match(…)` 에서 죽어 `client-nojs` 화면으로 남는다(3차 1r). korea-kr 의 첫 에러도 쿠키 문자열의 `length` 로 보인다(추정). 지금 엔진은 HTTP 쿠키도 다루지 않는다(`net` 에 `Cookie`/`Set-Cookie` 처리가 없다).

| 선택지 | 내용 |
|---|---|
| a | 문서 하나 동안만 사는 문자열 |
| **b (권고)** | 세션 동안 메모리 저장소, 스크립트 읽기·쓰기만, HTTP 헤더와는 안 섞음 |
| c | HTTP `Set-Cookie`/`Cookie` 까지 — 자격증명 규칙과 같이 설계할 별도 결정 |

**결정(2026-11-28, 사용자)**: **b** — 세션 동안 출처별 메모리 저장소, 스크립트 읽기·쓰기만, HTTP `Cookie`/`Set-Cookie` 와 섞지 않는다(2b). (키 = 도메인·경로·이름, 리뷰 반영)

**리뷰 반영(b 의 뜻 그대로, 명세에 맞게 다듬음)**: 쿠키의 키는 "출처"가 아니라 **(도메인, 경로, 이름)**이다(RFC 6265 §5.3).
- **범위**: 포트로 나누지 않는다. `Secure` 가 아닌 쿠키는 http/https 가 공유한다. `Domain=` 쿠키는 하위 도메인이 공유한다. 공개 접미사 목록(PSL)이 없으므로 `Domain` 은 **호스트와 같을 때만** 받는다. 저장은 호스트별이고 host-only 표시를 둔다.
- **경로**: 기본 경로는 문서 경로의 디렉터리다.
- **읽기 결과**: `name=value` 를 `"; "` 로 잇고, 순서는 경로가 긴 것 먼저, 그다음 만든 순서다(§5.4).
- **만료**: `Max-Age` 가 `Expires` 를 이긴다. 0 이하이거나 지난 날짜면 지운다. 시계는 엔진 시계다.
- **속성**: `Secure` 는 보안 출처에서만 받는다. 스크립트가 `HttpOnly` 로 쓰면 통째로 무시한다(6265bis). 불투명 출처는 읽으면 `""`, 쓰기는 무시한다.
- **상한(Robustness 규칙)**: 쿠키 하나 4096바이트, 도메인당 180개(Chrome).

---

## 범위와 순서

| 단계 | 내용 | 왜 이 순서 | 노력 |
|---|---|---|---|
| **0** | 측정·견고성 정비: `mb-agent` 의 도달 판정 함수 + probe 새 열(`obs_unreachable` 합집합·JS 판정·슬리버 줄 수·`misses.tsv`), ours/browser-too 표 동결, D6 철자, `compat4` 골든·eval 만들기, `--refetch` 도구와 보충 ①, **K5** `Accept`, 기준선 `r4-base` | 주 지표를 잴 열이 아직 없다. 3차의 교훈대로 probe 와 분류표를 먼저 바꾸고 옛 엔진으로 "전"을 잰다 | M (3.5일) |
| **1** | 에이전트 정합성: `visibility`·`inert`, `pointer-events` + 가로챈 클릭 거절, **링크도 히트 테스트로**, **히트 테스트 z-order**, in-view centre 클릭, 닫힌 드롭다운, `<noscript>` 파싱 | 관찰·클릭 계약을 바꾸는 단계라 **맨 앞**이어야 이후 요소 diff 를 원인별로 가를 수 있다. 2단계가 살릴 JS 메뉴는 대개 `visibility:hidden` 으로 닫혀 있다 | M (3.5~4일) |
| **2** | 작은 JS API(클래식): 조회·조작 → 플랫폼 객체 → `customElements`, 상대 URL `fetch`/XHR. 끝에 보충 ② | 14곳의 없는 API. **DOM 을 크게 바꾸니 레이아웃·CSS 보다 앞** | L (7일 상당, 개수로 묶음) |
| **3** | 레이아웃: `display: contents`, 블록을 품은 인라인 폭, 뷰포트보다 넓은 상자(진단은 2단계 뒤 라벨로), 표 min-content, shrink-to-fit `%` | 에이전트에게 보이는 결과가 CSS 버림 수보다 크다 | M (5일) |
| **4** | 셀렉터: `:lang()`, `:target`, `:popover-open`(+ UA 숨김 규칙), `:indeterminate`, `:placeholder-shown`, `:read-only/:read-write`, `:dir()`, `:defined`(N5) | 작다. **5단계보다 먼저** — 살아난 규칙의 선언이 5단계 "전"에 들어가게(3차 4a 교훈) | S (2일) |
| **5** | CSS 값: 5a 배치 값 → 5b 색 → 5c 그림·파싱만(+ `content` 를 접근 가능한 이름에) | 보조 지표. 5a 는 배치를 바꾸니 먼저 | M (3~4일) |

**실행 순서 = 번호 순서**: 0 → 1 → 2 → 3 → 4 → 5. 1단계의 `visibility` 는 2단계보다, 4단계의 셀렉터는 5단계보다 먼저여야 측정이 오염되지 않는다. 3과 4는 서로 독립이지만 에이전트 영향이 큰 3을 먼저 둔다.

**노력의 뜻**: 일수는 **상대 크기**다(합계 약 24~26일 상당 — [요약](#4차-전체-노력위험-요약)). 3차는 18일로 어림했지만 달력으로 3일(11-26 녹화 → 11-28 `final`) 만에 끝났고, 중간에 끼운 부분이 10개를 넘었다. 그래서 끝을 정하는 것은 일수가 아니라 **개수 규칙**이다(2단계 "진행 방식", 3c "모양 다섯", 5c "우선순위 넷", 단계당 끼워 넣기 2개).

**범위 밖(5차 이후, [D7](#d7-4차에서-일부러-뺄-것-확정))**: ES 모듈, `<body>` taffy 고유, `:is` 메모, `:has()`, 생성 콘텐츠 그리기, CSS 중첩, `@import`·`@container`, longhand 테두리, R7, 이미지 타일 캐시, 둘째 쪽 녹화(D2 다), HTTP 쿠키(D8 c), hover 도구(D5 위험), 가로 스크롤, `position: sticky`(원문 68·9호스트 — 정적 배치로 읽혀도 클릭 좌표는 맞다), 스크롤 컨테이너 안 스크롤, `-webkit-line-clamp`, Shadow DOM(2c).

**공통 규칙** (3차 [공통 규칙](compat-round3.md#범위와-순서)을 그대로 잇고, 달라지는 것만):
- 실패하는 테스트 먼저. 픽스처는 손으로 쓴 작은 HTML/CSS/JS이고, **예외는 MIT 라이선스 jQuery 1.9.1·1.12.4 원본**이다(2a — 라이선스 머리말 유지, `tests/fixtures/vendor/`). 실사이트 원문은 git-ignored `compat-cache/` 에만 둔다. 이 문서에도 개수·속성 이름·짧은 셀렉터·API 이름만 쓴다.
- 단계 하나 = 커밋 하나(필요하면 하위 커밋). `cargo test --workspace` green + `cargo clippy --workspace --all-targets` clean + `cargo fmt`. 테스트 수 변화는 커밋 본문에. 커밋은 부모 세션이 사용자 승인 뒤에 한다.
- **끼워 넣는 부분**(3차의 2.1a·F1–F4·5m 같은 것): 시작 전에 이 문서에 **이름과 이유**를 먼저 적는다(진행 표 행 + 단계 절 메모). **한 단계에 끼워 넣는 부분이 2개를 넘으면** 3번째를 시작하기 전에 범위를 사용자와 다시 본다.
- 결정성(AGENTS.md):
  - 새 JS API 는 엔진 시계·시드 난수만 쓴다.
  - `matchMedia` 는 `css::media::evaluate` 와 같은 고정 문맥.
  - `screen` 은 뷰포트로 만든 상수.
  - 쿠키 만료는 엔진 시계.
- **측정 프로토콜**: 3차와 같다. 단계마다 ① probe 커밋 → 옛 엔진으로 "전", ② 엔진 커밋 → "후". 판정은 release 빌드로 한다. **각 단계의 "전"은 앞 단계의 "후" 라벨**(그 단계에 P 가 있으면 P 라벨)이다 — 라벨 목록은 [부록 D](#부록-d--측정-재현과-명령). `r4-base-sup` 은 ③·④·보조 지표의 기준일 뿐 단계의 "전"이 아니다. 4차 동안 캐시는 D1 결정대로 **두 번만** 바뀐다: 0단계 보충 ①, 2단계 끝 보충 ②. 그 밖의 보충·재녹화는 이 문서를 먼저 고치고 사용자가 정한다.
- **지표 정의를 바꾸려면 이 문서의 표를 먼저 고친다.** 4차에 새로 동결하는 것:
  - `obs_unreachable` 의 구성과 우선순위(0단계 P2);
  - 도달 판정의 ours/browser-too 표(P2b);
  - JS 판정 목록 파일 `scripts/compat-js-apis.tsv`(P4).
  목록을 바꾸면 문서를 먼저 고치고, **앞선 모든 라벨을 다시 분류**한다. P4 는 저장된 `console.txt` 의 후처리라 엔진을 다시 돌릴 필요가 없다.
- 단계 끝마다 [진행 표](#진행-표)와 [compat-survey.md](compat-survey.md) 의 사이트별 수치를 갱신한다.
- **에이전트 영향 기준**(단계 공통 — 리뷰 반영):
  - 기대 컨트롤 **전부 발견**.
  - **두 라벨에 모두 있는 요소 중 새로 닿지 않게 된 것 = 0**. probe 가 `unreachable.txt` 를 쓰고 `compat-corpus.sh diff` 가 비교한다.
    - 1차 키 = **가장 가까운 `id` 조상 기준의 상대 경로** + 역할 + 이름(2단계가 노드를 끼워 넣어도 `id` 아래 경로는 덜 흔들린다).
    - 2차 검사 = (역할, 이름) **다중집합** 비교(1차 키가 경로 변화로 못 짝지은 것을 사람이 볼 목록으로).
    - 5c 는 이름을 바꾸므로(`content` → 이름) **이름 없는 키**(`id` 상대 경로 + 역할)로 판정한다.
  - 관찰 요소당 `obs_unreachable` 비율이 늘지 않음.
  - 단계 전후 요소 diff 마다 정상/버그 판정을 진행 표 메모에.
  - "총수가 늘지 않음"은 기준이 아니다. 2단계는 JS 가 요소를 더하고, 3·5a 는 기하를 바꾸므로 구조적으로 깨진다. 대신 진행 표에 구성 요소별 변화 열을 둔다.
- **추적 골든 + eval**(단계별 기대표는 [부록 E](#부록-e--골든eval-단계별-기대표)): `mb-agent/tests/fixtures/compat4.html` 과 그 골든, eval 시나리오 `eval/scenarios/compat4-menu.json`, LLM 없는 대응 테스트 `compat4_menu_opens_through_its_script_and_leads_to_docs` — **셋 다 0단계 항목 G 에서 만든다**. eval 실행은 **사용자가 자기 터미널에서** 한다(`scripts/eval-run.sh` → `claude -p`). `compat4.html` 맨 위 주석의 "Chrome 이라면" 목록은 항목마다 **어느 단계에서 맞아지는지** 적는다.

---

## 0단계 — 측정·견고성 정비

### 목표

4차 주 지표를 **잴 수 있게** 하고, 측정을 흐리는 결함(녹화 429, K5, 철자)을 걷어 낸 뒤 4차 기준선을 잡는다. 엔진 동작 변경은 K5 하나다. 도달 판정 함수(P2)는 새 라이브러리 코드지만 아무 동작도 바꾸지 않는다.

### 세부 항목

**P1 — 숨은 관찰 요소**: 관찰된 요소마다 `visibility` 를 판정해 `obs_hidden_visibility` 열과 `summary.txt` 표본(역할·이름 앞 40자)을 낸다.
- 리뷰 반영: 옛 엔진에서는 `visibility`·`pointer-events` 가 **상속 목록에 없어서**(`style/mod.rs` `INHERITED_PROPERTIES`) 계산값에 자기 선언만 남는다. 그래서 P1 은 1a 전까지 **조상을 올라가며** 가장 가까운 선언을 찾는다(`visible` 이 다시 켜면 거기서 멈춤). 1a 뒤에는 자기 계산값만 본다. 결과가 같은지 1a 커밋에서 확인한다.
- 같은 방식으로 `pointer-events: none` 인 관찰 요소를 `obs_pointer_none` 으로 센다.
- 표본은 **`:hover`/`:focus-within` 규칙이 `visible` 로 드러내는 것**을 따로 표시한다(D5 위험의 크기).

**P2 — 닿지 않는 관찰 요소(`obs_unreachable`)**: 에이전트가 보지만 클릭이 제대로 가지 않는 요소의 **합집합**이다. 요소마다 범주 하나이고, 아래 우선순위에서 처음 맞는 것이다(동결):
1. **`hidden`** — `visibility` 가 숨김(P1), 또는 자기·조상에 `inert` 속성(N4). 1a 뒤에는 관찰에서 빠지므로 0.
2. **`clipped`** — 클립이 다 가려 거절됨(3차 3b `elements_clipped_out`, 지금 276).
3. **`offscreen_x`** — 보이는 상자(클립 적용 뒤)와 가로 뷰포트 `[0, 폭)` 의 교집합이 비어 있음. 오늘은 이 클릭이 **성공한다**(사실 1). 사람·스크린샷이 못 보는 것을 누르는 몫이다.
4. **`intercepted`** — 클릭 점(in-view centre — 1c 와 같은 계산)의 히트가 그 요소·자손이 아니고, 히트 노드의 조상 `<label>` 이 그 요소와 연결된 라벨도 아님.
   - 링크도 같은 히트 규칙으로 센다(N3 의 귀결 — 1b 뒤 링크 이동이 히트를 따른다). 옛 엔진에서 링크는 기하 판정으로 이동했지만, 측정은 처음부터 Chrome 규칙으로 한다.
   - 원인을 세 열로 가른다: `pointer_none_overlay`(덮개가 `pointer-events:none`), `z_order`(오늘의 히트가 DFS 문서 순서상 마지막 상자를 고르고 z-index 를 모름 — 페인터 순서로 다시 보면 대상이 맞는 경우), `other`. **셋 다 1b 가 맡는다**(1b-z 가 `z_order` 를 고친다).
- 열: `obs_unreachable` + 범주별 `obs_u_hidden`·`obs_u_clipped`·`obs_u_offscreen_x`·`obs_u_intercepted`(그 안에 원인 셋). 범주별 합 = 총합(겹쳐 세지 않음).
- **스크롤 위치**: probe 의 관찰은 `full_snapshot`(스크롤 0)이다. 판정은 요소마다 **`click` 이 `scroll_into_view` 로 고를 스크롤 위치**를 계산하고, 그 값을 실제 엔진과 같이 `clamp_scroll`(`state/events.rs` 242행 — 휠 입력이 이 함수로 잘린다)로 자른 위치에서 히트를 본다. `reach` 는 두 함수를 그대로 쓴다(따로 계산하지 않음).
- **코드 자리**: 판정은 **mb-agent 라이브러리 함수** `mb_agent::reach`(신규 모듈, `pub`)에 두고 단위 테스트로 고정한다. 그 안에 `index::find_visible_rect` 를 감싼 공개 함수, `click_point` 를 메서드에서 뽑아낸 자유 함수(`click_point(rect, scroll, viewport)`), 범주 판정이 들어간다. `index` 모듈이 지금 `pub` 이 아니어서(`mod index;`) 필요하다. `AgentBrowser::click` 은 같은 함수를 부르게 바꾼다 — 리팩터링만이고 동작은 같다(골든·액션 테스트 그대로). probe 는 그 함수를 부르기만 한다.

**P2b — 도달 판정의 ours/browser-too 표 (P 커밋 전에 동결)**: Chrome 에서도 같은 것은 목표에서 뺀다. 3차 분류표처럼 **결과를 보기 전에** 정한다.

| 범주 | 판정 | 분류 |
|---|---|---|
| `offscreen_x` | 상자가 작성자 오프셋으로 화면 밖에 있음: 자기나 조상의 `transform: translateX(…)` 음수·`left`/`right`/`margin-left` 가 뷰포트 폭 이상 음수(`-9999px` 건너뛰기 링크), 오프캔버스 메뉴(`translateX(-100%)`) | browser-too |
| `offscreen_x` | 그 밖(오른쪽으로 밀린 것 — 넓은 상자·줄바꿈 실패 등 레이아웃) | ours |
| `intercepted` | **세로 작성자 오프셋**: 보이는 상자가 페이지 위쪽 밖에 통째로 있음(아래 끝 ≤ 0 — `top:-100px` 건너뛰기 링크). 클릭 점에 상자가 없다(`no_hit`). 가로의 `-9999px` 와 같은 경우 — **`r4-p0` 를 본 뒤 더함(사용자 결정 2026-11-28): 표가 세로 경우를 빠뜨렸다** | browser-too |
| `intercepted` | 가로챈 요소가 `position: fixed/sticky`(또는 뷰포트를 덮는 `absolute`)이고 뷰포트 넓이의 30% 이상을 덮음(쿠키·모달 덮개) | browser-too |
| `intercepted` | `z_order` 원인 | ours(엔진 갭 — 1b-z 가 고침, 목표 안) |
| `intercepted` | 그 밖 | ours |
| `hidden`·`clipped` | 전부 | ours(Chrome 은 관찰·클릭 대상 자체가 아님 — 우리는 관찰에 내놓음) |

목표 ②는 ours 몫만 센다. 표를 바꾸려면 이 표를 먼저 고친다.

**표 변경 기록**: 세로 작성자 오프셋 행은 `r4-p0` 결과를 본 뒤 더했다(사용자 결정 2026-11-28 — 3차 5r 방식). 옛 정의는 열로 남긴다: `obs_unreachable_ours_p2b0`·`obs_u_intercepted_ours_p2b0`(세로 행 없는 첫 동결 표). 새 정의가 `obs_unreachable_ours`·`obs_u_intercepted_ours`·`obs_unreachable_rate` 다. 범주는 그대로 `intercepted`(원인 `other`)이고 ours/browser-too 구분만 바뀐다. 상자를 0 위에 통째로 두는 것은 음수 오프셋(`top`·`margin-top`·`bottom`·`translateY`)뿐이라 레이아웃 몫을 따로 가르지 않는다.

**P2b 구현 규칙(2026-11-28, `r4-p0` 를 돌리기 전에 적음 — `mb_agent::reach`)**: 표의 말을 코드로 옮길 때 정한 것.
- `offscreen_x` browser-too = 요소나 그 DOM 조상 가운데 하나의 상자가 (a) 자기 `transform` 의 x 이동이 음수(`render::transform_for(..).e < 0` — `translateX(-100%)`·`translate(-50%, …)` 포함) 이거나 (b) `left`·`right`·`margin-left` 가 px 로 `≤ −뷰포트 폭`. `%`·`calc()` 위치는 보지 않는다(ours 쪽으로 — 애매하면 ours).
- `intercepted` browser-too(세로 — 표 변경 기록) = 보이는 상자의 `y + 높이 ≤ 0`. 덮개 규칙과 별개로 판정하고(`Interception::above_page`), 둘 중 하나면 browser-too.
- `intercepted` browser-too(덮개) = 원인이 `z_order` 가 아니고, 히트 노드에서 위로 대상과의 공통 조상 직전까지 올라가는 사이에 `position: fixed/sticky/absolute` 인 요소가 있고, 그 요소의 보이는 상자 ∩ 클릭 때의 뷰포트 넓이가 뷰포트 넓이의 30% 이상. (엔진은 `fixed` 를 스크롤과 함께 움직이는 `absolute` 처럼 배치한다 — 클릭 스크롤 위치의 뷰포트와 겹치는 넓이로 잰다.)
- 원인 순서: 히트 노드가 `pointer-events: none`(가장 가까운 선언) → `pointer_none_overlay`; 아니면 `view::hit_node_in_paint_order`(페인터와 같은 `walk_paint_order` 를 공유하는 새 함수 — 아직 입력에는 안 씀)의 히트가 대상에 닿으면 → `z_order`; 나머지 → `other`.
- "닿음" = 히트 노드가 대상 자신·DOM 자손이거나, 히트 노드의 조상 `<label>` 이 대상을 라벨함(`for` 가 대상 `id` 와 같음, 또는 `for` 없이 대상을 품음).
- 클릭 점 = 1c 의 in-view centre: (보이는 상자 ∩ `[0, 폭)`) 의 가로 가운데, 세로는 오늘 `click_point` 그대로. 스크롤 = `reach::scroll_target`(오늘 `scroll_into_view` 의 목표) → `state::clamped_scroll`(엔진의 `clamp_scroll` 과 같은 함수).
- P1 의 "가장 가까운 선언": 레이아웃 트리를 내려가며 `visibility`(`hidden`/`collapse` 는 숨김, `visible` 은 켬)·`pointer-events`(`none` 은 켬, 그 밖의 키워드는 끔) 선언을 물려준다. `inert` 는 DOM 조상 속성.
- 관찰 요소는 모두 크기 있는 상자가 있어서(관찰 규칙) 상자 없음(`NoBox`)은 합집합 밖이다.
- **P3**: 줄 수 = `round(텍스트 상자 높이 ÷ layout::line_height_px)`(최소 1) — 레이아웃이 텍스트 상자 높이를 `줄 높이 × 줄 수`로 잡는다. 옛 `slivers` 열은 그대로 두고 `slivers_lines` 를 나란히 낸다.
- **P6**: `import_sheets` = 서로 다른 `@import` 대상 수, `import_cached` = 캐시 `index.tsv` 의 URL 가운데 그 경로(쿼리 제거)로 끝나는 것이 있는 대상 수 — 시트를 합친 뒤라 상대 `@import` 의 기준 URL 이 남지 않아 경로 끝 일치로 근사한다.

**P3 — 슬리버 판정의 줄 수**(3차 5b 의 bbc +29 거짓 양성): 슬리버 = `폭 ÷ (글자 수 ÷ 줄 상자 수) < 3px`, 글자 3개 미만은 여전히 제외한다. probe 판정만 바뀌므로 같은 엔진으로 전후를 나란히 적는다.

**P4 — 스크립트 에러 판정**(리뷰 반영: 저장된 `console.txt` 의 **후처리**로 — `compat-corpus.sh classify <라벨>`):
- **목록 파일** `scripts/compat-js-apis.tsv`(P 커밋에 커밋): 판정 대상 API 이름·종류, 그리고 **연쇄 라이브러리 이름**(`jQuery`·`$`·`wcs_do` 처럼 앞선 스크립트가 만들었어야 할 전역).
- **규칙(동결)**:
  - `missing-api`: `not a function (near "a.b")` 의 `b`, `X is not defined` 의 `X` 가 목록의 API.
  - `cascade`: 같은 실행 앞쪽에 `missing-api` 가 있고 `X is not defined` 의 `X` 가 연쇄 라이브러리 이름.
  - `page-bug`: 나머지. 애매하면 이쪽.
  - 한계: `… of undefined` 모양(쿠키 등)은 규칙으로 못 잡는다. 그래서 ④는 수동 판정표로 한다(N2).
- **열**:
  - `js_first_error` — 첫 `Uncaught` 의 정규화한 메시지(위치 제거, 60자). page-bug 여도 남긴다 — 다음 에러를 잃지 않게.
  - `js_missing_api` — 첫 에러가 `missing-api` 면 그 이름.
  - `js_any_missing_api` — 줄 중 `missing-api` 이름들.
  - `js_error_kinds` — `missing-api:N · page-bug:N · cascade:N`, 메시지 + 위치로 **중복 제거**.
  - `uncaught_dedup`.
- **`js_scripts_completed/total`**: 실행한 클래식 스크립트 중 최상위에서 예외 없이 끝난 수 / 전체. 런타임에 probe 전용 카운터가 필요하다(`net::survey_stats` 와 같은 프로세스 전역 — 관찰·행동엔 안 들어감).
- **테스트**: `classify` 에 픽스처 콘솔 몇 개(missing-api·cascade·page-bug·중복 줄)로 기대 열을 고정한다.

**P5 — 컨트롤 누락 이유 줄**: `controls_missing` 항목마다 "DOM 에 그 이름의 요소가 있나"를 `summary.txt` 에 적는다. 있으면 왜 빠졌나(`display:none`·0 크기·`visibility`·인덱스 밖), 없으면 스크립트가 만들었어야 한다는 뜻이다.

**P6 — `@import` 바이트**: 기존 `import` 열(yna 3·d2 2)에 가져올 시트 수·캐시에 있는지를 더한다.

**P7 — 미스 목록 `misses.tsv`**(2차 리뷰 반영 — N6 걸러 받기에 필요): 지금 캐시의 `offline_misses` 는 `"GET URL"` 문자열만 담고 probe 는 그중 20개만 찍는다. 요청을 내는 자리(문서·CSS·스크립트·이미지·폰트·fetch/XHR·비콘 후보)에서 **요청 종류를 함께 기록**하고(`net::survey_stats` 와 같은 probe 전용 경로), probe 가 사이트별 전체 `misses.tsv`(종류·호스트·URL)를 쓴다. N6 필터는 이 파일을 읽는다(사이트 이름 열은 스크립트가 붙임).

**G — 추적 골든·eval 만들기**: `compat4.html` + 골든, `eval/scenarios/compat4-menu.json`, 대응 테스트 `compat4_menu_opens_through_its_script_and_leads_to_docs`(오늘의 틀린 상태를 단언 — [부록 E](#부록-e--골든eval-단계별-기대표)).

**T — D6 철자**: 이 문서 표 → `scripts/compat-sites.tsv` 의 d2 셋째 열을 `link:hello world` 로. (⑥에서 했다 — `r4-k5` 가 `r4-p0` 와 빈 diff 여야 해서 K5·보충 ① 뒤, `r4-base` 바로 전.)

**S — 보충 도구 `--refetch` 와 보충 ①**(리뷰 반영):
- `compat-corpus.sh record --refetch <미스목록파일>`. 파일 한 줄 = 사이트 이름·URL·종류.
- 캐시 키는 (URL, 메서드, `follows_redirects`, 본문)의 해시다(`net/cache.rs` 94–95행). 문서는 `follows_redirects=false`, CSS 는 `true` 라서 bash 로는 계산할 수 없다. 그래서 작은 **Rust 도우미 예제**(`cache_key URL KIND`)가 키를 내고 그 항목만 지운다. 지금 `cache_key` 는 비공개(`net/cache.rs` 224행)라 도우미가 쓸 수 있게 **내보낸다**(동작 변경 없음). 받는 것도 같은 `net` 경로(GET 만)로 한다.
- 지금 `--supplement` 가 녹화된 robots 판정을 다시 쓰는 자리는 `compat-corpus.sh` 249–251행이다 — `--refetch` 는 그 경로를 쓰지 않는다.
- 규칙:
  - 사이트마다 **robots.txt 를 새로 받는다**(재사용 안 함). 금지면 **옛 항목을 그대로 두고** `blocked` 로 기록한다.
  - 요청마다 2초 간격.
  - 결과가 200 이 아니면 `supp_status` 로 기록하고 그 사이트는 `ok*` 를 유지한다.
  - **429 가 다시 오면 재시도하지 않는다.** kowiki 는 CSS 지표에서 빼고 메모한다.
  - `recorded.tsv` 에 날짜·사유를 남긴다.
- 보충 ① = kowiki 스킨 시트 1개 + crates.io **문서 1개**(N1-나). crates.io 는 K5 엔진 커밋 **뒤**에 한다. 그 문서가 내는 하위 요청은 받지 않는다(미스로 남고 `ok*`).
- 보충 ② 의 미스 목록 파일은 P7 의 `misses.tsv` 를 N6-가 규칙으로 거른 것이다(걸러진 수 `supp_skipped`).
- **테스트**: N6 필터(종류·같은 호스트·비콘 흔적 — 픽스처 `misses.tsv`)와 `cache_key` 도우미(엔진이 쓰는 키와 같은지 — 문서·CSS 둘)를 고정한다.
- 도우미가 0.5일을 넘으면 범위를 다시 본다(이 문서에 적고).

**K5 — `Accept` 헤더**: 이동(문서) 요청에는 Chrome 의 문서용 `Accept`(`text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8` 꼴)를 보낸다. 하위 리소스는 종류별 값을 보낸다(CSS `text/css,*/*;q=0.1`, 이미지 `image/*` 꼴, 스크립트 `*/*`). 정책 게이트·리다이렉트 경로는 그대로다.

**F — 견고성 재확인**: 퍼저 긴 실행(레이아웃·CSS·이미지 각 3만, release)을 `cef51c5` 에서 한 번 돌린다.

**기준선 둘**:
- `r4-base`: 0단계 끝(P + T + 보충 ① + K5). 1단계의 "전", ②의 기준(N2 — 판정은 `r4-final`)·목표 창.
- `r4-base-sup`: 2단계 끝 보충 ② 뒤, **`r4-base` 엔진을 새 캐시로 다시 돌린 것**. **③·④·보조 지표의 기준**이다. 3~5단계의 "전"은 앞 단계의 "후"(3단계는 `r4-js-sup` 뒤의 `r4-layout-p`)다.

### 0단계 구현 메모 (2026-11-28)

**① P — probe 와 동결 표**(옛 엔진, 커밋 대기):
- 동결 순서: P2b 구현 규칙(위)과 `scripts/compat-js-apis.tsv`(api 65 · cascade 13)를 먼저 적은 뒤 `r4-p0` 를 돌렸다.
- 라이브러리: `mb_agent::reach`(공개 모듈) — `visible_rect`(`index::find_visible_rect` 를 감쌈)·`scroll_target`·`scroll_for_click`·`click_point`·`in_view_centre`·`ReachContext::classify`. `AgentBrowser::click` 의 `scroll_into_view`·`click_point`·`target_rect` 가 같은 함수를 부른다(리팩터링만 — 골든·액션 테스트 그대로). 엔진 쪽 동작 무변경 추가 셋: `state::clamped_scroll`(`clamp_scroll` 의 식을 뽑은 자유 함수), `render::paint_order`(페인터의 쌓임 걸음을 방문자로 뽑은 `walk_paint_order` — 페인터도 그것을 씀), `view::hit_node_in_paint_order`(입력엔 아직 안 씀), `layout::line_height_px`(P3).
- P7: `net::RequestKind`(document·css·js·img·font·fetch·xhr·other)를 요청 자리에서 넘기고(`net::fetch_as`, `fetch_without_redirects` = document), 캐시가 미스마다 `SurveyMiss { kind, url }` 를 남긴다. 비콘은 따로 종류가 없다 — 캐시가 GET 아닌 것을 이미 막고(`sendBeacon` 심 없음), GET 비콘은 N6 필터가 URL 흔적으로 거른다(S).
- P4: `js::script_survey_stats`(클래식 스크립트 실행 수 / 최상위 예외 없이 끝난 수, `install_document` 의 실행 고리). 판정 코드는 `examples/compat_support/classify.rs` 하나를 probe 와 테스트(`tests/compat_classify.rs` 6개)가 같이 쓴다. `classify final` 이 부록 B 를 그대로 재현한다: 첫 에러 missing-api **10곳**, 어떤 줄이든 **14곳**, 중복 제거 줄 **83**.
- **`r4-p0` 판정**: 기존 77열이 `final` 과 같다(시간 열 5개 제외 — 차이 0), `diff final r4-p0` 비어 있음. 새 열 23개가 25곳에 다 찍힌다(+ `unreachable.txt`·`element_keys.txt`·`misses.tsv`).
- **첫 측정(20곳 합계, 참고 — 목표 창은 `r4-base` 에서)**: 관찰 요소 4,423 중 `obs_unreachable` 832(ours 812, 18.4%) = hidden 359 · clipped 73 · offscreen_x 94(전부 ours) · intercepted 306(ours 286; pointer-none 덮개 1 · z-order 60 · other 245). 클립 거절 276 중 203 은 `hidden` 이 먼저 잡았다(우선순위대로).
- **진단으로 더한 하위 열(범주는 그대로)**: `obs_u_i_other_no_hit`(16 — 클릭 점에 상자가 아예 없음: `top:-…` 로 위로 치운 건너뛰기 링크, naver 8·mdn 2 …)와 `obs_u_i_other_ancestor`(0 — 점이 대상 상자들의 틈에 떨어져 조상이 맞음). `no_hit` 는 P2b 표에 없는 **세로** 작성자 오프셋이라 지금은 ours 로 센다 — 표를 바꿀지는 사용자 결정(아래 "열린 점").
**② K5 — `Accept`**(커밋 대기):
- `net::RequestKind::default_accept` 가 종류별 Chrome 값을 준다: document `text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8`, css `text/css,*/*;q=0.1`, img `image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8`, 그 밖(js·font·fetch·xhr·other) `*/*`.
- `cached_fetch` 의 **네트워크 클로저 안에서만** 붙인다 — 캐시 키(URL·메서드·`follows_redirects`·본문)는 그대로라 동결 캐시가 그대로 맞는다. 요청이 이미 `Accept` 를 가지면(페이지 `fetch`/XHR 헤더, 대소문자 무시) 붙이지 않는다. ureq 는 기본 `accept: */*` 를 보내지만 우리가 준 `accept` 가 있으면 그것 하나만 보낸다(테스트로 확인 — 중복 줄 없음).
- 테스트(실패 먼저 확인): `document_requests_send_an_html_accept_header`·`subresource_requests_send_a_kind_specific_accept`·`a_page_supplied_accept_is_sent_alone`(`net.rs`, 실제 소켓으로 받은 헤더 줄).
- 정책 게이트·리다이렉트 경로는 손대지 않았다(문서 요청은 여전히 `fetch_without_redirects` 로 hop 마다 게이트).
- **`r4-k5` 판정**: `r4-p0` 와 102열 전부 같고(시간 열 제외 — 차이 0), `diff r4-p0 r4-k5` 비어 있음(요소·닿지 않는 요소 모두). `unreachable.txt`·`element_keys.txt`·`console.txt` 도 바이트까지 같다. `misses.tsv` 는 github·yna 에서 **줄 순서만** 달랐다(하위 리소스를 병렬로 받음 — 집합은 같음) → probe 가 이 파일을 정렬해 쓰게 했다.
- 테스트 1193(+3).

- **열린 점**: ① 위로 치운 요소(`no_hit` 16) → **사용자 결정(2026-11-28): browser-too**. 아래 "P2b 세로 행" 참고. ② T(D6 철자)는 `r4-k5` 가 `r4-p0` 와 빈 diff 여야 해서 K5 뒤·`r4-base` 전에 한다.

**P2b 세로 행(사용자 결정 2026-11-28, 3차 5r 방식 — 커밋 대기)**:
- 순서: P2b 표에 행과 "표 변경 기록"을 먼저 적고 → 코드(`reach::Interception` 이 `overlay`·`above_page` 둘을 따로 들고 `browser_too()` = 둘 중 하나, 테스트 `skip_link_above_the_page_top_is_browser_too`) → 옛 정의 열(`obs_unreachable_ours_p2b0`·`obs_u_intercepted_ours_p2b0`)과 `obs_u_i_above_page` 를 새 열 옆에.
- `r4-p0` 다시 분류(오프라인 재생 — 트리에는 커밋 전 K5 가 있지만 `r4-k5` 가 빈 diff 라 같은 재생): 다른 열은 모두 그대로이고 바뀐 것은 정의가 바뀐 세 열(`obs_unreachable_ours`·`_rate`·`obs_u_intercepted_ours`)뿐이다. `obs_unreachable_ours_p2b0` 는 고치기 전 값(812)을 그대로 재현한다.

  | 20곳 합계 | 첫 표(`_p2b0`) | 새 표 |
  |---|---:|---:|
  | `obs_unreachable_ours` | 812 (0.1836) | **796 (0.1800)** |
  | `obs_u_intercepted_ours` | 286 | **270** |
  | 옮긴 몫(`obs_u_i_above_page`) | — | 16 = `no_hit` 16 전부 (naver 8 · kakaocorp 2 · korea-kr 2 · mdn 2 · docsrs 1 · bbc 1) |

**③ S — 보충 도구(네트워크 없음, 커밋 대기)** — 구현 전에 정한 규칙:
- **목록 파일**(`record --refetch` 의 입력): 한 줄 = `이름<TAB>URL<TAB>종류`(종류 = `net::RequestKind` 이름: document·css·js·img·font·fetch·xhr·other). `#` 줄은 주석, `#skipped<TAB>이름<TAB>N` 줄은 N6 필터가 거른 수(`supp_skipped`). 보충 ①은 손으로 쓰고, 보충 ②는 `compat-corpus.sh refetch-list <라벨>` 이 각 사이트의 `misses.tsv` 를 N6 로 걸러 `compat-cache/corpus/<라벨>/refetch-list.tsv` 로 만든다(같은 URL 은 한 번).
- **N6 필터**(`examples/compat_support/refetch_filter.rs`):
  - (iii) 비콘 흔적이 먼저다. 경로·쿼리를 소문자로 바꿔 영숫자가 아닌 글자로 자른 **토큰**이 `collect`·`beacon`·`pixel`·`track` 으로 시작하거나(`collector`·`beacons`·`pixels`·`tracking` 포함), `log`·`logs`·`logging`·`logger` 와 같으면 종류와 무관하게 거른다. 글자 단위 부분 일치로 하면 `logo.png`·`blog`·`catalog` 까지 걸러져서 토큰으로 정했다.
  - (i) 그다음 css·js·font·img 는 호스트와 무관하게 받는다.
  - (ii) 그 밖(document·fetch·xhr·other)은 URL 호스트가 사이트 문서 호스트(그 라벨 `summary.tsv` 의 `final_url`, 없으면 `compat-sites.tsv` URL)와 **글자 그대로 같을 때만** 받는다.
- **받기**(`crates/mb-runtime/examples/survey_cache.rs refetch URL 종류`): `net::survey_refetch` 가 엔진의 `cached_fetch` 와 같은 와이어 경로(GET 만, 종류별 `Accept`, document 는 리다이렉트를 따라가지 않음)로 받는다. 캐시 항목은 **200 일 때만** 쓴다(옛 항목을 덮음). 200 이 아니면 캐시를 바꾸지 않는다 — 옛 항목은 그대로이고, 미스였으면 미스로 남아 `ok*` 를 유지한다. 그래서 일부러 지울 일이 없다. 키는 `net::cache_key`(내보냄)·`net::survey_cache_key(종류, URL)`(document = `follows_redirects` 거짓)이고, 도우미 `key URL 종류` 가 같은 값을 찍는다.
- **robots**: 받을 URL 의 **출처마다** 그 실행에서 robots.txt 를 새로 한 번 받는다(사이트 문서 출처만이 아니라 CDN 출처도 — 3차보다 엄격). `User-agent: *` 그룹, 3차와 같은 판정기. 금지면 그 출처의 URL 은 받지 않고 `blocked` 로 적는다. 원문은 `compat-cache/corpus/robots/refetch-<UTC 날짜시각>/<호스트>.txt`.
- **예의**: 순차, robots 요청과 문서·리소스 요청을 모두 포함해 **요청 사이 2초**, 재시도 없음(429 도), 3차와 같은 데스크톱 UA(식별 토큰 없음).
- **기록**: `compat-cache/corpus/refetched.tsv`(이름·URL·종류·robots 판정·근거 규칙·UTC 날짜·`supp_status`). `supp_status` = HTTP 코드, 또는 `blocked`·`error:…`·`skipped:N`. 문서 초안의 "`recorded.tsv` 에 남긴다"는 따로 적는 파일로 바꿨다 — `run` 이 `recorded.tsv` 의 **마지막 줄**로 robots 판정·날짜를 읽어서, 보충 기록을 거기 더하면 재생 판정이 바뀐다. `run` 은 보고서 끝에 `supp_status`(그 사이트 보충의 `코드:개수`, URL 마다 마지막 시도)·`supp_skipped` 두 열을 붙인다.
- **구현 결과**:
  - `net::cache_key`·`net::survey_cache_key` 를 내보냈다(동작 무변경). `net::survey_refetch` 와 `RequestKind::parse` 를 더했다. `cached_fetch` 의 와이어 부분은 `wire_fetch` 로 뽑아 엔진과 보충이 같이 쓴다.
  - 도우미 예제 `survey_cache key|refetch URL 종류`. `MB_NET_CACHE` 가 없거나 `MB_NET_CACHE_OFFLINE` 이면 받지 않고 에러로 끝난다.
  - 스크립트: `refetch-list <라벨> [이름…]`, `record --refetch <목록>`, 보고서 두 열. `COMPAT_CORPUS_DIR` 로 다른 ignored 코퍼스 디렉터리를 줄 수 있다(시험용). 도우미는 0.5일 안에 끝났다.
- **테스트**: `survey_cache_key_names_the_entry_the_engine_writes`(document·CSS), `refetch_stores_only_a_200_and_otherwise_keeps_the_old_entry`(429 → 그대로, 200 → 덮음, 미스 + 404 → 미스 유지 — `net/cache.rs`), `tests/compat_refetch_filter.rs` 3개(손으로 쓴 `fixtures/compat-misses.tsv` — 가짜 호스트).
- **실사이트 없이 확인**:
  - `refetch-list r4-k5`: 8곳 42 URL(img 40 · css 1 · js 1), 걸러진 것 0. 이 목록은 보충 ②(2단계) 몫이라 지웠다.
  - 루프백(127.0.0.1) 서버로 `record --refetch` 를 시험했다(임시 코퍼스·캐시, 끝나고 지움). robots 를 한 번 새로 받았고, 요청 사이가 2초였고, `Disallow` URL 은 요청하지 않고 `blocked` 로 남았고, 404 는 캐시를 안 바꿨고(`kept`), 200 은 도우미 키 이름으로 저장됐고, `#skipped` 가 적혔다.
  - 전체 오프라인 `run`(임시 라벨)은 `r4-p0` 와 105열 같고 diff 가 비었다. 새 두 열은 `-` 다.
- 테스트 1199(+5).

**④ 보충 ① — 실사이트 방문(2026-11-28 11:10 UTC, D1-B·N1-나, 사용자 승인)**:
- 목록 파일 `compat-cache/corpus/supplement-1.tsv`(손으로 씀, ignored): 두 줄뿐이다.
  - 동결 캐시 `index.tsv` 의 kowiki 429 항목은 둘이었다. 그중 스킨 시트 `https://ko.wikipedia.org/w/load.php?lang=ko&modules=…&only=styles&skin=vector-2022`(css — 기존 항목이 CSS 키로 있음을 `survey_cache key` 로 확인) 하나만 넣었다. 다른 하나(`/wiki/Special:CentralAutoLogin/start?…type=1x1…`, 1×1 로그인 추적 이미지)는 넣지 않았다.
  - crates.io 는 `https://crates.io/`(document, 기존 항목 404 · 빈 본문) 하나다.
- **보낸 요청 = 4개**(순차, 2초 간격, GET, 3차 UA, 재시도 없음):

  | 순서 | 요청 | 결과 |
  |---|---|---|
  | 1 | `https://ko.wikipedia.org/robots.txt` | 200 — `*` 그룹 `Allow: /w/load.php?` → 허용 |
  | 2 | kowiki `load.php?…only=styles&skin=vector-2022` | **429 다시** → 재시도 없이 멈춤, 캐시 안 바꿈(옛 항목 바이트 그대로) |
  | 3 | `https://crates.io/robots.txt` | 200 — 맞는 규칙 없음 → 허용 |
  | 4 | `https://crates.io/`(K5 의 문서 `Accept`) | **200** `text/html`, 5,500바이트 → 저장(옛 404 항목을 덮음) |

- 기록: `refetched.tsv` 두 줄, robots 원문 `robots/refetch-20261128T111015Z/`. 새 파일은 모두 `git check-ignore` 로 무시 대상임을 확인했고, 저장소에 추적되는 `compat-cache/` 파일은 0개, 작업 트리는 깨끗했다.
- **오프라인 확인 라벨 `r4-sup1`**(부록 D 의 동결 라벨이 아니다 — 보충 효과만 보는 확인용. `r4-base` 는 T 뒤 ⑥):
  - `r4-p0` 와 다른 사이트는 crates.io 하나다. kowiki 는 모든 열이 같다(`supp_status` 429:1).
  - crates.io: `nav-fail` → **`ok*`**(최종 URL 생김, 앱 껍데기 — 미스 6: css 5 · img 1, 받지 않음). 요소 0, 클래식 스크립트 1/2 완주, `Uncaught` 1줄(page-bug 모양). N1-나대로 견고성 지표에만 들고 ①·②·③·④·보조 합계에서는 뺀다.
- **kowiki 는 CSS 지표에서 뺀다**(0단계 S 규칙 "429 가 다시 오면"): 스킨 시트가 4차 내내 없으므로 kowiki 의 `decl_drop_*`·`rule_drop_*`·합산 버린 선언은 보조 지표 합계(`r4-base-sup`·`r4-final`)에서 빼고 따로 적는다. 에이전트 지표(①·②)에는 그대로 든다 — 스킨 없이 보이는 화면이 우리가 실제로 관찰하는 화면이다. 보충 ②는 이 시트를 저절로 다시 요청하지 않는다 — 캐시에 429 항목이 있어 오프라인 미스가 아니므로 `misses.tsv` 에 안 나온다.

**⑤ G — 추적 골든·eval(커밋 대기)**:
- `crates/mb-agent/tests/fixtures/compat4.html`(손으로 씀, 패턴만). 맨 위 주석의 "Chrome 이라면" 목록 9항목에 고치는 단계를 적었다: ① `visibility` 메뉴(1a → 2) · ② 메뉴 스크립트가 첫 줄 `getElementsByTagName` 에서 죽음(2a·2b, `insertAdjacentHTML` "Sign in" 도 2a) · ③ `inert` 패널(1a) · ④ `opacity:0` 버튼(모든 단계 유지, N4) · ⑤ `aria-hidden` 링크(4차 유지, N4) · ⑥ `<noscript><style>` 이 "Search as you type" 버튼을 숨김(1e) · ⑦ 닫힌 `[popover]`(4) · ⑧ `::before { content }` 만 있는 아이콘 버튼 이름(5c) · ⑨ `pointer-events:none` 장식 아래 "Subscribe"(관찰은 그대로, 클릭은 1b — probe 의 도달 열이 셈).
- **골든은 스크립트를 돌린다**(compat3 와 다른 점): `tests/golden.rs` `compat4` 가 실제 `AgentBrowser`(대체 글꼴·가상 시계·800×600)에 `install_document` 로 문서를 넣고, **불러온 직후**와 **"Menu" 클릭 뒤** 두 관찰을 한 골든 파일(`compat4.observation.txt`)에 적는다. 부록 E 의 골든 열("1 메뉴 항목 사라짐 · 2 메뉴 버튼으로 열림")은 클릭 뒤 관찰이 있어야 보이고, 2단계가 바꾸는 콘솔 줄도 관찰에 나온다. 결정적인지는 여러 번 돌려 확인했다.
- **만들며 바로잡은 것**: 초안의 ⑥은 `<noscript>` 안 **링크**였는데, 오늘도 관찰되지 않는다(UA 가 `<noscript>` 를 그리지 않음). 그래서 1e 의 증상은 `<noscript>` 안 `<style>` 이 적용되는 쪽(3차 docsrs −1 과 같은 모양)으로 바꿨다.
- 오늘 골든: 요소 9개(Menu · Docs · Pricing · Download draft · Send feedback · Background link · Popover tip · 이름 없는 버튼 · Subscribe). 콘솔 `Uncaught … near "document.getElementsByTagName"` 1줄. "Menu" 클릭 뒤 `+0 / -0`.
- `eval/scenarios/compat4-menu.json`("메뉴를 열어 Docs 로 가서 내용을 말하라", 성공 = `/compat4-docs.html`). 페이지는 `examples/agent-demo/compat4.html`(골든 픽스처의 심볼릭 링크)과 `compat4-docs.html`. mb-mcp `every_committed_scenario_parses` 가 `compat4-menu` 도 요구하게 했다. **LLM eval 은 돌리지 않았다** — 사용자 터미널 몫(`scripts/eval-run.sh eval/scenarios/compat4-menu.json 3`, 0단계 기대 3/3 — 틀린 이유로).
- 대응 테스트 `compat4_menu_opens_through_its_script_and_leads_to_docs`(actions.rs, LLM 없음): 오늘의 틀린 상태를 단언한다. 스크립트가 첫 줄에서 죽고, "Docs" 가 처음부터 관찰되며, "Menu" 를 눌러도 `+0 / -0` 이고, Docs 를 누르면 이동한다. 1·2단계에서 단언을 다시 쓴다(주석에 적음).
- 테스트 1201(+2).

**⑥ T + `r4-base` + N2 목표 창(커밋 대기 — 목표 창 결정됨 2026-11-28)**:
- T: 부록 C 의 d2 줄 → `scripts/compat-sites.tsv` d2 셋째 열 `link:hello world`.
- **`r4-base`**(오프라인·release, P + K5 + P2b 세로 행 + S + 보충 ① + T): 확인 라벨 `r4-sup1` 과 다른 것은 d2 의 `controls_found` 0/1 → 1/1 하나뿐이다(T 의 몫).
  - 견고성: ok 12 + ok\* 9(crates.io 포함) · nav-fail **0** · panic 0 · timeout 0 · blocked 2 · robots skip 2 · `[mb-budget]` 0.
  - ① 기대 컨트롤 누락: **6/20**(naver·daum·hani·korea-kr·velog·google).
  - ② `obs_unreachable` ours: **0.1800(796 / 4,423)**. 첫 P2b 표로는 0.1836(812). 구성: hidden 359 · clipped 73 · offscreen_x ours 94 · intercepted ours 270(z-order 60 · pointer-none 덮개 1 · other 209). browser-too 36(덮개 20 · 페이지 위 16).
  - ③ `Uncaught` 중복 제거 83(중복 포함 96) · 클래식 스크립트 완주 **179/272(65.8%)**. ④ 첫 에러 missing-api 10 · 어떤 줄이든 14.
  - 보조: 합산 버린 선언 ours 1,356(kowiki 10 을 빼면 1,346 — ④ 규칙대로 보조 합계에서 뺄 몫) · 버린 규칙 ours 1,506(참고). 보조 지표의 기준은 여전히 `r4-base-sup` 이다.
- **N2 목표 창(②)**: 아래 "② 목표 창" — 사용자가 제안대로 결정(2026-11-28).

#### ② 목표 창 (`r4-base`, 2026-11-28 — 결정)

**결정(2026-11-28, 사용자) — 이후 조정 없음, 못 맞추면 "못 맞춤"**: 아래 제안을 그대로 받았다. ① 구성 요소 목표는 절대 수, 총합 ②는 관찰 요소당 비율과 절대 수를 같이 적는다. ② `hidden` 0. ③ `intercepted`(ours) ≤135(−50%). ④ `offscreen_x`(ours) ≤28(−70%, kowiki 포함). ⑤ `clipped` 목표 없음(참고). 판정 라벨은 `r4-final`, 단계별 값은 참고다.

(아래는 결정 전에 쓴 제안과 근거 — 기록으로 남긴다.)

**단위 제안**: 구성 요소의 목표는 **절대 수**로 잡는다. 총합 ②는 지금처럼 관찰 요소당 비율로 적되, 절대 수를 같이 적는다. 비율로 잡지 않는 이유는 분모가 단계마다 구조적으로 움직이기 때문이다.
- 1a 가 숨은 요소 359 를 관찰에서 빼면 분모가 4,423 → 약 4,064 로 줄어, 다른 구성 요소의 비율이 저절로 9% 오른다.
- 2단계는 JS 가 그린 요소를 더해 분모를 키운다.

| 구성 요소 | `r4-base` 절대(요소당) | 사이트별(큰 순) | 맡는 단계 | N2 출발점 | **제안 목표** | 근거 |
|---|---:|---|---|---|---|---|
| `hidden` | 359 (0.081) | enwiki 169 · bbc 68 · github 48 · kakaocorp 37 · korea-kr 33 · apple 3 · d2 1 | 1a | −100% | **0**(−100%) | 정의상 — 1a 뒤 관찰에서 빠진다 |
| `intercepted`(ours) | 270 (0.061) | mdn 79 · yna 45(z 24) · apple 37(z 20) · microsoft 26 · enwiki 17 · hani 12 · korea-kr 11 · kowiki 10 · docsrs 7(z 5) · d2 6 · velog 5(z 5) … | 1b(+3) | −60% (≤108) | **−50% (≤135)** | 1b 의 코드 수정이 직접 없애는 것은 z-order 60 + pointer-none 1 = 61(23%)뿐이다. 나머지 209 "other" 는 표본으로 보면 거의 다 **레이아웃 겹침**이다: mdn 사이드바 `li`/`ol`/`details` 가 링크 위에 놓임, microsoft 내비 묶음 제목이 링크를 덮음, enwiki 바닥 링크가 위로 겹침, hani 이미지·광고 상자. 이것들은 3단계(3a `display: contents` — mdn 본문 폭이 부록 A.3 대상, 3c 넓은 상자) 몫이다. −50% = 1b 의 61 + mdn 79 의 대부분. −60% 는 그 위에 22개를 더 없애야 한다 |
| `offscreen_x`(ours) | 94 (0.021) | github 24 · microsoft 24 · enwiki 20 · kowiki 14 · hani 8 · google 4 | 3 | −80% (≤19) | **−70% (≤28)** | 가장 넓은 상자: microsoft 5,117 · google 3,350 · kowiki 2,528 · enwiki 2,338(인용 `span`) · github·hani 1,280(뷰포트 폭 — 안쪽 줄바꿈 문제)이다. 3c 가 겨냥한다. **kowiki 14 는 4차에 안 움직인다** — 스킨 시트 429(보충 ①)라 Chrome 이 받는 CSS 가 없다. −80% 는 kowiki 를 뺀 80 을 거의 다(≥75) 없애야 한다. kowiki 를 ② 에서도 빼자는 안(그러면 80 → ≤16)은 택하지 않았다 — 스킨 없는 화면이 우리가 실제로 관찰하는 화면이다(④ 메모) |
| `clipped` | 73 (0.017) | hani 48 · mdn 11 · yna 5 · d2 3 · github 3 · korea-kr 2 · enwiki 1 | — | 목표 없음 | **목표 없음(참고)** | N2 그대로. 클립 거절 276 중 203 은 `hidden` 이 먼저 잡았다. 1a 뒤 그 203 이 관찰에서 빠지므로 이 수는 거의 그대로일 것이다 |

- 제안대로면 `r4-final` 의 ② ours ≈ 0 + 135 + 28 + 73 = **236 이하**다. 1a·2단계 뒤 분모를 약 4,064 로 보면 0.058 이하(`r4-base` 0.180)가 된다. 이 값은 참고이고, 판정은 구성 요소별 절대 수로 한다.
- browser-too 36(덮개 20 · 페이지 위 16)은 목표 밖이다.
- **정한 것(사용자, 2026-11-28)**: ① 단위(구성 요소 = 절대 수, 총합 = 비율 + 절대 수), ② `intercepted` −50%(N2 출발점 −60% 대신), ③ `offscreen_x` −70%(출발점 −80% 대신, kowiki 포함), ④ `hidden` 0 · `clipped` 목표 없음 — 모두 제안대로(위 "결정" 줄). 그 뒤로는 조정하지 않는다(N2).
- **F — 퍼저 긴 실행**: 계획은 `cef51c5` 에서였으나 **`e034ae2`(+ 커밋 전 ⑥ 문서)로 바꿔 돌렸다**(부모 세션 결정 2026-11-28). 0단계의 엔진 쪽 변경은 동작을 바꾸지 않는 리팩터링(페인터 걸음 `walk_paint_order`·`clamped_scroll`·`RequestKind` 배관)과 K5 의 `Accept` 헤더(퍼저가 닿지 않는 네트워크 경로)뿐이라, 지금 HEAD 가 같은 것을 재고 페인터 리팩터링까지 덮는다. 결과: 2026-11-28 11:22 UTC, `MB_FUZZ_CASES=30000 cargo test --release -p mb-engine --test fuzz_layout -- --ignored --nocapture --test-threads=1`(시드 1부터, 시스템 글꼴 없음). 셋 다 실패 0 — 레이아웃 30,000 사례 81.8초(2.73 ms/사례, 가장 느림 157ms · 시드 17421 — 1초 상한 안, 무한 값 사례 8,228, 가장 큰 트리 769노드, 실패 서명 0) · CSS 토큰 30,000 입력 11.6초(살아남은 선언 239,804, 패닉 지점 0) · 이미지 바이트 30,000 입력 13.6초(디코드 4,524 — 가장 큰 4096×4096, 가장 느림 53ms, 실패 0). 0단계는 이것으로 끝났다.

### 설계 결정

- **합집합과 우선순위**: `hidden` 을 맨 앞에 둔다. 1단계가 그것을 관찰에서 빼면 그 요소는 관찰 요소 자체에서 사라지므로, 다른 범주로 옮겨 가 이중으로 줄어든 것처럼 보이지 않는다.
- **K5 를 0단계에 두는 이유**: nav-fail(견고성)이고, 고쳐야 보충 ①로 crates.io 를 넣을 수 있다. 오프라인 재생은 캐시 키에 헤더가 없어 바뀌지 않는다. 그래서 K5 커밋의 코퍼스 diff 는 비어 있어야 하고, 비지 않으면 버그다. `Accept` 는 캐시 키에 넣지 않는다(동결 캐시 전체가 미스가 됨).
- **P4 를 후처리로 두는 이유**: 목록이 바뀔 때마다 앞선 라벨을 다시 분류해야 하는데(공통 규칙), 엔진을 다시 돌리지 않고 저장된 콘솔로 할 수 있다.

### 테스트 (0단계)

- `document_requests_send_an_html_accept_header` / `subresource_requests_send_a_kind_specific_accept`(테스트 서버가 받은 헤더 확인 — ureq 의 소문자 헤더 함정).
- `mb_agent::reach` 단위 테스트(손 픽스처): `hidden_wins_over_clipped`, `offscreen_x_is_an_empty_horizontal_intersection_not_an_off_screen_centre`(5,117px 상자는 아님), `link_under_an_overlay_is_intercepted_like_chrome`(히트 규칙), `button_under_an_overlay_is_intercepted`, `z_order_interception_is_classified_separately`, `label_ancestor_of_the_hit_counts_as_the_target`, `reach_uses_the_clamped_scroll_position_click_would_use`.
- `click` 리팩터링: 기존 액션 테스트·골든 그대로(리팩터링만).
- `compat4.html` 골든: 오늘의 틀린 관찰을 기록한다.

### 측정·완료 기준 (0단계)

| 기준 | 판정 |
|---|---|
| P 커밋(옛 엔진) 기존 열이 `final` 과 같다 | 시간 열 빼고 동일 · 요소 diff 비어 있음 |
| 새 열이 25곳에 다 찍힌다 | `obs_*`·`js_*`·`unreachable.txt`·이유 줄 |
| K5 커밋 코퍼스 diff | 라벨 `r4-k5` 가 `r4-p0` 와 비어 있는 diff(열 동일) |
| 보충 ① 뒤 | kowiki `css_bytes` 증가(아니면 `supp_status`), crates.io `nav-fail` → `ok*`(N1-나 — 기대 컨트롤 판정은 분모 밖) |
| `r4-base` | 위 전부 뒤 한 번. 진행 표 0행을 다시 적고, **N2 목표 창**(②의 구성 요소별 목표)을 엔진 커밋 전에 정한다 |

### 위험 (0단계)

- **보충이 다른 내용을 받음**: 11-26 의 kowiki 문서와 오늘의 스킨 시트가 안 맞을 수 있다. 그대로 두고 메모한다.
- **ours/browser-too 표가 틀림**: 결과를 본 뒤에 표를 고치면 3차의 함정(목표를 쉽게 만듦)이 된다. 고칠 때는 이유를 적고, 옛 정의 열을 나란히 남긴다(3차 5r 처럼).
- **z-order**: 오늘의 히트가 z-index 를 모르므로 `r4-base` 의 `intercepted` 에 `z_order` 몫이 섞인다. 열을 따로 두었고, 1b-z 가 고치는 몫이라 목표 안이다.

### 노력 (0단계)

M (3.5일): `reach` 함수·테스트 1일, probe 열·P4 후처리·P7 0.75일, G 0.25일, `--refetch` 도우미·K5·픽스처 테스트 0.5~1일, 보충·기준선·문서 0.5일.

---

## 1단계 — 에이전트 정합성

### 목표

에이전트가 보는 요소와 누르는 결과를 Chrome 과 맞춘다. 관찰에 "없어야 할 요소"(숨은 메뉴)와 "눌러도 다른 데로 가는 요소"(덮개)를 없애고, 못 누르는 이유는 **거절로 알린다**(N3). 클릭이 무엇에 닿는지는 사람과 에이전트가 **같은 히트 테스트 하나**로 정한다(링크 포함, z-order 반영).

### 세부 항목

**1a `visibility: hidden/collapse` 와 `inert`** — `visibility` 는 지금 아무도 읽지 않는다. 그려지고, 관찰되고, 클릭된다(선언 303·16곳). 고칠 곳:
- 스타일: `INHERITED_PROPERTIES` 에 추가, `RENDERED` 에 넣기. 값 종류는 이미 `Keywords(visible|hidden|collapse)` 다(`properties.rs` 769–772행).
- 페인트 — **모든 경로**:
  - 상자 자신의 배경·테두리·글자(`paint_self`);
  - 이미지(`view.rs` `collect_image_commands_into`);
  - 링크 밑줄(`view.rs` `link_decoration_commands`).
  - 자손은 자기 값대로 그린다.
- 히트 테스트: 그 상자 기록을 건너뛰고 자손은 계속 본다(3b 클립 규칙과 같은 모양). 링크 이동이 히트를 따르게 되므로(1b-링크) 숨은 링크는 이동하지 않는다. `LinkTarget` 목록(커서·밑줄용)에서도 뺀다.
- 관찰(D5): 요소와 `text:` 블록에서 빼고, accname 규칙을 따른다.
- 행동: 클릭·`type`·`select`·포커스 → `NotInteractable`(사유 `Hidden`). Tab 순서에서 뺀다.
- `collapse`: 표 행·열이 아니면 `hidden` 과 같다. 표 행은 4차에서 `hidden` 으로 근사하고 메모한다. `<col>/<colgroup>` 의 값은 셀로 상속되지 않는다.
- **문서 높이**: Chrome 은 `visibility:hidden` 상자도 스크롤 넘침에 센다. 그래서 `document_height` 는 숨은 상자를 **계속 센다**. `document_height(&commands)` 처럼 그려진 명령으로 재는 경로가 있으면, 숨은 상자의 범위를 따로 더해 높이가 변하지 않게 한다(테스트로 고정).
- **`inert`(N4)**: 자기·조상에 `inert` 속성이 있으면 관찰에서 빼고, 클릭·`type`·`select`·포커스를 `NotInteractable`(사유 `Inert`)로 거절하고, Tab 순서에서 뺀다. 그리기는 그대로다(Chrome 도 그린다). **히트 테스트는 `pointer-events:none` 처럼 건너뛴다**(HTML 명세). 그래서 `inert` 링크는 사람이 눌러도 이동하지 않는다. `opacity:0` 은 관찰에 남기고(Chrome 과 같음), `aria-hidden` 은 4차에서 무시한다(N4).

**1b `pointer-events: none` + 가로챈 클릭** — 지금은 파싱만이다(335·14곳). 덮개가 JS 클릭 대상을 가린다. 고칠 곳:
- 스타일: 상속 목록에 추가, `RENDERED`. 값 종류 `Other`(`properties.rs` 623행) → `Keywords`(`@supports` 가 맞게 판정하도록). HTML 에서 뜻이 있는 것은 `auto`/`none` 뿐이고, SVG 값은 `auto` 로 본다.
- 히트 테스트: `none` 인 상자는 건너뛰고 `auto` 자손은 받는다. `:hover` 는 `none` 조상에도 맞고, 이벤트는 `none` 조상을 지나 버블한다.
- **1b-링크 — 링크 이동을 히트 테스트로**(N3 의 귀결): 오늘 링크 이동은 `LinkTarget` 목록의 기하 일치(`state/events.rs` 248행 `clicked_link`)다. 이를 **히트된 노드에서 조상으로 올라가 처음 만나는 `href`** 로 바꾼다. 사람·에이전트 공통이다.
  - 덮개가 가로채면 링크 이동이 막힌다(Chrome 과 같음).
  - 숨은·`none`·`inert` 링크는 히트에서 빠지므로 저절로 이동하지 않는다.
  - `LinkTarget` 목록은 커서 모양·밑줄·hover 표시에만 남긴다.
- **1b-z — 히트 테스트의 z-order**: 페인터는 이미 positioned z-층을 z-index 로 정렬한다(`render/display_list.rs` 126–190행 `paint_stacking_context`, 511행 `z_index_of`). 히트 테스트(`view.rs` 555행 `walk_for_hover`)는 DFS 문서 순서상 마지막 상자를 고른다. 히트 테스트가 **페인터와 같은 순서**로 걷게 한다(같은 정렬 함수를 공유 — 순서 규칙을 두 곳에 두지 않음). S 규모(0.5~1일).
- **에이전트 클릭 — 가로챈 클릭 거절(N3)**: 클릭 점의 히트가 대상·자손이 아니고, 히트 노드의 조상 `<label>` 이 대상과 연결된 라벨도 아니면 거절하고 가로챈 요소를 알린다.
  - 라벨: `<label>` 활성화는 **히트 노드에서 시작한다**(`state/mod.rs` 588–597행 → 1658행 `find_checkable_activation_target` 이 조상 라벨을 찾는다). 그래서 히트가 대상 체크박스의 라벨(또는 그 안)이면 "닿음"이다. 체크박스 해킹에서 숨은 체크박스를 누르려 하면 거절하고, 힌트가 그 라벨의 `eN`(이미 발급된 경우)을 가리킨다.
  - 링크도 같다(1b-링크 뒤).

**1c in-view centre 클릭(N3)** — 오늘은 x 가 상자 중심 그대로이고 창 밖이어도 클릭이 성공한다(사실 1). 바꿀 것:
- 클릭 점의 x 를 y 처럼 (보이는 상자 ∩ 뷰포트) 가로 범위의 가운데로 당긴다. 5,117px 상자는 화면 안 부분이 눌린다.
- 교집합이 비면 거절한다(사유 `OutsideViewportX`).
- **먼저 오늘 동작을 고정하는 테스트**를 쓴다. 그다음 바꾼다(테스트가 바뀌는 커밋에서 이유를 남김).

**거절 사유와 힌트(N3, 2차 리뷰 반영 — 자리를 다시 잡음)**: `overflow: hidden/auto/scroll` 조상은 이미 클립을 만든다(`clip.rs` 205–213행). 그래서 그런 조상 안에서 화면 밖으로 밀린 요소는 `find_visible_rect` 가 **먼저 `clipped`** 로 거절한다. `offscreen_x` 는 클립 조상이 없거나, 클립 조상 자신이 뷰포트를 넘칠 때만 생긴다. 힌트는 다음 자리에 둔다:

| 사유 | 조건 | 힌트(다음 행동) |
|---|---|---|
| `Clipped` | 가장 가까운 클립 조상이 `overflow: hidden`(또는 `clip`)이고 요소가 그 가로 범위 밖 | 캐러셀·슬라이더일 가능성: "그 컨테이너의 다음/이전 컨트롤을 눌러 보이게 하라" |
| `Clipped` | 클립 조상이 `overflow: auto/scroll` | "엔진은 컨테이너 안을 스크롤하지 못한다. 링크면 그 주소로 `navigate` 하라. 아니면 Tab 으로 포커스한 뒤 Enter" |
| `Clipped` | 그 밖(세로로 잘림 등 — 3차 3b 그대로) | 지금 힌트 그대로 |
| `OutsideViewportX` | 클립 조상 없음 또는 클립 조상이 뷰포트를 넘침 | "페이지가 뷰포트보다 넓고 엔진은 가로로 스크롤하지 못한다. 링크면 그 주소로 `navigate` 하라. 아니면 Tab 으로 포커스한 뒤 Enter" |
| `Intercepted` | 1b | "다른 요소가 덮고 있다: 그 요소를 먼저 닫거나 치워라"(덮은 요소의 역할 — 이름은 아래 규칙) |
| `Hidden`·`Inert` | 1a | "보이지 않는(또는 비활성) 요소다. 그것을 여는 컨트롤을 먼저 써라" |

- **Tab + Enter 는 의도한 우회로다**: 키보드 경로는 히트 테스트를 지나지 않는다. Chrome 도 Tab 으로 포커스한 요소를 Enter 로 활성화한다(가로 스크롤해서 보여 준다). 그래서 `offscreen_x`·컨테이너 클립 요소에는 힌트로 안내한다. `Intercepted` 에는 안내하지 않는다 — 덮개(대개 모달·동의 창)를 무시하라는 신호가 되므로. 막지도 않는다(Chrome 과 같음).
- **사람 경로(`fill_as_person`)는 거절하지 않는다**: 재생의 사람 행동 재현은 사람 입력이라 정책·거절 밖이다(AGENTS.md "재생은 MCP 와 같은 경로"의 예외). 클릭 점 계산(`click_point`, in-view centre)은 같은 함수를 쓰고, 거절 판정만 건너뛴다.

**에이전트 계약 구현(2차 리뷰 반영)**:
- `AgentError::NotInteractable { reason: &'static str }` 와 `hint() -> &'static str` 로는 가로챈 요소의 역할·이름을 담을 수 없다. 그래서 `error.rs` 의 모양을 바꾼다: `NotInteractable { id, reason: NotInteractableReason }`. 사유는 열거형 `Hidden | Inert | Clipped(ClipHint) | OutsideViewportX | Intercepted { by: Option<IssuedId>, by_role, by_name } | ZeroSize | NotRendered` 이고, `hint()` 는 `String` 을 돌려준다.
- **가로챈 요소의 이름은 페이지 문자열**이다. trusted 메시지·힌트에는 역할과 (이미 발급된 경우만) `eN` 만 넣는다. 이름은 `text` 모듈로 따옴표 처리해 **page content(untrusted) 쪽**에만 싣는다(관찰의 trusted/untrusted 분리와 같은 규칙 — AGENTS.md "trusted 블록의 `note:` 는 브라우저만 쓴다").
- `eN` 은 이미 발급된 ID 일 때만 쓴다(ID 는 관찰만 발급 — AGENTS.md). 발급되지 않았으면 역할만 쓴다.

**1d 닫힌 드롭다운 링크 클릭**(3차 4b 부수 발견) — enwiki "145 languages" 의 닫힌 목록 링크를 누르면 아무 일도 없다. 그 목록은 `visibility:hidden` + `height:0; overflow:hidden auto` 다. 1a 로 관찰에서 빠지고, 1b-링크로 숨은 링크가 이동하지 않으면 증상이 사라질 것으로 본다. 남으면 원인을 진단한다. 두 값 `overflow` 는 **파서만** 한 키워드다(`properties.rs` 585행) — 클립 표는 이미 축별로 읽는다(`clip.rs` 205–206행). 파서는 5a 몫이다.

**1e `<noscript>`**(3차 1a 후속, 2차 리뷰로 범위가 줄어듦) — 스크립트를 돌리는 문서인데 `<noscript>` 안 `<link>`/`<style>` 이 적용된다(docsrs −1 요소, bbc 원문에도 있음). 바꿀 것:
- HTML 파서의 `scripting_enabled` 를 켠다. **`parser_opts()`(`html.rs` 136–145행) 한 곳**이다. 오류·텍스트 문서는 문자열로 만드는 정해진 문서라 영향이 없다.
- html5ever 0.40.1 은 이 플래그를 `<noscript>` 에만 쓴다(`tree_builder/rules.rs` 225·990행). 켜면 `<noscript>` 내용이 요소가 아니라 원시 텍스트가 되어(HTML §13.2.6.4.4) DOM API·추적용 `<img>`·앞으로의 meta refresh 가 모두 Chrome 과 같아진다.
- `html.rs` 122행 주석("이 플래그를 켜면 인라인 `<script>` 본문을 삼킨다")은 **틀렸다**. 그 자리의 `false` 인자는 `context_element_allows_scripting` 이다. 주석을 고친다.
- probe 는 엔진이 파싱한 문서를 걷고 골든은 `html::parse` 를 쓴다. 그래서 따로 맞출 파서 사본은 **없다**(1차 리뷰 A12 의 "사본 둘" 지적은 철회).

### 설계 결정

- **`visibility` 는 레이아웃을 바꾸지 않는다**: 공간과 문서 높이는 그대로다. 페인트·히트·링크·관찰·행동만 바뀐다.
- **가로 스크롤을 만들지 않는다**: 크롬 높이·클릭 좌표·스크린샷 자르기가 묶여 있다(AGENTS.md `CHROME_HEIGHT`). 실사이트의 가로 넘침은 대부분 우리 레이아웃 버그라 3단계가 원인을 고친다.
- **클릭이 닿는 곳은 히트 테스트 하나가 정한다**: 링크 이동(1b-링크)·라벨 활성화·JS 클릭·에이전트 거절이 모두 같은 히트 결과를 본다. 히트는 페인터와 같은 z-order 로 걷는다(1b-z). 사람이 창에서 누르는 것과 에이전트가 누르는 것이 같아진다.

### 테스트 (1단계)

- 1a: `visibility_hidden_is_not_painted_hit_linked_or_observed`(이미지·밑줄 포함, 자손 `visible` 은 남음), `visibility_hidden_keeps_layout_space_and_document_height`, `hidden_link_does_not_navigate`, `aria_labelledby_reads_hidden_text`, `hidden_element_is_skipped_by_tab_and_refuses_type`, `inert_subtree_is_not_observed_hit_or_navigated`, `opacity_zero_button_stays_observed_and_clickable`, `aria_hidden_is_ignored_by_the_observation`(N4 — 4차 동작 고정).
- 1b: `pointer_events_none_link_does_not_navigate`, `pointer_events_auto_child_of_none_receives_the_click`, `overlay_blocks_link_navigation_for_people_and_agents`(1b-링크), `link_navigation_follows_the_hit_node_ancestor_href`, `hit_test_follows_paint_order_for_z_index`(1b-z — 문서 순서상 앞이지만 z-index 가 높은 상자), `intercepted_click_is_refused_naming_the_interceptor_outside_the_trusted_block`, `label_ancestor_of_the_hit_activates_its_checkbox`.
- 1c: `click_right_of_the_viewport_succeeds_today`(먼저 — 오늘 동작 고정, 1c 커밋에서 다음 둘로 바뀜), `click_point_is_clamped_into_the_visible_part`, `click_with_no_horizontal_intersection_is_refused_with_a_next_action`, `carousel_slide_is_refused_as_clipped_with_a_carousel_hint`, `fill_as_person_clicks_without_refusal`.
- 1e: `noscript_is_raw_text_when_scripting_is_enabled`, `inline_script_text_survives_scripting_enabled`(주석이 틀렸음을 확인).
- 퍼저: `visibility`·`pointer-events`·`z-index` 값 풀(별도 난수 흐름) — 히트 테스트와 페인트 순서가 같은지 단정.
- 골든 `compat3`·`compat4` 갱신.

### 측정·완료 기준 (1단계)

| 기준 | 판정 |
|---|---|
| `obs_u_hidden` | **0**(정의상 — `visibility` 숨김과 `inert` 가 관찰에서 빠짐) |
| `obs_u_intercepted`(ours) | N2 목표 창의 값(출발점 −60%), 원인 셋 모두 |
| 화면 밖 클릭 | 성공도 헛클릭도 없음 — 전부 당긴 점으로 누르거나 거절 |
| 에이전트 영향 기준(공통) | 두 라벨에 다 있는데 새로 닿지 않게 된 요소 0. 사라진 요소마다 "Chrome 에서도 안 보임" 확인(enwiki 닫힌 언어 목록 145 는 정상) |
| **계약 갱신** | ① concept.md: `not_interactable` 설명(지금 "rect 없음·크기 0"만)에 새 사유와 다음 행동 힌트, "링크도 히트를 따른다", D5·N3·N4 결정 기록(3차 3a 처럼 결정 기록 절). ② `mb-mcp/src/prompts.rs` 의 tool 설명·instructions. ③ `cargo test -p mb-mcp --test replay`. ④ eval 재실행은 **사용자가 자기 터미널에서**(`scripts/eval-run.sh` → `claude -p`): 기준은 concept.md 847행의 시나리오 6개 × 14회 결과(14/14)다. `eval/scenarios` 에는 8개가 있고 `compat3-menu`·`preact-todo` 는 그 기준 밖이다. 관찰 `result.elements` 가 바뀌어 옛 세션 로그가 회귀처럼 보일 수 있으니, 차이마다 "계약 변경 몫"으로 판정해 메모 |

### 위험 (1단계)

- **JS 가 여는 메뉴가 관찰에서 사라짐**: 지금은 "열린 척" 보이던 메뉴가 닫힌 채로 보인다. 그것이 맞는 관찰이지만, 2단계 전에는 그 스크립트가 죽어 열리지 않을 수 있다. 기대 컨트롤이 일시적으로 줄면 메모하고 2단계에서 다시 본다.
- **hover 전용 메뉴**(D5 위험): P1 표본의 `:hover` 몫으로 크기를 본다.
- **링크가 히트를 따르며 바뀌는 사람 경험**: 관전 창에서 사람이 누르는 링크도 같은 규칙을 따른다. Chrome 과 같아지는 변화지만, 오늘 덮개 아래로 눌리던 링크가 안 눌리게 된다. 코퍼스 요소 diff 와 별도로 `r4-vis` 에서 "1b-링크 때문에 이동이 막힌 관찰 링크 수"를 센다.
- **거짓 거절**(히트가 우리 레이아웃 갭 때문에 엉뚱한 덮개를 고름) — **임계값(사용자 확인 필요)**: `r4-vis` 에서 1b 가 **새로** 거절하는 관찰 요소가 (a) 코퍼스 전체 관찰 요소의 **1%** 를 넘거나, (b) 한 사이트에서 **20개**를 넘으면, 그 사이트들의 표본을 미리보기 PNG 로 본다. 거짓이 절반 이상이면 1b 의 거절을 **가로챈 요소가 positioned(`fixed`·`absolute`·`sticky`) 덮개인 경우로 좁힌다**(나머지는 오늘처럼 히트 결과대로 진행). 문서를 먼저 고치고 진행 표에 적는다.

### 노력 (1단계)

M (3.5~4일): 1a 1일, 1b 1.5~2일(1b-링크 0.5, 1b-z 0.5~1, 거절·`error.rs` 0.5), 1c 0.5일, 1d·1e 0.25일, 계약 문서·replay 0.25일.

---

## 2단계 — 작은 JS API 표면 (클래식 스크립트)

### 목표

사이트의 스크립트 에러를 앞에서부터 없애, 스크립트가 원래 그리던 컨트롤을 그리게 한다. 모듈 로더는 하지 않는다(D3).

### 세부 항목

근거는 [부록 B](#부록-b--js-에러-사이트별). 묶음은 손댈 파일 기준이다. "지금"은 리뷰가 코드로 확인한 상태다.

**2a — 조회·조작**

| API | 사이트(`final`) | 지금 | 설계 |
|---|---|---|---|
| `getElementsByTagName`·`getElementsByClassName`·`getElementsByName` — `document` 와 **`Element` 둘 다** | 첫 에러 5곳(naver·hani·kakaocorp·bbc·microsoft) + 뒤쪽 줄에만 3곳(google·korea-kr·yna) = 8곳 | `getElementsByTagName` 없음. `getElementsByClassName` 은 `document` 판만 있고(`document.rs` 171행) **정적 배열**이다(아래 위험이 이미 있음). `Element` 판 `getElementsBy*` 는 전부 없다 — jQuery 는 요소 판 `getElementsByTagName` 을 쓴다 | **라이브 컬렉션**: `revision` 으로 무효화하는 Proxy(`length`·색인·`item()`·`namedItem()`). 정적 배열이면 `while (c.length) c[0].remove()` 가 끝나지 않는다. HTML 이름은 ASCII 대소문자 무시, `*` 지원, `getElementsByName` 은 NodeList. 스크립트는 파싱 뒤에 돌므로 `getElementsByTagName('script')` 의 마지막 항목은 문서의 마지막 스크립트다(`currentScript` 를 흉내 내는 옛 코드가 다르게 동작 — 메모) |
| `document.implementation.createHTMLDocument` | korea-kr(jQuery 3.7.1 이 `createElement` of undefined — **추정**) | 없음 | 떼어 낸 문서 하나(`DOMParser` 와 같은 길이 있으면 그것). jQuery 가 기능 탐지에 쓰는 범위만 |
| `Element.prototype.remove()` | (테스트 `c[0].remove()` 와 많은 라이브러리) | **없음** — `element.rs` 에는 `appendChild`·`removeChild`·`replaceChild`·`insertBefore` 만 | 트리 변경 갈고리를 지나는 `remove()`(부모 없으면 아무것도 안 함) |
| `insertAdjacentHTML/Element/Text`, `append/prepend/before/after/replaceWith` | example.com | 없음 | 3차 R3 트리 변경 갈고리(깊이·고리 검사)를 지난다. `insertAdjacentHTML` 은 `parse_fragment_at`, 넣은 `<script>` 는 실행 안 함, 부모가 없거나 문서면 `NoModificationAllowedError` |
| `createTreeWalker`·`createNodeIterator` | daum(뒤쪽 줄) | 없음 | 신규 `js/traversal.rs`. **라이브 순회**(호출마다 현재 트리), `FILTER_REJECT` 는 서브트리를 건너뛰고 `FILTER_SKIP` 은 노드만, `whatToShow` 는 u32(`SHOW_ALL` = `0xFFFFFFFF`) |
| `createEvent`·`initEvent` | kakaocorp(Nuxt) | 없음 | `js/event.rs` |
| `CustomEvent` | — | **이미 있음**(`dom_globals.rs` 195–202행) | 재사용 |
| `dispatchEvent` | 콘솔에 안 보임 | **조용한 스텁**(true 만 돌려줌, `dom_globals.rs` 82행) — P4 에 안 잡힘 | 엔진 이벤트와 같은 디스패치 경로로 실제 전달 |
| 캡처 단계 | — | **없음**(`js/event.rs` 버블만) | 리스너 등록부에 `capture` 플래그(`removeEventListener` 도 플래그로 짝짓기), 캡처 → 대상 → 버블. 등록부 구조를 바꾸는 하위 부분이라 따로 커밋(2a-cap) |
| `document.currentScript` | bbc(turbopack 런타임 13회 — **추정**) | 없음 | 스크립트 실행 직전 설정, 끝나면 `null` |
| `getClientRects` | (3a 테스트가 필요로 함, 라이브러리의 보이는지 검사) | 없음(`getBoundingClientRect` 만 있음) | 상자마다 사각형 하나씩 — 1프레임 늦은 rect 맵(spa-roadmap #6)을 그대로 쓴다. `display: contents`·`none` 이면 빈 목록 |

**2b — 플랫폼 객체**

| API | 사이트 | 설계 |
|---|---|---|
| `matchMedia` | velog(첫)·docsrs·apple(뒤쪽 줄) | `css::media::evaluate` 재사용. 무효 쿼리 → `media` 가 `"not all"`. `change` 는 렌더링 단계에서 **결과가 뒤집힐 때만**. `addEventListener('change')` + 옛 `addListener` |
| `document.cookie` | enwiki·kowiki·korea-kr(추정 — page-bug 모양) | D8 b(리뷰 반영판) |
| `URL`·`URLSearchParams` | d2 | `url::quirks`(이미 쓰는 `url` crate) — `href` 와 `searchParams` 가 양방향 |
| `TextEncoder`·`TextDecoder` | velog | UTF-8 + 표준 라벨 별칭, 모르는 라벨은 `RangeError`, BOM 제거 |
| `structuredClone` | google | 원시값·배열·평범한 객체·`Date`·`RegExp`·`Map`/`Set`·`Error`·typed array·`ArrayBuffer`, 순환 참조, 프로토타입은 버림, 함수·DOM 노드는 `DataCloneError`. 부트스트랩에서 붙잡은 내장만 쓴다(3차 K3/K4) |
| `screen` | yna | `width/height/availWidth/availHeight/colorDepth` + `screen.orientation.type`(`landscape-primary`) — 뷰포트에서 만든 상수 |
| `IntersectionObserver` | yna(지연 로딩 라이브러리 — **표에 없던 것**, 리뷰 반영) | spa-roadmap Tier 3 #11 의 최소판: 관찰 대상의 레이아웃 rect 와 뷰포트(스크롤 반영)의 교차를 프레임마다 계산해 바뀔 때 콜백(`isIntersecting`·`intersectionRatio`·`boundingClientRect`). `root`·`rootMargin` 은 뷰포트·0 만, `threshold` 는 배열 받기 |
| `WebSocket` | daum(뒤쪽 줄 `this.ws.removeEventListener`) | **심이 아예 없다**(grep 0). daum 의 `this.ws` 는 페이지가 만든 객체다. 새 스텁: 생성은 되지만 연결하지 않는다. 다음 태스크에 `error` → `close`(code 1006), `readyState` CLOSED, EventTarget 메서드 + `on*` 속성. 결정성: 이벤트는 엔진 타이머 큐 |
| 상대 URL `fetch`·XHR | 3차 K7(코드 확인) | 문서 URL 기준 해석. 하위 요청이라 게이트 대상 아님 |

**2c — `customElements`**(daum·yna)
- **API**: `define`·`get`·`whenDefined`·`upgrade`.
- **반응 시점(명세)**: `[CEReactions]` API 가 끝날 때 반응을 실행한다. 우리 쪽 구현은 Rust 트리 변경 갈고리(`append_child` 등)가 반응 큐에 넣고, JS 래퍼 메서드가 돌아가기 직전에 비운다.
- **정의와 생성**: `define()` 은 돌아가기 **전에** 기존 요소를 문서 순서로 업그레이드한다. `createElement` 는 생성자를 동기로 부른다.
- **어려운 곳**: 생성 가능한 `HTMLElement`. `super()` 가 `new.target` 으로 정의를 찾게 하고, 이미 있는 래퍼는 `Object.setPrototypeOf` 로 업그레이드한다(3차 "JS 래퍼는 노드마다 하나" 규칙 유지).
- **콜백 순서**: 업그레이드 때 이미 있는 관찰 속성의 `attributeChangedCallback` → `connectedCallback`. 이후 `disconnectedCallback`·`attributeChangedCallback`(`observedAttributes`).
- **생성자가 던지면**: 그 요소만 실패 상태(`:defined` 거짓)로 두고 `console.error`. 문서는 계속.
- **Shadow DOM 은 범위 밖**: `attachShadow` 를 "요소 자신을 돌려주기"로 근사하면 `shadowRoot.innerHTML =` 이 light DOM 을 덮어써서 페이지를 망가뜨린다. 그래서 정의하지 않는다(부르면 페이지 에러로 보임). 사이트가 필요로 하면 진행 방식의 "표 밖 API" 로 적는다.

**진행 방식 — 개수로 묶은 양파 벗기기**(리뷰 반영):
- **한 겹** = 표의 API 몇 개를 넣고 코퍼스를 한 번 돌리는 것.
- **겹을 세는 법**(2차 리뷰 반영): 어떤 사이트에 겹이 하나 쌓이는 것은 **그 사이트를 겨냥한 API(그 사이트의 `js_first_error`·`js_any_missing_api` 에 있던 것)가 들어간 실행**일 때뿐이다. 다른 사이트를 위해 넣은 API 의 덤으로 바뀐 사이트는 겹을 쓰지 않는다.
- **사이트당 최대 3겹.** 사이트는 다음 중 하나면 멈춘다: 첫 에러가 page-bug 가 됨 / 첫 에러가 범위 밖(모듈·Shadow DOM 등) / 3겹을 다 씀.
- **표 밖 API 는 4차 전체 6개까지**. 넘으면 사용자 결정이다(이 문서에 이름·사이트·이유를 적고).
- 겹마다 진행 표 아래 **양파 기록**([표](#2단계-양파-기록))에 한 줄을 적는다: 라벨·사이트·`js_first_error` 전 → 후·더한 API·`js_scripts_completed/total`·④ 판정 근거(수동 판정일 때만 — N2 "④ 판정 규칙").

### 설계 결정

- **심은 페이지가 바꿀 수 있는 것을 호출 때 읽지 않는다**(3차 K3/K4): 새 JS 심은 필요한 내장을 부트스트랩 때 붙잡는다. 폴리필이 전역을 바꾸는 사이트(daum·hani·bbc)가 코퍼스에 있다.
- **트리 변경은 한 길로**: 새 조작 API 와 커스텀 요소 반응은 전부 3차 R3 의 `Document` 변경 메서드를 지난다.
- **API 마다 의미 있는 증상 테스트**: 빈 스텁(예: `dispatchEvent` 처럼 true 만 돌려줌)은 ④ 숫자를 줄이지만 동작은 없다. 그래서 각 API 는 "그 API 가 있으면 페이지가 하는 일"을 확인하는 테스트로 들어간다.
- **에러 줄 규칙 유지**: 새 콜백 경로의 예외는 `__mb_report_error` → `error_location` 한 길로.

### 테스트 (2단계)

- 2a: `jquery_1_9_1_initialises_and_selects`·`jquery_1_12_4_initialises_and_selects`(MIT 원본 픽스처), `live_collection_loop_removing_first_item_terminates`(`getElementsByClassName` 포함), `get_elements_by_tag_name_is_ascii_case_insensitive_and_supports_star`, `insert_adjacent_html_respects_the_depth_cap_and_does_not_run_scripts`, `tree_walker_reject_skips_the_subtree_and_skip_only_the_node`, `dispatch_event_reaches_listeners`, `capture_listeners_run_before_target_and_bubble`, `current_script_is_set_while_a_script_runs`.
- 2b: `match_media_uses_the_same_evaluator_as_the_cascade`, `match_media_change_fires_only_when_the_result_flips`, `document_cookie_follows_domain_path_and_order_rules`, `document_cookie_ignores_httponly_from_script`, `url_search_params_write_back_to_href`, `text_decoder_rejects_unknown_labels`, `structured_clone_survives_a_page_replacing_map`, `intersection_observer_reports_an_element_scrolled_into_view`, `websocket_stub_errors_then_closes`, `relative_fetch_resolves_against_the_document`.
- 2c: `custom_element_defined_after_parse_is_upgraded_before_define_returns`, `upgrade_fires_attribute_changed_then_connected`, `create_element_runs_the_constructor_synchronously`, `custom_element_constructor_throwing_does_not_stop_the_page`.
- `mb-runtime/tests/dom_depth.rs` 에 새 조작 API 모양, eval `compat4-menu`·대응 테스트 통과로 전환([부록 E](#부록-e--골든eval-단계별-기대표)).

### 측정·완료 기준 (2단계)

| 기준 | 판정 |
|---|---|
| ④(N2) | 첫 에러 missing-api ≤3 · 어떤 줄이든 ≤6(기준선은 수동 판정표, 새 줄은 P4 자동 규칙 — N2 "④ 판정 규칙", ①과 함께) |
| ③(N2) | `js_scripts_completed/total` 상승, `uncaught_dedup` 은 참고(늘 수 있음 — 양파 기록에 이유) |
| ① 기대 컨트롤 누락 | ≤3(naver 는 남을 것으로 예상) |
| 견고성 | 25곳 timeout·panic 0, `[mb-budget]` 0 |
| 보충 ② | 2단계 끝 한 번(N6 규칙). "후"는 `r4-js-sup`, 기준은 `r4-base-sup`(옛 엔진을 새 캐시로). 보충 전 값도 같이 적는다 |
| 에이전트 영향 기준(공통) | 새로 닿지 않게 된 요소 0. 새 요소는 대부분 "JS 가 그린 컨트롤"(정상), 사라진 것은 사이트마다 판정 |

### 위험 (2단계)

- **양파가 깊음**: daum 처럼 화면 전체를 앱이 그리는 곳. 3겹 규칙으로 끊고 남은 첫 에러를 기록한다.
- **스크립트가 더 멀리 가서 생기는 새 실패**: 무한 마이크로태스크(3차 K4)·큰 DOM·광고 SDK 의 이동 시도. 이동은 정책 게이트를 지나고, 시간은 `[mb-budget]`·settle 벽시계가 잡는다. 새 무한 사슬이 나오면 원인을 고치고 상한으로 덮지 않는다.
- **오프라인 재생의 한계**: 보충 ② 전의 "후"는 낮게 나온다. 전·후를 둘 다 적는다.
- **노력**: 리뷰는 5일이 낙관적이라고 보았다(WebSocket 스텁·캡처 단계·`customElements` 반응·`document.implementation`). 7일 상당으로 잡았고, 끝은 개수 규칙이 정한다.

### 노력 (2단계)

L (7일 상당): 2a 2.5일(캡처 단계·`remove()`·요소 판 `getElementsBy*`·`getClientRects` 포함), 2b 2일, 2c 2일, 측정·보충·문서 0.5일.

---

## 3단계 — 레이아웃

### 목표

에이전트가 보는 요소가 Chrome 과 같은 자리(특히 **뷰포트 안**)에 놓이게 한다. 지표는 `obs_u_offscreen_x`(ours)와 "가장 넓은 상자 > 뷰포트" 사이트 수(지금 8곳)다.

### 세부 항목

| # | 항목 | 측정(`final`) | 에이전트 의미 |
|---|---|---|---|
| 3a | `display: contents` | 원문 31·7호스트. mdn 본문 열이 240px(3차 5b) | mdn 본문이 좁은 열에 눌림, 링크 위치가 틀림 |
| 3b | 블록을 품은 인라인 폭 | bbc 300px 열 안 인라인 `<a>` 가 973px → 카드 일부가 x 1,264–1,564 | 카드가 화면 밖(오늘은 보이지 않는 클릭이 성공, 1c 뒤엔 거절) |
| 3c | 뷰포트보다 넓은 상자 | 8곳: microsoft 5,117 · google 3,350 · apple 2,947 · kowiki 2,528 · enwiki 2,338 · naver 1,920 · velog 1,728 · korea-kr 1,300 | 원인별로 묶어 고친다. **진단은 3단계 P 라벨 `r4-layout-p`(2단계 엔진·보충 ② 뒤 캐시)로**(리뷰 반영 — JS 가 DOM 을 바꾼 뒤의 모양). 상자 경로·`white-space`·폭 출처 표가 그 P 의 probe 변경이다 |
| 3d | 표 min-content 바닥 | enwiki navbox 그룹 셀 0폭 ~56, 관찰 −11 요소 | 그룹 이름 링크가 빠짐 |
| 3e | shrink-to-fit 측정의 `%` 폭 | kakaocorp 문서 높이 2,102 → 4,244(3차 3d) | 낮음. 3차 3d 선택지 (a) |

### 설계 결정

- **`display: contents` 를 펼치는 자리(리뷰 반영)**: taffy 쪽만이 아니다. 레거시 경로들이 `children` 을 직접 돈다:
  - `flex.rs` 34–62행, `grid.rs` 39–82행, `table.rs` 67–98행, `block.rs` 8곳;
  - 인라인 묶기(`group_block_children`), taffy `native_child_slots`/`build_taffy_node`/`walk`.
  펼침은 **공용 `flattened_children()` 하나**로 하고, 모든 경로가 그 함수만 쓴다(직접 `children` 순회를 grep 으로 막는 테스트 하나).
- **`contents` 요소 자신의 흔적**: 상자가 없어도 링크·호버·JS 는 그 요소를 알아야 한다.
  - `<a style="display:contents">` 의 `href` 를 자식이 물려받아야 한다(`collect_link_targets_into` 가 레이아웃 상자 조상으로 href 를 찾으므로, 펼친 자식에 "`contents` 조상" 목록을 단다).
  - `:hover` 경로(`walk_for_hover` 의 경로가 DOM 경로가 되도록)를 맞춘다.
  - JS rect: `getBoundingClientRect` 는 0, `getClientRects` 는 빈 목록 — 인덱스 rect 와 따로.
- **명세의 예외**(CSS Display 부록 B): 대체 요소·폼 컨트롤에서는 `none` 처럼, 루트에서는 `block` 처럼 동작한다. 텍스트 자식은 익명 flex/grid 항목이 된다(우리는 인라인 런을 항목 하나로 둔다).
- **에이전트 인덱스**: 역할이 있는 `contents` 요소(버튼 등)는 관찰에서 안 사라지게 자식 rect 의 합집합을 rect 로 두되, **클릭 점은 넓이가 있는 첫 자식 상자**(합집합 중심이 아님 — 빈 사이를 누를 수 있으므로).
- **3c 는 원인별로**: P 표에서 같은 모양끼리 묶어 하나씩 고친다. 모양 다섯을 넘으면 남은 것은 5차로.
- **3d·3e 는 퍼저와 같이**: 크기 탐색의 `%`·min-content 를 건드려 3차 F1b·5f 류 증폭을 다시 낼 수 있다. 고칠 때마다 퍼저 레이아웃 긴 실행 3만을 돌린다.

### 테스트 (3단계)

`display_contents_children_become_grid_items`(레거시·taffy 둘 다), `display_contents_link_children_still_navigate`, `display_contents_on_a_button_acts_as_none`(폼 컨트롤 예외), `display_contents_element_has_empty_client_rects`, `display_contents_button_click_point_is_its_first_child_box`, `layout_code_iterates_children_only_through_flattened_children`, `inline_wrapping_a_block_takes_the_container_width`, 3c 모양마다 하나, `table_column_has_a_min_content_floor`, `shrink_to_fit_reads_percent_child_width_as_content_width`. 골든 `compat4` 갱신, 퍼저에 `display: contents` 값 풀.

### 측정·완료 기준 (3단계)

| 기준 | 판정 |
|---|---|
| `obs_u_offscreen_x`(ours) | 3단계 "전" = `r4-layout-p`(`r4-js-sup` 뒤 3단계 P). 목표는 N2 목표 창의 값(출발점: `r4-base` 대비 관찰 요소당 비율 −80% — ②의 판정은 `r4-final`, 이 행은 3단계가 그 몫을 맡았는지 보는 참고) |
| 가장 넓은 상자 > 뷰포트 | 8곳 → ≤3 |
| mdn 본문 열 | Chrome 과 같은 3열(미리보기 PNG) |
| enwiki navbox 그룹 셀 | 0폭 0개, 요소 +11 |
| 에이전트 영향 기준(공통) | 새로 닿지 않게 된 요소 0 |
| 견고성 | 퍼저 긴 실행 무결, `[mb-budget]` 0, enwiki·github·yna 스타일+레이아웃 시간이 `r4-layout-p` ×1.2 안 |

### 위험 (3단계)

- **넓은 상자 원인이 하나가 아님**: 모양 다섯 규칙으로 끊는다.
- **`display: contents` 와 경계 잎**: `is_native_block` 이 자식 목록을 보므로 펼친 목록과 어긋나면 브릿지가 틀린 노드를 짝짓는다. 공용 함수 하나로 막는다.

### 노력 (3단계)

M (5일): 3a 2일(리뷰 반영 — 레거시 경로·링크·호버·rect), 3b 0.5일, 3c 진단 0.5일 + 고침 1.5일, 3d·3e 0.5일.

---

## 4단계 — 셀렉터

### 목표

남은 의사 클래스 중 **작고 확실한 것**을 맞게 매칭하거나 "유효·매칭 없음"으로 받아 규칙을 살린다. `:has()` 는 뺀다(D7).

### 세부 항목과 설계

| 의사 클래스 | 규칙(`final`) | 매칭(리뷰 반영) | 에이전트 의미 |
|---|---:|---|---|
| `:lang()` | 1,228(apple 1,208 · enwiki 20) | 언어 = 가장 가까운 조상의 `xml:lang`(이긴다) 또는 `lang`(`lang=""` 은 "모름"으로 상속을 끊음). 없으면 `<meta http-equiv=content-language>`(값이 하나일 때만), 그다음 HTTP `Content-Language`(하나일 때만). 비교는 대소문자 무시, 하위 태그 경계 일치(`en` 은 `en-US` 에 맞고 `eng` 에는 안 맞음), 쉼표 목록 | **거의 없음**: 원문의 `:lang()` 인자 1,648개 중 zh 570 · ja 234 · ko 170 · ar 136 · th 122 … 인데 페이지는 `en-US` |
| `:target` | 88 | URL 프래그먼트의 `id`(없으면 `<a name>`) 요소. 같은 문서 해시 이동마다 `Document::touch()` + 뷰 캐시 키. **`pushState` 로는 안 바뀐다**(명세) | apple 지역 내비 메뉴 상태, 각주 강조 — 낮음~중 |
| `:popover-open` | 9(github) | Popover API 가 없으니 항상 거짓(유효). **UA 숨김 규칙 둘을 같은 커밋에서 더한다**: `[popover]:not(:popover-open):not(dialog[open]){display:none}` 와 닫힌 대화상자 `dialog:not([open]){display:none}`. 지금 `style/defaults.rs` 433–441행 `is_hidden_by_ua` 에 둘 다 없어 **닫힌 팝오버·대화상자가 관찰된다** | 닫힌 팝오버·`<dialog>` 내용이 관찰에서 빠짐 — 에이전트에 보이는 고침 |
| `:indeterminate` | 8 | 체크박스는 IDL `indeterminate` 가 생기기 전까지 거짓, 라디오 그룹 전체 미선택·값 없는 `<progress>` 는 참 | 낮음 |
| `:placeholder-shown` | 4 | `placeholder` 가 있고 **현재 값**이 빈 `input`/`textarea`. 입력마다 `touch()` | 낮음 |
| `:read-only`/`:read-write` | 5 | read-write = 편집 가능한 텍스트 컨트롤(체크박스·라디오·`select`·버튼 제외, `readonly`·`disabled` 아님)·`contenteditable` | 낮음 |
| `:dir()` | 5 | `dir` 조상 사슬, `dir=auto`(첫 강한 방향 글자)·`<bdi>` 처리, 없으면 ltr | 낮음 |
| `:defined` | 2(github) | 2c 의 정의 상태. 일반 요소는 참. 커스텀 요소의 정책은 **N5** | N5 |
| 나머지(`:invalid` 4 · `:host` 3 · `:-webkit-autofill` 2 · `:modal` 1 · `:window-inactive` 1 …) | ≤4씩 | `:-webkit-autofill`·`:modal`·`:window-inactive` 는 항상 거짓(유효), `:invalid` 는 `required` + 빈 값만 | 무시할 만함 |

- **`:target`·`:placeholder-shown` 은 상태 의사 클래스**라 3차 4b 처럼 리비전·뷰 캐시 키를 지킨다(AGENTS.md 해당 절에 한 줄).
- **4단계 P**(라벨 `r4-sel-p`): `:not(:defined)` 와 숨김(`visibility:hidden`/`display:none`/`opacity:0`)을 함께 쓰는 규칙 수, 그리고 그 규칙에 맞을 커스텀 요소 수를 사이트별로 센다. 이 측정 뒤 N5 를 사용자가 고르고, 그 전에는 `:defined` 를 구현하지 않는다.

### 테스트 (4단계)

`lang_matches_by_subtag_boundary_through_ancestors`, `empty_lang_breaks_inheritance`, `xml_lang_beats_lang`, `lang_falls_back_to_content_language_meta`, `target_follows_fragment_navigation_but_not_push_state`, `closed_popover_is_hidden_by_the_ua_rule`, `closed_dialog_is_hidden_by_the_ua_rule`, `placeholder_shown_tracks_the_current_value`, `read_write_excludes_checkboxes`, `dir_auto_uses_the_first_strong_character`, `defined_is_false_until_custom_element_is_defined`(N5 에 따라).

### 측정·완료 기준 (4단계)

- 버린 규칙 ours: **참고로만 적는다**(N2 — 거의 다 `:lang()`).
- 판정은 셋이다:
  - 닫힌 팝오버가 관찰에서 빠짐(github);
  - `:target`·`:defined` 몫의 요소 diff 판정;
  - 에이전트 영향 기준(공통).
- 살아난 규칙 안 선언이 버린 선언을 늘리는 것은 정상이다(5단계 "전"에 들어감).

### 위험 (4단계)

- `:lang()` 조상 걷기 비용 — 규칙 색인(3차 F4)이 후보를 거르므로 적다. GitHub·apple 스타일 시간이 ×1.1 안인지 본다.
- N5 가: 모듈 문서의 커스텀 요소가 숨는다(P 로 크기를 먼저 잰다).

### 노력 (4단계)

S (2일).

---

## 5단계 — CSS 값

### 목표

남은 버린 선언 중 **배치를 바꾸는 것**(5a)을 먼저, 그다음 색(5b), 마지막으로 그림·파싱만(5c). 보조 지표는 합산 ours −50% 다(`r4-base-sup` → `r4-final`, N2).

### 세부 항목 (`final`, 합산 ours — [부록 A](#부록-a--후보-목록과-측정))

**5a — 배치 값**(약 200):
- **두 값 `gap`**(93·7곳) → `row-gap`/`column-gap`, `grid-gap` 별칭 포함.
- **두 값 `overflow`**(26, github 22) → `overflow-x`/`-y`. 순서는 x 다음 y. 파서만 고친다 — 클립 표는 이미 축별이다. 명세(CSS Overflow 3 §3.1): `hidden`·`auto` 는 둘 다 자르고, 한 축이 `visible`/`clip` 이 아니면 다른 축의 `visible` 은 `auto`, `clip` 은 `hidden` 으로 계산된다.
- **`grid-area`·`grid-row`·`grid-column` 슬래시**(github 22+9+4, apple 2):
  - 빠진 끝은 시작이 이름이면 그 이름을 복사하고, 아니면 `auto`.
  - 시작 > 끝이면 바꾼다.
  - `grid-column: span 0`(apple 14)은 명세상 무효라, 분류표의 browser-too 로 옮길지 **분류표를 먼저 고쳐서** 정한다.
- **`aspect-ratio`**(19·8곳) → 고유 비율. `min-height: auto` 면 내용만큼 자랄 수 있고, 비율 0 은 `auto`.
- **`flex-flow`**(15·6곳) → `flex-direction`/`flex-wrap`. wrap 을 잃으면 항목이 한 줄로 넘쳐 화면 밖으로 간다.
- **`grid-template-rows: subgrid`**(6, github) — 받기만 할지는 사이트를 보고 정한다.

**5b — 색**(약 190):
- **현대 문법**: `rgb()`/`rgba()` 는 같은 함수다. 공백 구분 + `/` 알파, `%` 알파, `none` 키워드, `hsl()` 의 각도 단위(`deg`·`rad`·`grad`·`turn`). 쉼표 문법은 숫자와 `%` 를 섞지 못한다. 쉼표 문법의 `.8` 알파 재파싱도 다룬다(IACVT 쪽 53).
- **`oklch()`·`color-mix()` 는 여전히 버린다**(메모).
- **시스템 색**: CSS Color 4 의 전체 목록 + 옛 별칭(`ButtonFace`·`HighlightText` …). 키워드 폴백 ~54 + 파서 버림 `border-color: canvastext` 26.
  - 값은 **Chrome 라이트 테마에서 잰 값**이다. 사람이 손으로 쓴 로컬 페이지를 Chrome 에 띄워 `getComputedStyle` 로 잰다 — 사이트 방문이 아니다.
  - 결정성 때문에 고정한다. `color-scheme: dark` 를 선언한 페이지에서 Chrome 은 다른 값을 쓴다(우리는 라이트 고정 — 메모).

**5c — 그림·파싱만**(약 400):
- **`outline` 단축**(112) → `outline-width/-style/-color` 파싱. 그리기는 포커스 링에 한해 선택(스크린샷에 포커스가 보이면 좋음). 그리지 않으면 "파싱만"으로 옮겨 간다.
- **그라디언트 배경**(`background` 96·11곳 다수) — `radial-gradient` 의 위치·크기·`at`. 여러 층 `background` 에서 색은 **마지막 층에서만** 유효하다.
- **`filter`·`backdrop-filter`**(+`-webkit-`) → 파싱만. 메모: `filter` 는 고정 위치 자손의 포함 블록을 만드는데, 파싱만이라 그 효과도 없다.
- **`content` 문법**(148 — `"\f8ff"/""` 대체 텍스트, `attr()`, `counter()`) → 파싱 + **접근 가능한 이름에 넣기**(리뷰 반영, accname §2F). `::before`/`::after` 의 글은 이름에 들어가고, `/` 뒤 대체 텍스트가 있으면 그것이 대신한다. 사용 영역(PUA) 글리프만인 글은 뺀다. 그리기는 5차(D7).
  - **관찰 이름이 바뀌는 변경**이다. 아이콘 버튼 이름이 생긴다 — 골든과 에이전트 영향 기준으로 판정한다.
- **위치 값 3–4개**(`background-position` 29 · `object-position` 28 · `transform-origin` 28).
- **`transform` 조합 문법**(27 + `-webkit-` 14).
- **`clip-path: polygon()`**(18)·**`mask*`**(48) → 파싱만.

### 설계 결정

- **5c 는 우선순위 순서와 개수로 끝낸다**(2차 리뷰 반영 — "남은 시간 안에서" 대신). 5c 의 대부분은 버림 지표만 줄이므로, 아래 순서로 하나씩(항목 하나 = 하위 커밋 하나) 하고 **위 넷은 꼭**, 나머지는 **최대 셋 더** 한다. 그 밖은 5차로 넘기고 진행 표에 적는다.
  1. `content` 파싱 + 접근 가능한 이름(에이전트가 보는 이름이 바뀜);
  2. `outline` 단축(포커스 링 — 스크린샷);
  3. 그라디언트 배경과 마지막 층 색(배경이 사라져 글자가 안 보이는 경우);
  4. 위치 값 3–4개(`background-position`·`object-position`·`transform-origin`);
  5. `transform` 조합 문법;
  6. `filter`·`backdrop-filter` 파싱만;
  7. `clip-path`·`mask*` 파싱만.
  항목마다 "그리기/파싱만/이름에 반영"을 진행 표에 적는다. `@supports` 가 파싱만인 값을 참으로 보지 않게 `UNRENDERED` 를 같이 고친다.
- 새 단축 파서는 `css::longhands_of` 에 넣고, 길이는 `parse_length_or_number` 로 읽는다.

### 테스트 (5단계)

- 5a: `two_value_gap_sets_row_and_column`, `two_value_overflow_hidden_auto_clips_both_axes`(이름을 고침 — 리뷰 반영), `overflow_visible_on_one_axis_computes_to_auto`, `grid_area_slash_places_by_line_numbers_and_swaps_reversed`, `aspect_ratio_gives_a_replaced_box_its_height`, `aspect_ratio_with_min_height_auto_grows_to_content`, `flex_flow_wrap_wraps`.
- 5b: `modern_rgb_with_slash_alpha_and_none`, `comma_rgb_rejects_mixed_number_and_percent`, `system_colors_resolve_to_light_values`.
- 5c: `before_content_enters_the_accessible_name`, `content_alt_text_replaces_the_glyph_in_the_name`, `background_color_only_in_the_final_layer`.
- 퍼저 CSS 토큰 템플릿에 새 문법.

### 측정·완료 기준 (5단계)

| 기준 | 판정 |
|---|---|
| 합산 ours | `r4-base-sup` 대비 **−50%** at `r4-final`(N2) |
| 5a 항목의 버림 | 0(남으면 사유) |
| 에이전트 영향 기준(공통) | 새로 닿지 않게 된 요소 0. 5c 의 이름 변화는 요소마다 판정 |
| 요소 diff | 5a 는 배치가 바뀌므로 문서 높이·가장 넓은 상자 변화를 사이트별로 메모 |

### 위험 (5단계)

- `aspect-ratio`·`flex-wrap` 은 실사이트 배치를 꽤 바꾼다. 3차 5m 처럼 사이트별 원인을 적는다.
- 5c 를 끝까지 못 하면 보조 지표에 못 미칠 수 있다. 보조 지표라 4차 완료를 막지 않는다.

### 노력 (5단계)

M (3~4일).

---

## 4차 전체 노력·위험 요약

- 합계 **약 24~26일 상당**: 0(3.5) + 1(3.5~4) + 2(7) + 3(5) + 4(2) + 5(3~4). 일수는 상대 크기이고(3차 교훈), 끝은 개수 규칙(2단계 3겹·표 밖 6개, 3c 모양 다섯, 5c 우선순위 넷 + 최대 셋, 단계당 끼워 넣기 2개)이 정한다.
- 가장 큰 불확실성은 **2단계 양파 깊이**와 **3c 넓은 상자 원인 수**다.
- 견고성 쪽 새 위험은 거의 전부 2단계에 있다(스크립트가 더 멀리 감). 3차가 만든 그물(정책 게이트·`[mb-budget]`·settle 벽시계·트리 변경 깊이 검사·`catch_unwind`)을 지나는지가 리뷰 체크 항목이다.
- 측정 쪽 새 위험: 1b 의 거짓 거절(우리 레이아웃 갭이 엉뚱한 덮개를 고름 — 임계값은 사용자 확인), ours/browser-too 표를 결과 뒤에 고치고 싶은 유혹(P2b — 고치면 옛 정의 열을 나란히).

---

## 진행 표

4차 기준선은 `r4-base`(0단계 끝)와 `r4-base-sup`(2단계 보충 ② 뒤)다. 0행은 `r4-base` 값이다(2026-11-28). 목표는 [N2](#n2-d4-목표-고쳐-쓰기-d4-를-다시-정함)(사용자 결정 2026-11-28 — D4 를 대체).

| 단계 | 상태 | 커밋 | 테스트 수 | 코퍼스 fail/panic/timeout | 기대 컨트롤 누락 | `obs_unreachable`(ours) 관찰 요소당 비율(절대 수) · 구성 hidden/clipped/offscreen_x/intercepted(참고) | `Uncaught`(중복 제거) / 스크립트 완주 / missing-api 첫·어느 줄 | 합산 ours / 버린 규칙 ours(참고) | 메모 |
|---|---|---|---|---|---|---|---|---|---|
| 3차 끝(`final`) | ✅ | `cef51c5` | 1173 | ok 12 + ok* 8 · nav-fail 1(crates.io) · panic 0 · timeout 0 · blocked 2 · robots skip 2 · `[mb-budget]` 0 | 7/20 (naver·daum·hani·korea-kr·velog·d2·google) | 미측정(클립 거절 276) | 83(중복 포함 96) / 미측정 / 10 · 14 | 1,356 / 1,506 | `r4-check` 재생이 `final` 과 같음([부록 D](#부록-d--측정-재현과-명령)) |
| 0 측정 정비 | ✅ 완료 — ② 목표 창 결정(2026-11-28) · F ✅(`e034ae2`, 각 3만·실패 0) | `4f46de3`(P) · `68498f3`(K5·P2b 세로) · `d263093`(S) · `6e62671`(보충 ①) · `e034ae2`(G) · (T·`r4-base` 커밋 대기) | 1201 | `r4-base`: ok 12 + ok\* 9 · nav-fail **0** · panic 0 · timeout 0 · blocked 2 · robots skip 2 · `[mb-budget]` 0 | **6/20** (naver·daum·hani·korea-kr·velog·google) | **0.1800 (796)** · 359/73/94/270 (첫 P2b 표 0.1836·812) | 83(96) / **179/272 (65.8%)** / 10 · 14 | 1,356(kowiki 제외 1,346) / 1,506 | `r4-p0` → `r4-k5`(빈 diff) → `r4-sup1`(확인) → **`r4-base`**. **목표 창 결정**: hidden 0 · intercepted ours ≤135 · offscreen_x ours ≤28 · clipped 목표 없음(절대 수) |
| 1 에이전트 정합성 | 설계 | | | | | | | | |
| 2 작은 JS | 설계 | | | | | | | | 겹마다 `r4-js-1`… → 보충 ② → `r4-js-sup`(후) / `r4-base-sup`(③④·보조의 기준) |
| 3 레이아웃 | 설계 | | | | | | | | |
| 4 셀렉터 | 설계 | | | | | | | | |
| 5 CSS 값 | 설계 | | | | | | | | |

끼워 넣는 부분이 생기면 행을 더하되 먼저 이 문서에 이름·이유를 적는다(공통 규칙). 미리보기 PNG 는 `compat-cache/previews/<단계>/<사이트>.png`(git-ignored).

### 2단계 양파 기록

| 라벨 | 사이트 | `js_first_error` 전 → 후 | 더한 API(이 사이트를 겨냥했나) | 스크립트 완주 전 → 후 | 겹(최대 3) | ④ 판정 근거(수동일 때만) | 멈춘 이유 |
|---|---|---|---|---|---|---|---|
| (2단계에서 채움) | | | | | | | |

표 밖 API 누계: 0 / 6.

---

## 부록 A — 후보 목록과 측정

3차 문서와 다른 문서가 남긴 4차 후보 전부, 그리고 이번 측정·리뷰에서 새로 나온 것. **수치는 라벨 `final`(= `r4-check`)·20곳**이다.
- "합산 ours" = 파서 버림 ours + 계산값 시점(IACVT·키워드 폴백) ours.
- "원문" = 동결 캐시의 CSS/HTML 원문을 글자 그대로 센 것(적용 여부와 무관 — 근사).
- 처분 = 이 문서의 단계 또는 D7(뺌).

### A.1 에이전트 정합성·견고성

| 후보 | 출처 | 측정 | 에이전트 의미 | 처분 |
|---|---|---|---|---|
| `visibility` 를 아무도 안 읽음 | 이번 측정 | 선언 303·16곳(github 132 · google 37 · apple 35 · yna 22) | 숨은 메뉴·대화상자가 관찰·그림·클릭·링크 이동에 나옴 | 1a(D5) |
| `pointer-events` 를 아무도 안 읽음 | 이번 측정 | 선언 335·14곳(github 151 · apple 49 · kakaocorp 43) | 덮개가 **JS 클릭 대상**을 가로챔(오늘 링크 이동은 기하 판정이라 덮개를 통과), `pointer-events:none` 링크가 이동함 | 1b(+ 1b-링크: 링크도 히트를 따름) |
| 가로 뷰포트 밖 클릭 | 이번 측정 + 리뷰(사실 1) | P2 로 잼. 가장 넓은 상자 > 뷰포트 8곳, bbc 카드 x 1,264–1,564 | **오늘은 성공한다** — 사람·스크린샷이 못 보는 것을 누름 | 1c(in-view centre, N3) + 3단계 |
| 히트 테스트의 z-order | 리뷰 | DFS 문서 순서 마지막 상자(`view.rs` 555행). 페인터는 이미 z-index 로 정렬 | 겹친 요소 중 엉뚱한 것이 히트 | 1b-z(페인터 순서로) |
| 클립에 다 가려 거절되는 관찰 요소 | 3차 3b | 276·10곳(enwiki 146 · hani 48 · bbc 45 · mdn 11 · korea-kr 7 · yna 5 · kakaocorp 4 · d2 4 · github 3 · apple 3) | 거절로 알림(정직). 상당수가 `visibility:hidden` 과 겹침 | P2 범주 `clipped` |
| 닫힌 드롭다운 링크 클릭이 조용히 무시 | 3차 4b | enwiki "145 languages" | 이유를 모름 | 1d |
| `<noscript>` | 3차 1a | docsrs −1, 원문 bbc 1 | no-JS 스타일이 덮임 | 1e(파서 플래그) |
| 닫힌 팝오버가 관찰됨 | 리뷰 | github `[popover]` | 닫힌 메뉴 내용이 보임 | 4(UA 규칙) |
| K5 crates.io `Accept` | 3차 부록 B | nav-fail 1 | 사이트 전체 404 | 0 + 문서 보충(N1-나) |
| kowiki 스킨 CSS 없음 | 3차 3a·3d·5r | **녹화 때 HTTP 429**(엔진 정상) | kowiki 측정이 틀림 | D1 보충 ① |
| d2 기대 컨트롤 철자 | 이번 측정 | `link:helloworld` ≠ "Hello world" | 측정 정의 문제 | D6 |
| probe 슬리버 줄 수 | 3차 5b | bbc +29 거짓 양성 | 측정만 | P3 |
| R7·이미지 타일 캐시 | 3차 부록 C·F3b | 코퍼스 영향 없음 | — | D7 |

### A.2 JS (리뷰 뒤 고침 — 순서는 콘솔 순서)

| 후보 | 출처 | 측정 | 에이전트 의미 | 처분 |
|---|---|---|---|---|
| `getElementsByTagName` | 이번 측정 | 첫 에러 5곳(naver·hani·kakaocorp·bbc·microsoft — jQuery 1.9.1) + 뒤쪽 줄에만 3곳(google·korea-kr·yna) = 8곳 | jQuery·광고 SDK 가 초기화에서 죽음 | 2a(라이브 컬렉션, `document`·`Element` 둘 다) |
| `Element.prototype.remove()`·요소 판 `getElementsBy*`·`getClientRects` | 2차 리뷰(코드 확인) | 콘솔엔 아직 안 보임(앞 에러에 가려짐) | 라이브러리가 흔히 씀 | 2a |
| `document.implementation` | 리뷰 | korea-kr jQuery 3.7.1 `createElement` of undefined(추정) | korea-kr 의 jQuery 전체(검색창 추정) | 2a |
| `document.cookie` | 3차 1r | enwiki·kowiki 첫 에러, korea-kr 첫 에러(추정) — 모두 page-bug 모양 | 위키 `client-nojs` | 2b(D8) |
| `matchMedia` | 이번 측정 | velog 첫, docsrs·apple 뒤쪽 줄 | velog 로그인 버튼(추정)·docsrs 설정 | 2b |
| `customElements` | 3차 K9 | yna 첫, daum 뒤쪽 줄 | daum 화면 전체 | 2c |
| `IntersectionObserver` | 리뷰(표에 없던 것) | yna 뒤쪽 줄(지연 로딩) | 이미지·목록이 안 채워짐 | 2b |
| `createTreeWalker` | 3차 K9 | daum 뒤쪽 줄 | daum | 2a |
| `WebSocket` | 3차 K9 | daum 뒤쪽 줄 — **심 없음**(`this.ws` 는 페이지 객체) | daum | 2b(새 스텁) |
| `URL` | 이번 측정 | d2 첫 | Next 앱 수화 실패 | 2b |
| `TextEncoder` | 이번 측정 | velog 뒤쪽 줄 | velog | 2b |
| `structuredClone` | 이번 측정 | google 첫 | google 일부(Gmail 누락과의 관계 미확인) | 2b |
| `insertAdjacentHTML` | 이번 측정 | example.com 첫 | 없음 | 2a |
| `createEvent` | 이번 측정 | kakaocorp 뒤쪽 줄 | kakaocorp | 2a |
| `dispatchEvent`(스텁)·캡처 단계 | 리뷰 | 콘솔에 안 보임 | 이벤트로 도는 앱이 조용히 멈춤 | 2a |
| `screen` | 이번 측정 | yna 뒤쪽 줄 | 광고 | 2b |
| `document.currentScript` | 이번 측정(추정) | bbc 13줄(같은 줄 반복) | bbc 청크 로더 | 2a |
| 상대 URL `fetch`/XHR | 3차 K7 | 코드 확인 | 데이터 로드 실패 | 2b |
| ES 모듈 | spa-roadmap Tier 2 | `type=module` 문서 5곳(naver·github·mdn·apple·bbc). 본문을 잃는 것으로 확인된 곳은 naver. `nomodule` 스크립트는 지금 **실행된다**(모듈을 넣을 땐 같이 꺼야 함) | naver 본문 | D3·D7 |

### A.3 레이아웃

| 후보 | 출처 | 측정 | 에이전트 의미 | 처분 |
|---|---|---|---|---|
| `display: contents` | 3차 5b | 원문 31·7호스트, mdn 본문 240px | 본문 폭·링크 위치 | 3a |
| 블록을 품은 인라인 폭 | 3차 5b | bbc 973px 링크 | 카드가 화면 밖 | 3b |
| 뷰포트보다 넓은 상자 | 3차 5단계 판정 | 8곳 | 화면 밖 요소 | 3c |
| 표 min-content 바닥 | 3차 2.1a | enwiki ~56 셀, −11 요소 | navbox 링크 빠짐 | 3d |
| kakaocorp 캐러셀 | 3차 3d | 문서 높이 2,102 → 4,244 | 첫 화면 같음 | 3e |
| `<body>` taffy 고유 | 3차 5s | 효과 1상자 | 없음 | D7 |
| `position: sticky` | 3차 2.3 | 원문 68·9호스트 | 클릭 좌표는 맞음 | 범위 밖 |
| `order` | 이번 측정 | 파싱만 329·10곳 | 시각 순서만 | 범위 밖 |
| `-webkit-line-clamp` | 3차 3단계 | 파싱만 372·7곳 | 잘려야 할 글이 보임 | 범위 밖 |
| `text-transform` | 이번 측정 | 파싱만 97·10곳 | 관찰 글자 대소문자 | 범위 밖 |

### A.4 셀렉터

| 후보 | 출처 | 규칙(`final`) | 처분 |
|---|---|---:|---|
| `:lang()` | 3차 4단계 | 1,228 — 인자 대부분 페이지 언어 아님 | 4 |
| `:has()` | 3차 4단계 | 125·10곳 | D7 |
| `:target` | 3차 4단계 | 88·5곳 | 4 |
| 결합자 중첩 상한 | 3차 4a | 13 | D7 |
| `:popover-open`(+ UA 규칙)·`:indeterminate`·`:read-only`·`:dir()`·`:invalid`·`:placeholder-shown`·`:host`·`:defined`… | 3차 4단계 + 리뷰 | 각 ≤9 | 4(`:defined` 는 N5) |

### A.5 CSS 값 (합산 ours 상위)

| 속성(문법) | 합산 ours | 곳 | 대표 | 처분 |
|---|---:|---:|---|---|
| `content`(`"x"/"alt"`·`attr()`) | 148 | 8 | apple 102 | 5c(파싱 + 이름) |
| `outline` 단축 | 112 | 10 | github 39 | 5c |
| `background`(그라디언트·여러 층) | 96 | 11 | yna 20 | 5c |
| 두 값 `gap` | 93 | 7 | yna 76 | 5a |
| CSS 중첩 `&` | 73 | 1 | velog | D7 |
| `filter` | 56 | 11 | naver 18 | 5c |
| 색 문법(`background-color` 34 · `color` 18 · IACVT 재파싱 ~53) | ~105 | 6+ | rustlang | 5b |
| 시스템 색(키워드 폴백 ~54 · `border-color` 26) | ~80 | 4 | github | 5b |
| `background-position`·`object-position`·`transform-origin` | 29·28·28 | 5·4·9 | kakaocorp·yna·daum | 5c |
| `transform`(+`-webkit-`) | 27+14 | 6 | kakaocorp | 5c |
| 두 값 `overflow` | 26 | 4 | github 22 | 5a(파서만) |
| `grid-area`·`grid-row`·`grid-column` 슬래시 | 25+9+5 | 3 | github | 5a |
| `grid-column: span 0` | 14 | 1 | apple | 5a(분류 먼저) |
| `mask*`(+`-webkit-`) | ~48 | 3 | daum | 5c |
| `aspect-ratio` | 19 | 8 | github 6 | 5a |
| `clip-path: polygon()` | 18 | 2 | github 17 | 5c |
| `backdrop-filter`(+`-webkit-`) | 15+6 | 6 | apple | 5c |
| `flex-flow` | 15 | 6 | velog 5 | 5a |
| `font` 단축 | 12 | 6 | docsrs·google | 5c |

계산값 시점 browser-too(`display: -ms-flexbox` 234 등, 3차 5r)는 목표 밖이라 표에 없다.

---

## 부록 B — JS 에러 사이트별

`final` 의 `console.txt` 에서 `Uncaught` 줄을 **콘솔 순서대로** 본 것이다(리뷰 반영 — 초안은 정렬 뒤 세어 순서가 틀렸다). 스크립트 파일 이름·위치는 생략했다. "없는 API" 는 `crates/mb-runtime/src/js` 에 정의가 없음을 코드로 확인한 것이다. 0단계 P4 가 이 표의 규칙 판정 부분을 자동으로 만들고, `… of undefined` 모양의 원인 추정(쿠키 등)은 이 수동 표가 정본이다(N2 ④).

| 사이트 | 줄(중복 제거) | 첫 에러 → 판정 | 뒤쪽 줄의 없는 API | 그 밖 |
|---|---:|---|---|---|
| naver | 2 (2) | `getElementsByTagName` → missing-api | — | 광고 SDK `Na`. 본문은 모듈(D3) |
| daum | 7 (7) | `href` of undefined → page-bug | `WebSocket` 객체 메서드(심 없음), `createTreeWalker`, `customElements` | `document.documentElement` 를 함수로 부름(원인 미확인) |
| yna | 12 (12) | `customElements` → missing-api | `IntersectionObserver`, `getElementsByTagName`(2), `screen` | `pathname`·`search`·`width` of undefined, `firebase.messaging` |
| hani | 1 (1) | `getElementsByTagName` → missing-api | — | Next.js "client-side exception"(콘솔 error 수준) |
| kakaocorp | 6 (6) | `getElementsByTagName` → missing-api | `createEvent` | 카카오 SDK `replace`/`width` of undefined |
| korea-kr | 31 (30) | 쿠키 문자열 `length` of undefined → page-bug(**`document.cookie` 추정**) | jQuery 3.7.1 `createElement` of undefined(**`document.implementation` 추정**), `getElementsByTagName`(인라인 스크립트 2곳과 wcslog) | `jQuery is not defined`(사슬), `firebase.messaging` |
| kowiki | 1 (1) | `match` of undefined → page-bug(**`document.cookie`**, 3차 1r) | — | |
| velog | 2 (2) | `matchMedia` → missing-api | `TextEncoder` | |
| d2 | 1 (1) | `URL` → missing-api | — | |
| google | 5 (5) | `structuredClone` → missing-api | `getElementsByTagName` | 내부 함수 not a function, `ved` of undefined |
| enwiki | 1 (1) | `match` of undefined → page-bug(**`document.cookie`**) | — | |
| docsrs | 3 (3) | `join` not a function → page-bug | `matchMedia` | `setAttribute` of undefined |
| bbc | 15 (3) | `getElementsByTagName` → missing-api | `document.currentScript`(13줄 반복, 추정) | |
| apple | 4 (4) | `indexOf` of undefined → page-bug | `matchMedia` | 같은 `indexOf` ×3 |
| microsoft | 4 (4) | `getElementsByTagName`(jQuery 1.9.1) → missing-api | — | `jQuery`/`$` is not defined(사슬), 컴포넌트 팩토리 |
| example | 1 (1) | `insertAdjacentHTML` → missing-api | — | |
| github·mdn·rustlang·hn | 0 | — | — | |

합계: `Uncaught` 96줄(중복 제거 83)·16곳. 첫 에러가 없는 API 인 사이트 **10**. 어떤 줄이든 없는 API 가 있는 사이트 **14**(위 10 + daum·korea-kr·docsrs·apple). page-bug 모양 첫 에러 중 쿠키로 추정되는 곳 3(enwiki·kowiki·korea-kr).

---

## 부록 C — 기대 컨트롤 누락 진단

`final` 의 `controls_missing` 을 동결 캐시의 문서 원문·관찰과 대조했다. 0단계 P5 가 앞으로 이 이유를 자동으로 적는다.

| 사이트 | 누락 | 원문에 있나 | 판정 |
|---|---|---|---|
| naver | `button:검색`·`link:메일`·`link:카페`·`link:뉴스` | 본문이 모듈 스크립트로 그려짐(2차 조사) | ES 모듈(D3) — 4차에선 안 고쳐짐 |
| daum | 4개 전부 | 정적 마크업 317바이트(3차 K9) | JS(2a·2b·2c) — 첫 에러는 page-bug 모양, 양파 깊이 불확실 |
| hani | `link:경제` | 서버 HTML 에 없음(메타 설명에만) | 클라이언트 렌더 + Next 예외 → 2a(`getElementsByTagName`) |
| korea-kr | `textbox:검색` | 입력칸 id 는 스크립트에만, 마크업엔 열기 버튼(sr-only "검색")만 | jQuery 3.7.1 이 만드는 검색창으로 추정 → 2a(`document.implementation` — 추정) + 2b(쿠키). `getElementsByTagName` 에러는 인라인 스크립트·wcslog 쪽이라 jQuery 와 무관 |
| velog | `button:로그인` | 서버 HTML 에 없음 | 클라이언트 렌더 → 2b(`matchMedia`·`TextEncoder`) |
| d2 | `link:helloworld` → **`link:hello world`**(T, 0단계 ⑥) | 있음 — 관찰에도 "Hello world" | **철자**(D6) — `r4-base` 부터 고친 철자로 잰다 |
| google | `link:gmail` | 있음(`header` 안 링크). 인라인 시트에 감싼 상자 클래스의 `display:none` | 원인 미확인 — 다른 시트(외부·스크립트 주입)가 펼치는 것으로 추정. P5 로 다시 보고 1~2단계 뒤 재판정 |
| (crates.io) | `textbox:search`·`link:browse all crates` | 404 빈 본문 | K5 + 문서 보충(N1-나) — ① 분모 밖 |

---

## 부록 D — 측정 재현과 명령

- 이 문서의 수치는 라벨 `final`(3차 5r, `cef51c5`)의 `corpus-report.tsv`·사이트별 `css.txt`·`console.txt`·`elements.txt` 다.
- 같은 커밋으로 다시 돌린 `r4-check`(2026-11-28, 오프라인·release)는 `load_ms`·`style_ms`·`raster_*`·`budget_lines` 를 뺀 **모든 열이 같고**, `compat-corpus.sh diff final r4-check` 는 비어 있다.
- 부록 A 의 "원문" 수치와 부록 B 의 중복 제거 수(메시지 + 위치 동일)는 캐시·콘솔을 정규식으로 센 일회성 계산이다. 저장소에 스크립트는 없다 — 0단계 P4·P6 이 대신한다.
- 명령:

```sh
# 재생(항상 오프라인, release 빌드를 스크립트가 함)
MB_NET_CACHE=$PWD/compat-cache/net scripts/compat-corpus.sh run <라벨>
# 두 실행의 요소 차이(0단계 뒤 unreachable.txt 비교도)
scripts/compat-corpus.sh diff <라벨A> <라벨B>
# JS 에러 판정(0단계 P4 — 저장된 console.txt 후처리)
scripts/compat-corpus.sh classify <라벨>
# 보충(0단계 S — 미스 목록 파일)
MB_NET_CACHE=$PWD/compat-cache/net scripts/compat-corpus.sh record --refetch <미스목록파일>
```

- **라벨**(동결 — 각 단계의 "전"은 앞 단계의 "후", 그 단계에 P 가 있으면 P 라벨):

  | 라벨 | 단계 | 엔진 · 캐시 | 쓰임 |
  |---|---|---|---|
  | `r4-p0` | 0 P | `cef51c5` + probe 변경 · 동결 캐시 | 새 열의 첫 측정, 옛 열 = `final` 확인 |
  | `r4-k5` | 0 K5 | + K5 · 동결 캐시 | `r4-p0` 와 **빈 diff** 확인 |
  | `r4-base` | 0 끝 | + 보충 ① | 1단계 "전", ②의 기준·목표 창 |
  | `r4-vis` | 1 | 1단계 엔진 | 1단계 "후" |
  | `r4-js-1`, `r4-js-2`… | 2 | 겹마다 | 양파 기록 |
  | `r4-js-sup` | 2 끝 | 2단계 엔진 · 보충 ② 뒤 캐시 | 2단계 "후" |
  | `r4-base-sup` | 2 끝 | `r4-base` 엔진 · 보충 ② 뒤 캐시 | ③·④·보조 지표의 기준 |
  | `r4-layout-p` | 3 P | 2단계 엔진 + 3단계 probe 변경(3c 진단 표) | 3단계 "전" |
  | `r4-layout` | 3 | 3단계 엔진 | 3단계 "후" |
  | `r4-sel-p` | 4 P | + 4단계 probe 변경(N5 측정) | 4단계 "전", N5 결정 근거 |
  | `r4-sel` | 4 | 4단계 엔진 | 4단계 "후" |
  | `r4-val-p` | 5 P | + 5단계 probe 변경 | 5단계 "전" |
  | `r4-val-a` | 5a | 5a 엔진 | |
  | `r4-val-b` | 5b | 5b 엔진 | |
  | `r4-final` | 5c 끝 | 4차 끝 | ②·보조 지표의 판정 |

  P 가 없는 단계(1·2)는 probe 를 바꾸지 않는다. 바꿔야 하면 그 단계 앞에 `-p` 라벨을 더하고 이 표를 먼저 고친다.

---

## 부록 E — 골든·eval 단계별 기대표

`compat4.html` 은 오늘의 **틀린** 관찰을 골든으로 남기고 단계마다 바뀐다. 골든은 스크립트를 돌린 관찰 둘(불러온 직후 · "Menu" 클릭 뒤)이다(0단계 ⑤ 메모). `compat4-menu` 는 "JS 로 여는 메뉴를 펼쳐 항목으로 이동" 시나리오다. 메뉴는 `visibility:hidden` 으로 닫혀 있고, `getElementsByTagName`·`matchMedia` 로 여닫는다.

| 단계 | `compat4` 골든 | `compat4-menu` eval(LLM, 3회 — **사용자가 자기 터미널에서** `scripts/eval-run.sh`) | 대응 테스트 `compat4_menu_opens_through_its_script_and_leads_to_docs`(LLM 없음) |
|---|---|---|---|
| 0(오늘) | 기록 | **통과할 수 있음**(숨은 메뉴가 보이고 눌려서 — 틀린 이유로) 3/3 기대 | 메뉴 항목이 관찰에 있음을 단언(틀린 상태 기록) |
| 1 | 메뉴 항목 사라짐 | **실패가 정상** 0/3(스크립트가 죽어 메뉴가 안 열림) | 항목 없음 + 메뉴 버튼 클릭 뒤에도 없음 |
| 2 | 메뉴 버튼으로 열림 | **통과** 3/3 | 버튼 → 항목 나타남 → 이동 |
| 3~5 | 배치·이름 변화만 | 통과 유지 3/3 | 유지 |

골든·eval 시나리오·대응 테스트는 0단계 항목 G 에서 만든다. 기존 eval 은 1단계 완료 기준대로 사용자가 다시 돌린다 — 기준은 concept.md 847행의 시나리오 6개 × 14회 결과(14/14)다. `eval/scenarios` 의 8개 중 `compat3-menu`·`preact-todo` 는 그 기준 밖이다.
