# 실사이트 호환성 조사 — 호환성 2차 (2026-11-26)

> 상태: **조사 완료 + 에이전트를 막던 6개 버그 수정 (803 green)** · 다음 = **[호환성 3차 설계 — compat-round3.md](compat-round3.md)** (0단계 견고성 점검부터) · **3차 코퍼스 25곳 기준선 = [아래 0b 절](#3차-코퍼스-기준선-0b)** (2026-11-27)

naver.com 을 관전 창에 띄웠더니 "로드는 됐는데" 화면이 무너졌다 — 빈 테두리 상자 줄, 글자·이미지·메뉴 거의 없음, "검색" 이 한 글자씩 세로로, 에이전트 관찰엔 클릭할 것이 배지 하나(e186). 데모 페이지는 잘 되는데 실사이트는 왜 깨지는지, **추측 대신 측정**해서 원인별로 묶고 순위를 매긴다.

## TL;DR

- **에이전트를 통째로 막던 건 CSS 가 아니라 엔진 버그였다.** 5곳 중 4곳이 "깨진 화면" 이전에 아예 실패했다: Google·GitHub 는 `<img>` 하나에 `src` 가 없거나 `<link rel=stylesheet>` 에 `href` 가 없다는 이유로 **이동 전체가 실패**(빈 페이지, `url: (none)`), Wikipedia 는 grid 레이아웃 **패닉**, 패닉을 고치자 중첩 `<span>` 측정이 **2^깊이** 로 돌아 몇 분간 멈춤. 네 개 다 고쳤고(아래 [이번에 고친 것](#이번에-고친-것)), 이제 5곳 모두 에이전트가 주요 컨트롤(검색창·검색 버튼·파일 목록·본문)을 찾는다 — **naver 의 본문만 빼고**.
- **naver 본문이 없는 이유는 CSS 가 아니라 ES 모듈.** 서버가 주는 HTML 은 껍데기(태그 118개)뿐이고 뉴스·메뉴·바로가기는 `<script type="module">` 두 개(`main.*.js` 670KB, `search.*.js` 266KB)가 그린다. 모듈 미지원(Tier 2) → 그 부분이 통째로 없다. 남은 화면 붕괴("빈 상자 줄")는 `<input type="hidden">` 13개가 그려진 것이었다(고침).
- **화면을 무너뜨리는 1순위는 CSS 파서가 조용히 버리는 것들.** GitHub 은 CSS 의 46%(1.17MB)가 `@layer` 안이라 통째로 버려진다. 선언 값이 토큰 하나가 아니면 통째로 버린다: `!important` 가 붙은 선언(GitHub 3,550개), `border: 1px solid #ccc` 같은 shorthand(5/5 사이트), `calc()`·`max()`, 쉼표 목록(`font-family`, 여러 겹 `box-shadow`, `--x: 255,255,255`). 그리고 **모든 `@media` 가 참**으로 처리돼 GitHub 는 다크 모드 + 모바일 규칙으로, 위키는 인쇄용·다크 규칙까지 같이 먹는다.
- **"한 글자씩 세로로" 의 정체는 스크린 리더 전용 텍스트**(`.blind`, `.sr-only`, `visuallyHidden`: `position:absolute; width:1px; overflow:hidden; clip:rect(0 0 0 0)`). 페인트에 overflow 클리핑이 아예 없어서 1px 폭 상자 안 글자가 줄마다 한 글자씩 그려진다. 4/5 사이트. 에이전트 관찰엔 무해(실제 a11y 트리에도 있는 텍스트)하지만 사람 눈엔 가장 눈에 띄는 붕괴.
- **3차 순서 제안**: ① `!important` + 값 파서 묶음 → ② `@media` 평가 → `@layer` 펼치기 → ③ overflow 클리핑 → ④ 셀렉터(`:is`/`:where`/`:checked`…) → ⑤ `calc()`/`var()` 값 안 → ⑥ 이미지(WebP·콘텐츠 스니핑) → ⑦ JS 소소(`document.cookie`) → ⑧ ES 모듈(naver) → ⑨ `::before/::after`.

---

## 방법

**대상** (각 사이트 실제 요청 1회 — 이후는 전부 로컬 재생):

| 이름 | URL | 성격 |
|---|---|---|
| naver | `https://www.naver.com/` | SSR 껍데기 + 모듈 JS, CSS 750KB |
| wiki | `https://ko.wikipedia.org/wiki/웹_브라우저` | 서버 렌더 문서, Vector 2022 스킨(grid) |
| google | `https://www.google.com/` | 인라인 `<style>` 76KB, JS 무거움 |
| github | `https://github.com/tokio-rs/tokio` | 공개 저장소 페이지, CSS 2.5MB |
| hn | `https://news.ycombinator.com/` | 테이블 레이아웃, CSS 7KB (기준선) |

**도구** (이번에 추가, 앞으로의 조사에도 그대로 쓴다):

- `MB_NET_CACHE=DIR` — 네트워크 기록/재생 캐시([net/cache.rs](../crates/mb-runtime/src/net/cache.rs)). 설정하면 모든 fetch 가 `DIR` 를 먼저 보고, 없으면 받아서 저장한다. `MB_NET_CACHE_OFFLINE=1` 이면 캐시에 없을 때 네트워크 대신 에러 — 재생 전용. 사이트는 한 번만 방문받고, 엔진은 몇십 번이든 같은 입력으로 돌릴 수 있다. `DIR/index.tsv` 에 받은 것마다 상태·Content-Type·크기·URL 한 줄.
- `css::parse_with_report` — 파서가 버린 것을 돌려준다([css/report.rs](../crates/mb-dom/src/css/report.rs)): 셀렉터 때문에 버린 규칙, 값 때문에 버린 선언(`name: value` 원문), 본 at-rule 전부(이름·조건·본문 유지 여부). 평소 `parse` 는 기록하지 않는다(스레드 로컬 슬롯이 비어 있음).
- `compat_probe` 예제([crates/mb-agent/examples/compat_probe.rs](../crates/mb-agent/examples/compat_probe.rs)) — `AgentBrowser::navigate`(시스템 폰트, 가상 시계)로 에이전트처럼 열고 `OUT_DIR` 에 `observation.txt`(뷰포트 관찰 + 전체 페이지), `css.txt`(버린 선언을 속성별·버린 규칙을 원인별·`@media` 조건 종류별·파싱은 됐지만 아무도 안 읽는 속성), `layout.txt`(글자 폭이 한 글자당 3px 미만인 "슬리버", 화면 밖 상자, 레이아웃 상자를 얻은 `style`/`script`/`template` 과 그 레이아웃 조상 경로), `summary.txt`.
- 화면은 기존 `mb-mcp --start-url URL --preview PNG` 를 같은 캐시로.

```sh
mkdir -p compat-cache
export MB_NET_CACHE=$PWD/compat-cache/net          # 첫 실행만 네트워크
cargo build -p mb-mcp && cargo build -p mb-agent --example compat_probe
./target/debug/examples/compat_probe https://news.ycombinator.com/ compat-cache/hn
MB_NET_CACHE_OFFLINE=1 ./target/debug/mb-mcp --start-url https://news.ycombinator.com/ --preview compat-cache/hn.png
```

**저작권·예의 규칙**: 실사이트 HTML/CSS/이미지/스크린샷은 git-ignored `compat-cache/` 에만 둔다(공개 저장소). 이 문서엔 측정치와 짧은 셀렉터·선언 예시만 적고, 테스트는 한 가지 동작만 재현하는 손으로 쓴 작은 HTML 이다. User-Agent 는 엔진 기본(데스크톱 Chrome 문자열), 크롤링 없음 — 사이트당 문서 1회 + 그 하위 리소스 1회씩(전체 118건).

**뷰포트**: 1280×800, 라이트. "틀린 `@media`" 판정도 이 기준.

---

## 사이트별 결과

수치는 이번 수정 **후** (`compat_probe` 최종 실행). "수정 전" 은 같은 캐시로 처음 돌렸을 때.

### naver.com

| | 수정 전 | 수정 후 |
|---|---|---|
| 로드 | 됨 | 됨 |
| 화면 | 빈 테두리 상자 3열 격자 + "검색" 세로 + 검색창 테두리 | 검색창 테두리 + "검색" 세로 |
| 관찰 | 링크 8(건너뛰기) + combobox 1 | 같음 |

- **빈 상자 격자 = `<input type="hidden">` 13개.** 검색 폼의 숨은 필드가 UA 기본 `display:none` 이 없어서 inline-block 입력칸으로 그려졌다. 에이전트 관찰엔 원래 안 나왔다(역할 판정에서 제외) — 사람 눈에만 보이던 붕괴. **고침**.
- **"검색" 세로 = `<legend class="blind">검색</legend>`.** `.blind{position:absolute;clip:rect(0 0 0 0);width:1px;height:1px;margin:-1px;overflow:hidden}` — 1px 폭 상자에 글자가 줄마다 한 글자씩, `overflow:hidden` 클리핑이 없어 그대로 그려진다. `clip:rect()` 는 파서가 버린다.
- **본문이 없는 이유 = ES 모듈.** 문서 본문 태그는 118개(건너뛰기 링크, 헤더, 검색 폼, 광고 자리)뿐이고 뉴스스탠드·바로가기·쇼핑·피드는 `<script type="module" src=".../main.*.js">` 와 `search.*.js` 가 만든다. 우리는 `type=module` 을 실행하지 않는다(spa-roadmap Tier 2). 검색 버튼(`#search-btn`)도 인라인 `style="display:none"` 로 와서 JS 가 켜는 구조라 안 보인다 → **에이전트는 검색창(e186)에 입력은 할 수 있지만 검색 버튼이 없다** (Enter 로 폼 제출은 가능).
- CSS: 규칙 2,244 유지 / **827 버림**(셀렉터; 787 이 `:before`/`:after` 레거시 한 콜론 — 아이콘·구분선), 선언 458 버림: `background-size`(76, 2값), `border`(70), `color: rgb(var(--…))`(47), `flex: 1 1 auto`(24), `border-top`(23), 쉼표 `box-shadow`(23), 커스텀 속성 `--atcp-sys-color-*: 255,255,255`(쉼표 값, 수십 개 → 그걸 쓰는 `rgb(var(--…))` 도 연쇄 실패). `:where(:root,:host)` 디자인 토큰 규칙 6개가 통째로 버려짐.
- 이미지: 스프라이트 PNG 5장(`sp_main.*.png` 424KB 등)이 `Content-Type: application/octet-stream` 으로 와서 `net::load_image` 의 `image/*` 검사에 **전부 거절**. 디코드 1장(파비콘 제외).
- JS: 광고 SDK 2개가 `not a function` 으로 멈춘다 (사용자 `--window` 실행에서도 각 2회). `MB_JS_STACK=1` 로 호출 지점을 찾았다:
  - `ndp-loader.js:1:854` — `document.getElementsByTagName("head")[0].insertBefore(script, …)`: **`document.getElementsByTagName` 없음**. 스크립트 태그를 동적으로 꽂는 가장 흔한 관용구라 광고 밖에서도 자주 쓰인다.
  - `gfp-core.js:1:35942` — `setPolicyInfo` 안의 `console.warn(…), console.debug(…)`: **`console.debug` 없음** (`console.warn` 만 찍히고 바로 다음 줄에서 죽는다 — stderr 의 `setPolicyInfo [object Object]` 가 그 흔적).
  - 둘 다 광고라 레이아웃 붕괴와는 무관하지만, 같은 계열 API 가 넓게 비어 있다는 신호다 → [JS API 프로브](#js-api-표면-프로브).

### ko.wikipedia.org (문서)

| | 수정 전 | 수정 후 |
|---|---|---|
| 로드 | **패닉** (`layout/grid.rs:122` index out of bounds) | 됨 (debug 4.6초) |
| 패닉 수정 직후 | **2분 넘게 CPU 100%** (끝나지 않음) | — |
| 관찰 | 없음 | 요소 432, 텍스트 블록 260 — 검색창(e232)·검색 버튼·본문 전문·목차 링크 |

- **패닉**: Vector 2022 는 `grid-template-areas` 로 2열을 이름 붙이면서 `grid-template-columns` 는 `15.5rem minmax(0,1fr)` — `minmax()` 를 못 읽어 선언이 버려짐 → 트랙 1개짜리 표에 2열째 영역이 들어와 인덱스 초과. **고침**(영역이 암시하는 열 수만큼 트랙을 늘림).
- **멈춤**: 인라인 상자 크기를 잴 때 너비용·높이용으로 자식을 **두 번** 훑고 자식도 똑같이 해서 2^깊이. 위키의 중첩 `<span>`(각주·틀)에서 사실상 무한. **고침**(한 번 훑어 둘 다).
- JS: `RLQ is not defined` ×4 — 첫 줄이 `(RLQ=window.RLQ||[]).push(…)`(암묵 전역). rquickjs `EvalOptions::default()` 가 **strict 강제**라서 클래식 스크립트가 전부 strict 로 돌고 있었다. **고침**(sloppy, 소스의 `"use strict"` 는 그대로). 남은 것: `document.cookie` 없음 → `cannot read property 'match' of undefined`.
- 화면: 본문이 오른쪽으로 밀려 잘림, "주 메뉴" 세로(`vector-dropdown` 라벨 — 같은 1px 패턴). `minmax()` 미지원으로 그리드 트랙이 틀린 탓이 크다. 가장 넓은 상자 2,265px(뷰포트 1,280).
- CSS: 선언 233 버림 중 **91 이 `!important`** (`display:none !important` 12 — 숨겨야 할 게 보임, `color:inherit !important` 20). `@media` 85개 중 **틀리게 적용**: `prefers-color-scheme: dark` 14, `print` 8, `min-width > 1280` 4, `max-width < 1280` 1, `prefers-reduced-motion` 1. 셀렉터 버림 200: `::before/::after` 58, `:target` 등 기타 51, `:where()` 17, `:checked` 16.

### google.com

| | 수정 전 | 수정 후 |
|---|---|---|
| 로드 | **실패** — "image missing src" (빈 페이지, `url: (none)`, 요소 0) | 됨 |
| 관찰 | 요소 0 | 요소 18: 검색 combobox(e136)·"Google 검색"/"I'm Feeling Lucky" 제출 버튼·음성/이미지 검색·푸터 링크 |

- **이동 실패**: `<img>` 7개 중 6개가 `src` 없음(JS 가 채움). `resource::collect_image_urls` 가 `?` 로 `MissingSrc` 를 올려 **문서 로드 전체**가 실패했다. 같은 이유로 해석 안 되는 `src`/`href` 하나도 페이지를 죽였다. **고침**(그 리소스만 건너뜀).
- **버튼 이름이 CSS 원문**: 버튼 안에 `<style>` 을 넣는다(`<button class="…"><style>.plR5qb…{display:none}…</style>…`). 접근 가능한 이름이 모든 자손 텍스트를 모아서 이름이 `".plR5qb.PHjFye .CcxW7b{display:none}…"`. 화면에도 그 CSS 가 글자로 찍혔다 — 버튼이 flex 항목이고 `display:none` 자식 뒤에 블록 자식이 오면 `layout_inline_block_node` 의 블록 분기가 `display:none` 을 거르지 않았다. **둘 다 고침**.
- 화면: 로고 없음(JS/이미지), 키보드 아이콘이 거대(스프라이트 배경 크기·`background-size` 미지원 추정), 상단 대부분 공백.
- CSS: `@media (forced-colors: active)` 23개가 적용됨 → `border: 1px solid CanvasText` 같은 고대비 규칙(이건 `border` shorthand 미지원이라 오히려 버려져서 덜 망가짐). `font-family: Arial,sans-serif`(25, 쉼표), `calc(100% - 16px)`, `:focus-visible` 21 규칙 버림.
- JS: 인라인 스크립트 5개 + 외부 2개 에러(`cannot read property 'addEventListener' of undefined`, `'ved' of undefined`, `not a function`) — 원인 미분류(아래 [미분류](#미분류--후속-조사)).

### github.com (공개 저장소)

| | 수정 전 | 수정 후 |
|---|---|---|
| 로드 | **실패** — "stylesheet missing href" | 됨 (debug 21초) |
| 화면 | 빈 페이지 | **전체가 다크 테마**(#0d1117), "Navigation Menu" 세로 |
| 관찰 | 요소 0 | 요소 165: 헤더 메뉴·Sign in·Code/Issues/Pull requests 탭·브랜치 버튼·파일 목록(`tokio-macros, (Directory)` 등)·README |

- **이동 실패**: `<link rel=stylesheet>` 하나에 `href` 가 없음 → 위와 같은 `?` 로 문서 전체 실패. **고침**.
- **CSS 의 46% 가 통째로 버려짐**: 2.56MB 중 **1.17MB 가 `@layer` 블록 5개 안**(Primer primitives)이고 파서는 `@media` 외 at-rule 본문을 전부 버린다. `@supports` 40블록·`@container` 21블록(작음)도 같이. 지금 보이는 GitHub 은 Primer 의 절반이 없는 상태.
- **다크 모드**: GitHub 은 `data-color-mode="auto"` + `@media (prefers-color-scheme:dark)`. 파서가 본 최상위 다크 블록 22개(+ 버려진 `@layer` 안에 49개 더). 모든 `@media` 가 참이라 라이트·다크 둘 다 적용되고 뒤에 오는 다크가 이긴다. ⚠ `@layer` 를 먼저 펼치면 다크 블록 49개가 더 살아나 **더 어두워진다** — `@media` 평가와 같이, 또는 그 뒤에. 같은 이유로 `(width<=calc(48rem - .02px))` 같은 모바일 규칙도 적용. GitHub 은 **범위 문법**(`width>=768px`)을 주로 쓴다 — 평가를 넣을 때 범위 문법이 필수.
- **`!important`**: 선언 6,533 버림 중 **3,550 이 `!important`** (Primer 유틸리티 클래스 `d-none`·`p-0`… 가 전부 `!important`). 그 밖에 `calc(var(--base-size-8) + var(--Layout-row-gap))`(margin-top 416 등), `padding: 5px var(--base-size-16)`(값 안 `var()`), `border: 1px solid`(209).
- 셀렉터 버림 814: `:before/:after` 438, `:is()` 127, `:focus-visible` 86, `:disabled`/`:checked` 40, `:has()` 18.
- 레이아웃: "슬리버" 17 — 전부 sr-only(`Navigation Menu`, `Topics`, `Stars`…). 가장 넓은 상자 **187,319,520px** — 무언가 무한에 가까운 폭(미분류). `<template>` 9개가 레이아웃 상자를 얻음(UA `display:none` 없음) → **고침**.
- 이미지: 13장 중 디코드 5. **WebP 7장**(`image` crate 에 webp feature 꺼짐), camo SVG 배지는 됨.
- 관찰 이름 중복: `Issues 299 ( 299)`, `Star 33.8k ( 33.8k)` — 카운터 + VisuallyHidden 사본이 둘 다 이름에 들어감(실제 Chrome 도 비슷하게 읽힘, 낮은 우선순위).

### news.ycombinator.com (기준선)

- 원래 잘 되던 곳. 요소 225, 화면 정상(배지 밀집은 오버레이 문제).
- 그래도 측정하니: `font-family: Verdana, Geneva, sans-serif` 13개 **전부 버려짐**(쉼표), `@media` 12개 중 **10개가 `max-width < 1280`** — 모바일 규칙이 데스크톱에 적용 중. HN 이 덜 망가져 보이는 건 그 모바일 규칙이 대부분 글자 크기 정도라서다.

---

## 이번에 고친 것

에이전트를 막는 것만, 각각 실패하는 테스트 먼저. 793 → **803** (+10).

| # | 증상 (사이트) | 원인 | 수정 | 테스트 |
|---|---|---|---|---|
| C1 | 이동 전체 실패, 빈 페이지 (Google·GitHub) | `<img>` 의 `src`·`<link rel=stylesheet>` 의 `href`·`<script src>` 가 없거나 해석 불가하면 `?` 로 문서 로드 실패 | `resource::resolve_attr` — 그 리소스만 건너뜀 | `img_without_src_or_link_without_href_is_skipped_not_fatal` |
| C2 | 레이아웃 패닉 (Wikipedia) | `grid-template-areas` 가 `grid-template-columns` 보다 열이 많음 → 트랙 인덱스 초과 | `grid::effective_column_tracks` — 영역 열 수만큼 트랙 보충(선언이 없으면 `1fr`, 있으면 `auto`) | `grid_template_areas_wider_than_column_tracks_does_not_panic` |
| C3 | 몇 분 멈춤 (Wikipedia) | 인라인 크기 측정이 너비·높이를 따로 재며 자식을 두 번씩 → 2^깊이 | `inline::inline_content_size` 한 번에 둘 다 | `deeply_nested_inline_elements_lay_out_in_linear_time` (28단계) |
| C4 | `RLQ is not defined` 등 (Wikipedia) | rquickjs 기본 `EvalOptions` 가 strict 강제 | `classic_script_options()` — `strict = false` | `classic_scripts_run_in_sloppy_mode` |
| C5 | 버튼 이름·화면에 CSS 원문 (Google) | ① 접근 가능한 이름이 `<style>`/`<script>` 텍스트까지 모음 ② inline-block 의 블록 자식 분기가 `display:none` 미필터 | ① `index::push_text` 가 `script/style/template/noscript` 건너뜀 ② `.filter(!is_display_none)` | `style_and_script_text_stay_out_of_accessible_names`, `display_none_children_get_no_box_in_any_container` |
| C6 | 빈 상자 격자 (naver), `<template>` 내용 배치 (GitHub) | UA 기본에 `[hidden]`·`input[type=hidden]`·`template` 의 `display:none` 없음 | `defaults::is_hidden_by_ua` (UA 레벨 — 작성자 CSS 가 이김) | `ua_hides_hidden_attribute_hidden_inputs_and_templates` |

부수: 골든 2개 `page_height` 갱신 — `links_and_text` 375→344(`<p hidden>` 이 이제 숨음), `signup_form` 245→261(숨은 입력칸이 빠져 인라인 줄바꿈 위치가 바뀌어 한 줄 늘어남; Chrome 도 숨은 입력칸에 자리를 안 줌). 도구 쪽 테스트 3개(`cache_key_*`, `report_*` 2).

---

## 원인별 갭 목록

영향: **사이트 수** · **얼마나 나쁜가** (에이전트 차단 > 사람 화면 붕괴 > 모양 어긋남) · 노력 S(하루 이내)/M(며칠)/L(한 주+).

### CSS 파싱 (값)

선언 값 파서가 `parse_entirely` 안에서 토큰 하나만 읽는다(`css/parse.rs::parse_value`). 전용 파서가 있는 속성(`margin`/`padding`/`flex`/`background`/`box-shadow`/`transform`/`grid-*`…) 말고는 토큰이 둘 이상이면 **선언 통째로 버림**.

| 갭 | 사이트 | 규모 (버린 선언) | 영향 | 노력 |
|---|---|---|---|---|
| `!important` (`parse_important` 안 부름 + cascade 에 중요도 없음) | 4/5 | GitHub 3,550 · Wiki 91 · Google 2 · HN 1 | 유틸리티 클래스(`d-none!important`, `display:none !important`)가 안 먹어 **숨길 게 보이고** 간격이 무너짐 | S~M (파서 S + cascade 에 important 층) |
| `border`/`border-top…` shorthand | 5/5 | GitHub 209+ · naver 109 · Google 33+ · Wiki 43 | 테두리 대부분 사라짐(남은 테두리는 입력칸 UA 기본) | S |
| 쉼표 목록 (`font-family`, 여러 겹 `box-shadow`, `transition`) | 5/5 | HN 13 · Google 25 · naver 23 | `font-family` 는 monospace 판정에만 쓰여 영향 작음, `box-shadow` 사라짐 | S |
| 커스텀 속성 값이 임의 토큰열 (`--x: 255,255,255`) + `rgb(var(--x))` | naver·GitHub | naver 60+ (연쇄로 색 47) | 디자인 토큰 색이 사라짐 | S (커스텀 속성은 원문 토큰 보관) |
| `calc()`/`min()`/`max()`/`clamp()` | 3/5 | GitHub 수백(margin·padding) · Google 16 · Wiki 9 | 간격·크기 틀림 | M (var 치환 후 계산, 단위 혼합) |
| 값 안의 `var()` (shorthand 속) | GitHub·naver | `padding: 5px var(--x)` 등 수백 | 간격 | M (calc 와 같은 자리) |
| `minmax()` / `repeat()` 그리드 트랙 | Wiki | 3 (but 레이아웃 전체) | 위키 본문이 밀리고 잘림 | S~M |
| `background-size`, `clip`, `flex: 1 1 auto` | naver·Google | naver 76+24 · Google 18 | 스프라이트 아이콘 위치·크기, flex 늘어남 | S |

### CSS 캐스케이드 / at-rule

| 갭 | 사이트 | 규모 | 영향 | 노력 |
|---|---|---|---|---|
| **`@media` 평가 없음 — 전부 참** | 5/5 | 틀리게 적용: GitHub dark 22(+범위 문법 수백) · Wiki dark 14/print 8/폭 5 · HN 폭 10 · Google forced-colors 23 · naver 폭 2 | GitHub **전체 다크**, 모바일 레이아웃 규칙이 데스크톱에, 인쇄용 규칙 적용. 모바일 규칙의 `display:none` 은 에이전트가 보는 요소도 바꾼다 | M — 파싱 때 조건을 버리지 말고 규칙에 붙여 두고, 스타일 때 뷰포트로 평가. `width/height`(px·em·rem, 범위 문법 포함), `prefers-color-scheme: light`, `print`=거짓, `forced-colors: none`, `prefers-reduced-motion: no-preference`, `hover: hover`/`pointer: fine`. 창 크기가 바뀌면 재스타일(이미 프레임마다 스타일 → 캐시 키에 뷰포트 추가) |
| `@supports` 본문 통째 버림 | GitHub(40블록, 7.8KB) 외 | 현대 기능 감지 블록이 다 빠짐 | S (조건 평가 없이 "참" 으로라도 펼치기 — 단 우리가 지원 안 하는 기능의 블록일 수 있어 주의) |
| **`@layer` 본문 통째 버림** | GitHub | 1.17MB / 2.56MB (46%), 5블록 | Primer 기본 스타일 절반이 없음 | S — `@media` 처럼 펼치기(레이어 우선순위는 "레이어 안 < 레이어 밖" 만 근사). **`@media` 평가 뒤에** (안에 다크 블록 49개) |
| `@container`, `@custom-media`, `@property` | GitHub | 21 / 6 / 1 | 작음 | M / S / S |

### 셀렉터

`parse_non_ts_pseudo_class` 가 `hover/focus/active/link/visited` 만 받는다. 그 밖의 의사 클래스가 하나라도 있으면 **규칙 통째로 버림**(콤마로 묶인 다른 셀렉터까지).

| 갭 | 사이트 | 버린 규칙 | 영향 | 노력 |
|---|---|---|---|---|
| `::before`/`::after`/`:before` (생성 콘텐츠) | 5/5 | naver 792 · GitHub 438 · Wiki 84 · Google 60 | 아이콘·구분선·따옴표. 버리는 것 자체는 안전(요소에 잘못 붙지 않음) — 생성까지는 큰 일 | L |
| `:is()` / `:where()` | GitHub 127 · Wiki 17 · naver 6 | 144+ | `:where(:root)` 디자인 토큰, 버튼 상태 | **S** — selectors crate 가 지원, `parse_is_and_where() -> true` 만 |
| `:focus-visible` / `:focus-within` | GitHub 86 · Google 22 | 108 | 포커스 링, 그리고 콤마로 묶인 일반 규칙까지 같이 사라짐 | S (focus 와 같게) |
| `:checked`/`:disabled`/`:enabled` | GitHub 40 · Wiki 16 | 56+ | 드롭다운(체크박스 해킹)·비활성 버튼 모양 | S (속성 기반 — 이 엔진은 checked 가 속성) |
| `:has()` | GitHub 18 · naver 3 · Google 2 | 23 | 적음 | M |
| `::placeholder`, `::-webkit-*`, `:target`, `:dir()` | 여럿 | 소수 | 낮음 | S~M |

### 레이아웃 / 페인트

| 갭 | 사이트 | 영향 | 노력 |
|---|---|---|---|
| **overflow 클리핑 없음** (`overflow:hidden`·`clip`·`clip-path`) | 4/5 (sr-only 패턴) + 캐러셀 | 1px 상자 글자가 한 글자씩 세로로(naver "검색", GitHub "Navigation Menu" 등 17, Wiki "주 메뉴"), 넘치는 내용이 덮어 그림 | M — 표시 목록에 PushClip/PopClip, 래스터에 클립 마스크. `clip: rect(0 0 0 0)`/`clip-path: inset(50%)` 은 "안 그림" 으로 |
| `minmax()` 그리드 → 트랙 계산 틀림 | Wiki | 본문 오른쪽으로 밀려 잘림 | S~M |
| 거대 폭 상자 (187M px) | GitHub | 원인 미분류 | ? |

### 이미지 / 폰트

| 갭 | 사이트 | 영향 | 노력 |
|---|---|---|---|
| WebP 디코더 없음 (`image` crate feature 꺼짐) | GitHub 7장 | 마케팅 이미지·일러스트 | S (feature 하나, 바이너리 크기 확인) |
| Content-Type 이 `image/*` 아니면 거절 | naver 스프라이트 5장 | 아이콘 전부 | S — 바이트 스니핑(`image::guess_format`)으로 판정 |
| `src` 없는 `<img>`(JS 가 채움, lazy) | Google 6/7 | JS 가 채우면 되는데, 동적으로 바뀐 `src` 를 다시 받는 경로가 있는지 미확인 | ? |

### JS API

| 갭 | 사이트 | 영향 | 노력 |
|---|---|---|---|
| ~~strict 강제~~ | Wiki | **고침 (C4)** | — |
| `document.cookie` 없음 | Wiki | 첫 인라인 스크립트 실패 | S (문서별 메모리 jar; `Set-Cookie` 연동은 별도 결정 — 보안 규칙과 함께) |
| `<script type="module">` 미실행 (Tier 2) | naver | **naver 본문 전체** | L — spa-roadmap Tier 2 (`loader` feature 선행) |
| `document.getElementsByTagName`, `console.debug` 등 표면 API 다수 없음 | naver (광고 SDK 2개 사망) | [프로브](#js-api-표면-프로브) 참고 | S (각각 작음, 묶어서) |
| 최상위 스크립트 에러가 **stderr 에만**, 호출 지점 없이 | 5/5 | 에이전트 관찰 `console:` 에 안 나옴; 메시지는 QuickJS 의 `not a function` 뿐(V8 은 `document.getElementsByTagName is not a function`) — 파일:줄:열은 `MB_JS_STACK=1` 일 때만 | S — [진단성](#스크립트-에러-진단성) |
| Google 인라인 스크립트 에러 5종 | Google | 아직 측정 안 한 원인 (위 표면 API 갭일 가능성 큼) | ? |

#### JS API 표면 프로브

손으로 쓴 한 페이지(`typeof` 만 찍음, 저장소 밖)로 흔한 표면을 훑었다. **없음**(`undefined`)으로 나온 것:

- `document`: `getElementsByTagName`, `getElementsByName`, `documentElement`, `currentScript`, `cookie`, `createComment`, `createEvent`
- `console`: `debug`, `info`, `trace`, `table`, `group`/`groupEnd`, `time`/`timeEnd`, `assert`, `count`, `dir` (있는 건 `log`/`warn`/`error`)
- `window`: `innerWidth`, `devicePixelRatio`, `screen`, `scrollTo`, `matchMedia`, `getComputedStyle`, `requestIdleCallback`, `IntersectionObserver`, `ResizeObserver`, `URL`, `URLSearchParams`, `TextEncoder`, `crypto`, `Intl`, `structuredClone`, `postMessage`, `dispatchEvent`
- 요소: `append`/`prepend`/`before`/`after`/`remove`/`replaceWith`, `insertAdjacentHTML`, `dataset`, `contains`, `getElementsByTagName`, `firstElementChild`, `outerHTML`, `hasChildNodes`, `getAttributeNames`, `focus`/`blur`/`click`, `scrollIntoView`, `animate`, `attachShadow`

있는 것: `querySelectorAll`, `getElementsByClassName`, `head`/`body`, `closest`, `children`, `nextElementSibling`, `style`, `classList`, `navigator`, `performance`, `MutationObserver`(stub), `CustomEvent`/`Event`, `atob`/`btoa`, 스토리지.

우선순위: `console.*` 나머지(no-op 이 아니라 `log` 로 보내기)·`getElementsByTagName`·`documentElement`·`currentScript`·`innerWidth`/`devicePixelRatio`·`matchMedia`(`@media` 평가와 같은 코드)·노드 편의 메서드(`append`/`remove`/`contains`)가 거의 모든 라이브러리 부팅 경로에 있다. 하나 없으면 그 스크립트가 거기서 끝나므로 영향은 "그 스크립트 전체".

#### 스크립트 에러 진단성

`<script>` 최상위 에러는 `state/mod.rs` 에서 `eprintln!("script error: {url}: {message}")` 로만 나간다.

1. **에이전트가 못 본다.** 리스너·타이머 에러는 Tier 1.5 #18 에서 `console.error("Uncaught in …")` 로 관찰에 올렸지만 최상위 스크립트 에러는 그 경로를 안 탄다 — naver 관찰의 `console:` 엔 `setPolicyInfo` 경고만 있고 두 에러는 없다.
2. **무엇이 없는지 안 나온다.** QuickJS 의 `TypeError` 메시지는 `not a function` 뿐이다. 호출 지점(파일:줄:열)은 `MB_JS_STACK=1` 일 때 스택으로만 나오고, 그걸로 원문을 찾아가야 이름을 알 수 있었다.

제안 (S): 최상위 스크립트 에러를 리스너 에러와 같은 `__mb_report_error` 로 보내 `console.error` 에 남기고, 메시지엔 스택 첫 프레임(`url:줄:열`)을 늘 붙인다(전체 스택은 지금처럼 `MB_JS_STACK`). 가능하면 `not a function` 을 호출 식 이름으로 보강 — QuickJS 가 이름을 안 주므로 원문 열 위치에서 직전 멤버 식을 잘라 붙이는 정도(`… (near "document.getElementsByTagName")`). 관찰의 trusted 블록이 아니라 page 블록 `console:` 줄이므로 페이지 문자열이 섞여도 위조 규칙과 충돌하지 않는다.

→ **3차 0a 에서 구현** ([compat-round3.md 0.5](compat-round3.md#05-스크립트-최상위-에러-위치-보고) "0a 구현 메모"): 최상위 에러도 `console.error("Uncaught in script: …")`, 형식 `{message} (near "x") at {url}:{line}:{col}`(쿼리 제거). naver 광고 두 건의 실측 확인은 0b 코퍼스 녹화 후.

### 네트워크 / 리소스

- ~~`src`/`href` 없음이 문서 로드 실패~~ — **고침 (C1)**.
- 실패한 하위 리소스는 0건(모두 200). 리소스 측 문제는 "받았는데 못 씀"(WebP, octet-stream) 쪽.

---

## 에이전트 관점 요약

| 사이트 | 사람이 기대하는 주요 컨트롤 | 에이전트가 찾나 (수정 후) |
|---|---|---|
| naver | 검색창·검색 버튼, 메뉴 바로가기, 뉴스 | 검색창 ✓ · 검색 버튼 ✗(JS 가 켬) · 나머지 ✗(모듈) |
| wiki | 검색, 본문, 목차, 언어 | 검색창·버튼 ✓ · 본문 전문 ✓ · 목차 링크 ✓ |
| google | 검색창, 검색 버튼 | ✓ (둘 다 + 음성·이미지 검색) |
| github | 파일 목록, README, Issues/PRs, Star | ✓ (이름에 카운터 중복) |
| hn | 기사 링크, 댓글, 로그인 | ✓ |

수정 전에는 google·github·wiki 가 **0개**(실패/패닉/멈춤)였다. 즉 이번 라운드에서 에이전트 쪽 가치는 대부분 C1~C4 에서 나왔고, 남은 CSS 갭은 주로 **사람이 보는 화면**(관전 창·스크린샷) 문제다. 예외는 `@media`(모바일 규칙의 `display:none`)와 `!important`(`display:none !important`) — 이 둘은 **어떤 요소가 보이는지**를 바꿔서 관찰에도 새어 나간다.

---

## 3차 코퍼스 기준선 (0b)

> 2026-11-26 녹화(사이트당 문서 1회 + 하위 리소스 1회, robots 준수, 2초 간격) · 2026-11-27 측정 · 엔진 = `15775d6`(0b, 수정 K1~K3) + **K4**(`0e0ccc9` — [compat-round3.md 부록 B](compat-round3.md#부록-b--0b-코퍼스에서-찾은-것-알려진-문제--담당)) · release · 1280×800 라이트 · 캐시 `compat-cache/net/` **동결**. 같은 캐시로 두 번 돌려 `load_ms` 외 모든 열·요소 목록이 **똑같음**을 확인(결정성). K4 수정 뒤 다시 돌린 `baseline-k4` 는 daum·hani·bbc 만 바뀌고(timeout → ok) 나머지 22곳은 `load_ms` 외 똑같다(`diff` 빈 결과).

도구·열 정의·분류표는 [compat-round3.md 0.2 "0b 구현 메모"·"0b 분류표"](compat-round3.md#0b-분류표-동결--바꾸려면-이-표를-먼저-고친다). 사이트 목록 정본은 [`scripts/compat-sites.tsv`](../scripts/compat-sites.tsv).

**요약** (25곳):

| 결과 | 곳 | 사이트 |
|---|---:|---|
| ok / ok* | 20 | (아래 표) — `ok*` 1곳: microsoft 오프라인 미스 2(css 1·js 1, 녹화 때 받기 실패로 보임) |
| timeout (120초) | 0 | 0b 때 3곳(daum·hani·bbc — `queueMicrotask` 가 core-js 폴리필 Promise 를 species 로 만들어 끝없는 마이크로태스크, 부록 B K4) → 수정 후 ok |
| nav-fail | 1 | crates.io — 우리 문서 요청에 404·빈 본문(`Accept` 헤더 없음 추정, K5) |
| blocked (403) | 2 | stackoverflow·w3.org — 챌린지 페이지, 우회 안 함 |
| skipped (robots) | 2 | 11st·old.reddit — `User-agent: *` 에 `Disallow: /` |
| panic | 0 | |

측정된 20곳 합계: **버린 선언 ours 14,112 / browser-too 256** · **버린 규칙 ours 10,451 / browser-too 58** · `Keyword` 폴백 676 · 파싱만 9,192 · 중첩 규칙 73(velog 만) · `@import` 5 · 건너뛴 at-rule 본문 2,709,648바이트(CSS 9.45MB 의 29%; hani 1.23MB·GitHub 1.17MB) · 슬리버 297 · `[mb-budget]` 0 (0b 의 17곳: 13,251/236 · 10,114/53 · 슬리버 249). browser-too 는 선언의 **1.8%** 뿐 — 버림은 거의 전부 우리 갭이다(`invalid-value` 의 상위는 shorthand 안 `var()`, 4자리 `#rgba`, `calc(var())` — 전부 Chrome 이 받는 값).

**2차 5곳**(naver·kowiki·google·github·hn) 새 캐시 합계: 선언 **ours 7,389 / browser-too 15**, 규칙 **ours 1,745 / browser-too 11**, 슬리버 17. 2차 수치(7,593 / 1,931 / 19)와 다른 날 스냅샷이라 직접 비교하지 않는다(측정 프로토콜) — 3차 목표 비율은 이 값 기준.

| 사이트 | 결과 | ms | 요소 | 텍스트 블록 | 버린 선언 ours/btoo | 버린 규칙 ours/btoo | Keyword 폴백 | 슬리버 | 기대 컨트롤 | 못 찾은 것 | 콘솔 에러 | 그중 스크립트 |
|---|---|---:|---:|---:|---|---|---:|---:|---|---|---:|---:|
| naver | ok | 75 | 9 | 2 | 453/5 | 822/5 | 6 | 0 | 1/5 | 검색 버튼, 메일·카페·뉴스 링크(ES 모듈) | 2 | 2 |
| daum | ok | 277 | 0 | 0 | 471/20 | 238/1 | 57 | 0 | 0/4 | 전부 — 본문을 JS 앱이 그리다 빠진 API 에서 죽음(K9) | 9 | 7 |
| yna | ok | 1,640 | 547 | 469 | 2,340/152 | 1,411/2 | 147 | 77 | 3/3 | | 13 | 12 |
| hani | ok | 220 | 216 | 187 | 225/0 | 90/4 | 18 | 0 | 2/3 | 경제 링크 | 3 | 1 |
| 11st | skipped | | | | | | | | | | | |
| kakaocorp | ok | 202 | 39 | 54 | 1,038/2 | 288/16 | 223 | 0 | 3/3 | | 6 | 6 |
| korea-kr | ok | 370 | 150 | 164 | 424/0 | 1,221/2 | 5 | 36 | 1/2 | 검색창 | 33 | 32 |
| kowiki | ok | 197 | 753 | 473 | 44/0 | 25/0 | 2 | 0 | 3/3 | | 1 | 1 |
| velog | ok | 73 | 8 | 4 | 69/39 | 3/0 | 5 | 0 | 1/2 | 로그인 버튼(SPA) | 2 | 2 |
| d2 | ok | 208 | 87 | 175 | 159/11 | 973/3 | 6 | 0 | 0/1 | helloworld 링크 | 1 | 1 |
| google | ok | 159 | 18 | 7 | 341/4 | 90/0 | 25 | 0 | 2/3 | Gmail 링크 | 6 | 6 |
| enwiki | ok | 461 | 932 | 762 | 218/0 | 188/3 | 6 | 69 | 2/3 | Log in 링크 | 1 | 1 |
| mdn | ok | 108 | 459 | 476 | 153/0 | 194/0 | 3 | 0 | 3/3 | | 0 | 0 |
| docsrs | ok | 128 | 105 | 188 | 284/1 | 1,979/7 | 17 | 3 | 3/3 | | 3 | 3 |
| rustlang | ok | 79 | 37 | 68 | 175/5 | 10/3 | 38 | 9 | 3/3 | | 0 | 0 |
| cratesio | nav-fail | 19 | 0 | 3 | — | — | — | — | 0/2 | (오류 페이지) | 0 | 0 |
| github | ok | 1,562 | 165 | 225 | 6,533/0 | 808/6 | 80 | 17 | 3/3 | | 0 | 0 |
| stackoverflow | blocked | 18 | 0 | 3 | — | — | — | — | 0/2 | (403) | 0 | 0 |
| hn | ok | 51 | 225 | 95 | 18/6 | 0/0 | 0 | 0 | 4/4 | | 0 | 0 |
| bbc | ok | 266 | 114 | 214 | 165/0 | 9/0 | 3 | 48 | 3/3 | | 15 | 15 |
| old.reddit | skipped | | | | | | | | | | | |
| apple | ok | 312 | 143 | 154 | 733/3 | 1,849/4 | 16 | 17 | 3/3 | | 4 | 4 |
| microsoft | ok* | 116 | 63 | 68 | 266/8 | 253/2 | 18 | 20 | 2/2 | | 4 | 4 |
| example | ok | 11 | 0 | 1 | 3/0 | 0/0 | 1 | 1 | 0/0 | (오늘 판은 링크 없음) | 1 | 1 |
| w3c | blocked | 20 | 0 | 3 | — | — | — | — | 0/2 | (403) | 0 | 0 |

**0c 재측정** (2026-11-27, 라벨 `0c`, 같은 동결 캐시·release — 0c 엔진 수정 뒤, [compat-round3.md 부록 C](compat-round3.md#부록-c--0c-퍼저에서-찾은-것)): 결과 분포·버린 선언·버린 규칙·요소 수·기대 컨트롤은 25곳 모두 위 표와 **같다**, `scripts/compat-corpus.sh diff baseline-k4 0c` 의 요소 목록 차이 **없음**(에이전트가 보는 역할·이름 목록 동일). 바뀐 것은 기하뿐: 슬리버 **297 → 275**(korea-kr 36→25, github 17→10, enwiki 69→66, rustlang 9→7, apple 17→18; 2차 5곳 17→10), 문서 높이 hani 27,637→27,183 · google 901→753 · rustlang 10,199→9,825 — flex 안 인라인 내용 항목이 내용 폭으로(F11)·인라인 상자 안 % 기준(F3) 때문으로 보이나 사이트별 원인과 Chrome 대조는 안 했다. timeout·panic 0, `[mb-budget]` 0.

**R2 재측정** (2026-11-27, 라벨 `r2`, `r3` 대비 — 경계 잎 여백 이중 차감·flex/grid 항목 속성 수정, [compat-round3.md 부록 C R2](compat-round3.md#부록-c--0c-퍼저에서-찾은-것)): 결과·버림·요소 수·요소 목록 25곳 동일, 관찰이 바뀐 곳은 github `page_height` 8,229 → 8,277 하나. 위 표의 수치는 그대로 유효.

**2.1 재측정** (2026-11-27, 라벨 `media-before` → `media-after`, 같은 동결 캐시·release — `@media` 평가 + `<link media>`/`<style media>`, [compat-round3.md 2.1 구현 메모](compat-round3.md#21-구현-메모-2026-11-27--설계와-달라진-점-포함)): 결과 분포·버린 선언·버린 규칙·오프라인 미스·기대 컨트롤은 25곳 모두 위 표와 **같다**(`media-before` 는 probe 만 바꾼 옛 엔진 — 요소 목록이 `r4` 와 동일함을 `diff` 로 확인). 20곳 `@media` 블록 3,789·안의 규칙 16,220 중 1280×800 에서 적용 **5,754**(전: 전부), "wrong at 1280x800" 범주 규칙 적용 9,223 → 1(휴리스틱 오분류 — 실제로 참). 요소 수 20곳 4,070 → 3,773, 사이트별: yna 547→381 · enwiki 932→678 · bbc 114→183 · github 165→182 · kowiki 753→767 · microsoft 63→73 · hani 216→222 · apple 143→149 · d2 87→89 · docsrs 105→104(나머지 같음). 슬리버 275 → 984(enwiki 66→552·kowiki 0→203 — Vector 2022 데스크톱 격자가 적용되는데 `grid-template`·`minmax()` 를 못 읽어 본문 열이 0폭, 5단계 몫; d2 0→20·apple 18→27·github 10→15·hani 0→5, yna 77→57; 2차 5곳 10→218). 문서 높이: enwiki 17,625→35,445 · kowiki 10,855→16,345 · hani 27,183→11,898 · hn 1,377→1,020 · kakaocorp 2,246→3,046 · mdn 7,215→5,791 · github 8,277→9,205. 사라진/나타난 요소의 사이트별 판정은 [compat-round3.md 진행 표 2행](compat-round3.md#진행-표). `load_ms` 는 yna 1,413→918(적용 규칙이 1/30). timeout·panic 0, `[mb-budget]` 0.

**2.1a 재측정** (2026-11-27, 라벨 `grid`, `media-after` 대비 — `grid-template` 단축·`minmax()`·레거시 그리드 간격과 영역 폭 배치·다중 열 표 셀, [compat-round3.md 2.1a](compat-round3.md#21a-위키-그리드-회귀-수정-2026-11-27)): 결과·기대 컨트롤·오프라인 미스는 25곳 동일. 요소 목록이 바뀐 곳은 enwiki +484(678→1,162, 2.1 에서 빠졌던 navbox 링크 복귀)·kowiki +230(767→997, navbox 링크 — 2.1 전에도 colspan 표 버그로 빠져 있다가 처음 보임) 둘뿐. 슬리버 984 → 325(enwiki 552→71 · kowiki 203→0 · mdn 0→25 — mdn 은 사용자 속성 트랙·calc 곱셈 미디어 쿼리 때문, 1·5단계 몫; 2차 5곳 218→15). 버린 선언 ours 14,112 → 14,078. `[mb-budget]` 0.

**1단계 P "전" 측정** (2026-11-27, 라벨 `step1-before`, `grid` 대비 — probe 에 계산값 시점 열만 추가, 엔진 `873808c`, [compat-round3.md 1단계 구현 메모](compat-round3.md#1단계-구현-메모-2026-11-27)): 기존 열은 25곳 모두 `load_ms` 외 `grid` 와 같다. 새 열 20곳 합계: IACVT **3,447**(github 1,962 · yna 960 · kakaocorp 172 · naver 93 · google 88 · docsrs 75 · mdn 63 · apple 30 · velog 2 · enwiki 2) · var 폴백 0 · 합산 지표 `decl_drop_total`(ours + IACVT + `Keyword` 폴백 + var 폴백) **18,204**(2차 5곳 9,627). IACVT 의 약 2/3 는 `:root` 가 `<head>` 에만 맞는 기존 버그(`<html>` 요소가 DOM 에 없음) 때문 — 임시 실험에서 1,139 로 줄었다.

**1r 재측정** (2026-11-27, 라벨 `html-root`, `step1-before` 대비 — `<html>` 을 문서 요소로, [compat-round3.md 1r](compat-round3.md#1r--html-을-문서-요소로-2026-11-27)): 결과·기대 컨트롤·오프라인 미스·버린 선언(ours)은 25곳 동일, `[mb-budget]` 0. IACVT 20곳 3,447 → **318**(github 1,962→28 · yna 960→0), 합산 지표 18,204 → 15,075(2차 5곳 9,627 → 7,612). 요소 목록 diff: github +1(헤더 로고 링크) · enwiki −3(`.client-nojs` 가 숨기는 JS 전용 컨트롤 — 우리 `document.cookie` 가 없어 시작 스크립트가 `client-js` 로 못 바꿈). 문서 높이 11곳 변화(apple 26,617→4,271 `html.no-js` 미디어 숨김 · rustlang 9,772→4,507 `html{font-size:62.5%}` rem 기준 등).

**1a 재측정** (2026-11-27, 라벨 `important`, `html-root` 대비 — `!important` 파싱·캐스케이드, [compat-round3.md 1a](compat-round3.md#1a--important-2026-11-27)): 버림 사유 `important` 5,097 → 0, 버린 선언 ours 20곳 14,078 → 9,643(2차 5곳 7,371 → 4,387), 합산 지표 15,075 → 10,680(5곳 7,612 → 4,645). `[mb-budget]` 0. `ok*` 2곳 늘어남(github 스피너 svg 2·yna 배경 jpg 1 — 살아난 `background-image` 의 새 요청, 보충 녹화 안 함). 요소 목록 diff: yna −4·microsoft −2(`display:none!important` 유틸리티), docsrs −1(`<noscript>` 안 시트를 적용하는 기존 버그), enwiki +3(Log in·Create account·Donate — 기대 컨트롤 3/3, 단 헤더가 1,292px 로 커진 레이아웃 이상: 레거시가 % 높이를 부모 폭으로 푸는 기존 버그, 3단계).

**1p 재측정** (2026-11-28, 라벨 `pct-height`, `important` 대비 — % 높이를 확정 포함 블록 높이 기준으로, [compat-round3.md 1p](compat-round3.md#1p--퍼센트-높이-기준-2026-11-28)): 결과·버림·기대 컨트롤·오프라인 미스 25곳 동일, `[mb-budget]` 0. 요소 목록 diff 는 docsrs +15(본문의 `<a><code>` 문서 링크 — 0 높이로 빠져 있었음)뿐. enwiki 헤더 1,292 → 124px. 문서 높이 14곳 변화 — `html, body {height:100%}` 인 velog·enwiki·d2·google 은 루트 상자가 뷰포트(800)가 되어 관찰의 `page_height` 가 스크롤 길이를 못 나타냄(d2 5,376 → 800; `scrollHeight` 식 `page_height` 권고), 나머지는 폭 기준이던 이미지·카드 래퍼 `height:100%` 가 바뀐 것(yna 1,304 → 16,996 · bbc 26,042 → 6,670 등). 슬리버 319 → 340(블록화된 absolute sr-only 라벨이 1px 폭을 지킴 — 3단계 클리핑 대상).

- `ms` = `navigate`(settle 포함) 벽시계, 이 맥에서. 두 실행 사이 차이는 대부분 ±10% 안, 가장 큰 것은 kowiki 256 → 197(−23%)·github 1,829 → 1,562(−15%) — 시간은 비교용이 아니라 timeout 판정용.
- 기대 컨트롤 **누락 사이트 8곳**(naver·daum·hani·korea-kr·velog·d2·google·enwiki; 0b 때는 측정된 17곳 중 6곳). 원인 분류는 각 단계에서 — 지금 표는 기준선. naver 검색창은 ARIA `combobox` 다(역할 오기를 녹화 후 tsv 에서 고침).
- **콘솔 에러**(`prompts.rs` "제3자 스크립트 콘솔 에러" 결정용 입력): 측정된 20곳 중 **16곳이 1건 이상**, 합 104건 중 98건이 `Uncaught in script`. 많은 곳 korea-kr 33·bbc 15·yna 13·daum 9·google 6·kakaocorp 6. 대부분 빠진 API(`getElementsByTagName` 류, [JS API 표면 프로브](#js-api-표면-프로브)). naver 광고 2건은 이번 스냅샷에서 `near "document.getElementsByTagName"` / `near "Na"`(gfp-core 의 축약 별칭 — 2차의 `console.debug` 가 이번 빌드에선 지역 변수로 줄여져 있음)로 읽힌다. 0a 권고(지금은 `prompts.rs` 를 안 바꿈)를 바꿀 근거는 아직 없다 — 다음은 제3자 에러가 있는 손으로 쓴 eval 페이지로 확인.
- 오류 페이지 3곳(crates.io·stackoverflow·w3c)의 텍스트 블록 3 = `load failed` + 사유 + URL — 0b 이전엔 0(K2).

---

## 호환성 3차 제안 순서

> **확정판은 [compat-round3.md](compat-round3.md)** (2026-11-26): 아래 제안에서 0단계(견고성 점검 + 스크립트 에러 위치)를 앞에 추가하고, 1~5단계(값 파서 → `@media`/`@layer`/`@supports` → 클리핑 → 셀렉터 → `calc`/트랙; 실행은 `@media` 평가를 값 파서보다 먼저)만 3차 범위로, 이미지·JS 소소·ES 모듈·`::before` 는 4차 이후로 뺐다. 아래 목록은 조사 당시 제안으로 남겨 둔다.

영향(사이트 수 × 심각도) ÷ 노력. 묶음 단위로 한 커밋씩.

1. **`!important` + 값 파서 묶음** (S~M, 5/5) — `!important` 파싱 + cascade 중요도 층, `border`/`border-*` shorthand, 쉼표 목록(첫 항목/전체 보관), 커스텀 속성 원문 보관(`--x: 1,2,3`) → `rgb(var())`, `flex: a b auto`, `background-size`. 테스트는 속성마다 한 줄 CSS. 끝나면 `compat_probe` 로 "버린 선언" 수가 얼마나 줄었는지 재측정.
2. **`@media` 평가 → `@layer` 펼치기** (M + S, 5/5) — 위 표의 조건 집합 + 범위 문법. GitHub 다크 모드가 풀리는지로 확인. 결정성: 평가 입력은 뷰포트·테마(라이트 고정)뿐. 그다음 `@layer`(GitHub CSS 46%) 와 `@supports` 를 펼친다 — 순서가 반대면 GitHub 이 더 어두워진다.
3. **overflow 클리핑** (M, 4/5) — sr-only 세로 글자 제거, 캐러셀·잘린 목록. 에이전트 관찰의 "화면 안" 판정에도 클립을 반영할지는 그때 결정(현재는 a11y 처럼 포함이 맞음).
4. **셀렉터** (S) — `:is()/:where()` 켜기, `:focus-visible`→focus, `:focus-within`, `:checked/:disabled/:enabled`. 규칙 수백 개가 살아난다.
5. **`calc()`/`min()`/`max()` + 값 속 `var()`** (M) — GitHub 간격. `minmax()`/`repeat()` 트랙도 여기서(위키 레이아웃).
6. **이미지** (S) — 바이트 스니핑, WebP feature.
7. **JS 표면 API + 에러 진단성** (S) — 먼저 최상위 스크립트 에러를 `console.error` + `url:줄:열` 로(이게 있어야 나머지 갭을 사이트마다 바로 읽는다), 그다음 [프로브](#js-api-표면-프로브) 목록의 상위(`console.*`, `getElementsByTagName`, `documentElement`, `currentScript`, `innerWidth`, 노드 편의 메서드), `document.cookie`(메모리), Google 에러 재분류. naver 광고 SDK 2개가 확인용.
8. **ES 모듈 = Tier 2** (L) — naver 본문. spa-roadmap 의 "보류" 결정을 이 조사 근거로 다시 본다: 5곳 중 1곳이지만 국내 최대 포털이고 조사 계기가 된 사이트.
9. **`::before/::after` 생성 콘텐츠** (L) — 아이콘·구분선. 시각만.

각 단계 후 같은 캐시로 `compat_probe` 를 다시 돌려 이 문서의 수치를 갱신한다(사이트 재방문 없음).

## 미분류 — 후속 조사

- GitHub 의 폭 187,319,520px 상자 (어떤 요소인지, `calc` 버림 → `width: 100%` 누적?).
- Google 인라인 스크립트 에러 5종 (`'addEventListener' of undefined`, `'ved' of undefined`, `'ssdi' of undefined`, `not a function` ×2).
- Google 키보드 아이콘이 거대 — 배경 이미지 크기 규칙.
- JS 가 나중에 `src` 를 채운 `<img>` 를 받는 경로가 있는지.
- naver `#u_skip` 링크가 화면 위(-30px)로 — 의도된 숨김(`top:-…`)이라 문제 아님, 확인만.
