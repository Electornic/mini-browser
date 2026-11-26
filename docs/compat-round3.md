# 호환성 3차 설계 — 견고성 + CSS 파서·캐스케이드

> 상태: **설계 확정 (2026-11-26, 1차 검토 + 다각도 검토 — 코드 대조·웹 표준·실행/측정 — 반영), 구현 전** · 근거 = [compat-survey.md](compat-survey.md) (호환성 2차 실사이트 조사) · 기준선 `4588aba` 804 → 조사 캐시 2차 수정 후 **806 green** · 진행은 [진행 표](#진행-표)

이 문서만 읽고 어느 단계든 이어받을 수 있게 쓴다. 단계마다 **목표 → 세부 항목 → 설계 결정 → 테스트 → 측정·완료 기준 → 위험 → 노력** 순서다. 2차 조사의 수치·용어(슬리버, 버린 선언, `compat_probe`, `MB_NET_CACHE`)는 [compat-survey.md 방법](compat-survey.md#방법) 참고.

## 범위와 순서

| 단계 | 내용 | 왜 이 순서 | 노력 |
|---|---|---|---|
| **0** | 견고성 점검(불변식·코퍼스·퍼징·`?`/unwrap 검토) + 스크립트 에러 위치 | 2차 버그 6개 중 4개가 "페이지 전체 실패/패닉/무한 시간". CSS 를 고치기 전에 같은 종류가 더 없는지 체계적으로 걸러야 이후 측정이 의미 있다. 에러 위치는 이후 모든 진단을 빠르게 한다. 하위 커밋 **0a**(에러 위치·불변식·코드 검토) → **0b**(코퍼스) → **0c**(퍼저) | M (3~4일) |
| **1** | `!important` + 값 파서 묶음 | 5/5 사이트, 버린 선언의 대부분. 가장 싸고 넓다 | M (3~4일) |
| **2** | `@media` 평가 → `@layer` 펼치기 → `@supports` 펼치기 | 5/5, GitHub 다크 모드. `@layer` 는 반드시 `@media` 평가 **뒤** ([이유](#22-layer-펼치기)). **2.1 은 1단계보다 먼저 실행**(아래 실행 순서) | M (3~4일) |
| **3** | overflow 클리핑 (+ 퍼센트 높이 기준 바로잡기) | 4/5 사이트의 sr-only 세로 글자 | M (3~4일) |
| **4** | 셀렉터 `:is/:where`, `:focus-visible`, `:focus-within`, `:checked/:disabled/:enabled`, `::before/::after` 를 "유효·매칭 없음"으로 수용 | 버려진 규칙 수백 개. 작은 일 | S (1~1.5일) |
| **5** | `calc()/min()/max()` + 값 속 `var()`, `minmax()`/`repeat()` 트랙 | GitHub 간격·위키 그리드. 1단계의 "미해결 값" 장치 위에 얹는다 | M (4일, taffy calc 스파이크 포함) |

**실행 순서**: **0 → 2.1(`@media` 평가) → 1 → 2.2·2.3 → 3 → 4 → 5.** 번호는 문서 구조용이고 실행은 이 순서다. 이유: 1단계에서 `!important` 가 살아나면 `@media` 안의 반응형 유틸리티(Primer `.hide-sm{display:none !important}`, `.d-md-none` 류)가 **모든 폭에서** 적용되어 GitHub·위키 요소가 대량으로 숨는다. 그러면 1단계 완료 기준(기대 컨트롤 발견)이 1단계 버그가 아니라 미디어 미평가 때문에 실패해 측정이 오염된다. 2.1 은 `!important` 와 독립이라 앞으로 당길 수 있고, 2.2 의 레이어 중요도 반전은 1단계의 두 패스 위에 얹히므로 1단계 뒤에 둔다.

**범위 밖 (4차 이후)**: `@import`(현재 **미구현** — `layer()`/`supports()`/미디어 조건 포함; probe 가 사이트별 `@import` 수·바이트를 세어 기록만), 네이티브 CSS 중첩(`&`, 안쪽 `@media` — probe 로 빈도만 측정), `@container`, ES 모듈 = Tier 2 (naver 본문 — 별도 설계 문서), 이미지(WebP·바이트 스니핑), `::before/::after` **생성 콘텐츠**(셀렉터 수용은 4단계 — 규칙을 살리는 것까지만), 에러 위치 외의 작은 JS API(`getElementsByTagName`, `console.debug` 등 — [survey 프로브 목록](compat-survey.md#js-api-표면-프로브)).

**공통 규칙** (단계마다 반복하지 않음):
- 실패하는 테스트 먼저. 픽스처는 손으로 쓴 한 줄 CSS / 몇 줄 HTML. 실사이트 원문은 저장소에 절대 안 넣는다(저작권, 공개 저장소) — 원문은 git-ignored `compat-cache/` 에만.
- 단계 하나 = 커밋 하나(필요하면 하위 커밋). `cargo test --workspace` green + `cargo clippy --workspace --all-targets` clean + `cargo fmt`. 테스트 수 변화는 커밋 본문에.
- 결정성 유지(AGENTS.md): 새 입력은 뷰포트·테마(라이트 고정)·엔진 시계뿐. 벽시계·HashMap 순서 금지. 예외는 0단계 워치독의 시간 측정 — [0.1 참고](#01-불변식을-규칙으로).
- 단계 끝마다 같은 캐시로 코퍼스 전체 `compat_probe` 재실행 → [진행 표](#진행-표) 와 `compat-survey.md` 수치 갱신. 사이트 재방문 없음.
- **측정 프로토콜**(수치를 서로 비교 가능하게):
  - 0b 에서 새로 녹화한 캐시를 **3차 동안 동결**한다(보충 녹화는 아래 0.2 규칙으로만). 2차 수치(7,593 / 1,931)는 다른 날의 스냅샷이라 직접 비교하지 않는다 — **새 기준선** = 새 캐시 + 기준 엔진(0a 직후 커밋) + 0b probe 로 다시 잰 값. 목표는 비율(−80%, −95%)로 쓰고, 2차 절대값은 참고로만.
  - 단계마다 ① probe 갱신 커밋(새 열·분류) → 옛 엔진으로 **전** 측정 → ② 엔진 커밋 → **후** 측정. probe 와 엔진을 한 커밋에 섞지 않는다. 단계 절마다 "probe 변경" 항목을 둔다.
  - 코퍼스 판정·시간 측정은 **release 빌드**(`cargo build --release --example compat_probe`). debug 는 panic·nav-fail 만 본다.
  - 결과에 `offline-miss`(캐시 미스, 종류별)·하위 리소스 실패 수 열. 미스가 있으면 판정을 `ok*` 로 표시 — 엔진이 바뀌어 새 요청(살아난 `url()`, 캐시 버스터)을 보내면 0.1 규칙 1 때문에 조용히 `ok` 가 되므로.
  - 버림 수치는 파서 버림 + **치환 후 무효(IACVT)** 선언 수 + 전용 파서가 있는 속성의 `Keyword` 폴백 수를 같은 표에 둔다 — `var()` → `Unparsed` 로 실패가 계산값 시점으로 옮겨 가 지표가 부풀려지는 것을 막는다. `transition`/`animation` 같은 "파싱만" 속성은 별도 열(버림에서 빠진 이유가 보이게).
  - ours/browser-too 분류표는 0b 에서 고정하고, 바꾸려면 이 문서를 먼저 고친다.
- **에이전트 영향 기준**(1~5단계 공통, "컨트롤 수 감소 없음" 대신):
  - 사이트별 **기대 컨트롤 목록**(역할 + 이름 부분 일치 — 검색창·검색 버튼·주 내비·본문 링크 몇 개)을 `scripts/compat-sites.tsv` 에 **커밋**한다(짧은 UI 라벨이라 저작물 아님). 기준 = 기대 컨트롤 **전부 발견**.
  - probe 가 단계 전후 관찰의 사라진/새 요소 목록을 자동으로 낸다(관찰 텍스트 두 개의 요소 줄 diff — `compat_probe` 는 mb-agent 예제라 mb-mcp 의 `replay` 차이 계산을 쓸 수 없다(의존 방향). 필요하면 그 함수를 mb-agent 로 내려 공유). 사라진 것마다 정상(모바일 메뉴·`!important` 숨김)/버그 판정을 진행 표 메모에.
  - 사람 비교용 실제 Chrome 스크린샷은 **녹화 시점에** `compat-cache/chrome/<사이트>.png` 로 같이 남긴다(오늘의 라이브 사이트와 캐시 스냅샷이 달라지므로).
- **손으로 쓴 3차 골든 + eval**: 0a 에서 `mb-agent/tests/fixtures/compat3.html`(sr-only 건너뛰기 링크, `@media` 안 `!important` 반응형 숨김, 모바일 `@media`, 체크박스 해킹 메뉴, `overflow:hidden` 캐러셀)과 그 골든을 추가하고 단계마다 diff 로 추적한다. 지금 골든들은 author CSS 가 거의 없어 3차 회귀를 **못 잡는다**. 같은 페이지로 `eval/scenarios/` 에 "주 메뉴를 펼쳐 X 로 이동" 시나리오 1개.
- **"버린 선언·규칙" 수치는 `ours` 만 목표로 센다.** 실제 브라우저도 버리는 것(다른 엔진 접두사 `-moz-`/`-ms-` 속성·의사 클래스, 문법이 틀린 값, 모르는 속성)은 `browser-too` 범주로 따로 세어 목표에서 뺀다. 판정은 probe 의 분류표(접두사 + `css/properties.rs` 의 "아는 속성" + 버림 사유 코드)로, 애매하면 `ours`. 지금 `css/report.rs` 는 원문 발췌만 남기고 사유 코드가 없어 0b 에서 추가한다(`UnknownProperty`·`MultiToken`·`Important`·`UnsupportedSelector`·`SkippedAtRule` 정도). 0단계에서 probe 에 이 분류를 넣고 2차 기준선(7,593 / 1,931)도 둘로 나눠 다시 적는다.
- 골든(`mb-agent/tests/fixtures/*.observation.txt`) 이 바뀌면 `UPDATE_GOLDEN=1` 후 diff 를 눈으로 보고 커밋 본문에 **왜** 바뀌었는지.

---

## 0단계 — 견고성 점검 + 스크립트 에러 위치

### 목표

"전 엔진 감사"가 필요한가 → 한 줄씩 읽는 감사는 아니고, **체계적으로**. 2차에서 고친 6개 중 에이전트를 막은 4개는 세 종류였다:

| 종류 | 2차 사례 |
|---|---|
| **A. 입력 하나가 이동 전체를 실패시킴** | `src` 없는 `<img>`, `href` 없는 `<link>` (Google·GitHub 빈 페이지) |
| **B. 패닉** | grid 영역 > 트랙 → 인덱스 초과 (위키) |
| **C. 끝없는 시간** | 인라인 측정 2^깊이 (위키 몇 분 멈춤) |

이 세 종류를 (a) 규칙으로 쓰고, (b) 실사이트 코퍼스, (c) 퍼징, (d) 표적 코드 검토 네 방향에서 찾는다. 그리고 (e) 스크립트 최상위 에러를 `url:줄:열` 로 보고해 이후 진단을 빠르게 한다.

### 0.1 불변식을 규칙으로

AGENTS.md "Agent-track Rules" 옆에 **"Robustness Rules"** 절을 추가한다(이 단계의 산출물):

1. **하위 리소스·요소 하나가 이동 전체를 실패시키지 않는다.** 문서 로드가 실패할 수 있는 건 문서 응답 자체(네트워크 에러, 상태 코드, 지원 안 하는 Content-Type, 인코딩)뿐. CSS·스크립트·이미지·폰트·파비콘·속성 값 하나의 실패는 그 리소스만 건너뛴다. 코드 규칙: `navigation.rs`/`resource.rs` 의 리소스 수집 경로에서 `?` 로 `ResourceError` 를 올리지 않는다(이미 `resolve_attr` 로 정리됨 — 남은 `?` 는 0.4 에서 제거하고 함수 시그니처를 `Vec<…>` 로 바꿔 다시 생기지 않게).
2. **어떤 입력에도 패닉하지 않는다.** HTML·CSS·JS·이미지 바이트는 전부 신뢰할 수 없는 입력. 레이아웃·스타일·페인트에서 인덱싱은 `get()`, 나눗셈은 0 검사, `unwrap()` 은 불변식이 코드로 보장될 때만(주석으로 이유). 최후 방어선은 이미 있다 — `mb-mcp/src/worker.rs` 의 `catch_unwind` 가 패닉 시 브라우저를 재생성. 하지만 그건 **세션 손실**이라 방어선이지 해결책이 아니다.
3. **스타일·레이아웃은 시간 예산 안에서 끝난다.** 노드 수 n 에 대해 다항(목표: 거의 선형) — 자식을 두 번 훑는 재귀 금지(2차 C3), 같은 서브트리를 다시 재는 측정은 메모하거나 한 번에. 예산 초과는 **감지하고 보고**한다(아래 워치독).

**워치독 (C 종류 감지)**: `BrowserState::display_list` 의 스타일+레이아웃 구간을 `std::time::Instant` 로 재서 예산(기본 2초, **release 빌드 기준** — debug 에선 GitHub 이 2차에 이미 21초라 의미 없음; debug 빌드는 예산 ×10) 초과 시 `eprintln!("[mb-budget] style+layout took {ms}ms on {url_without_query} ({n} nodes, {us_per_node}µs/node)")` 한 줄. 노드당 µs 를 같이 찍어 기계 속도와 페이지 크기를 분리한다. **결정성 예외**: 이 측정은 stderr 진단에만 쓰이고 관찰·행동·정책 출력엔 절대 안 들어간다(AGENTS.md 결정성 규칙의 명시적 예외로 문서화). 중단(abort)은 하지 않는다 — 레이아웃을 중간에 끊으면 반쪽 화면이 더 혼란스럽고, 끊을 지점을 만드는 비용이 크다. `compat_probe` 는 이 줄과 별개로 전체 로드 시간을 재서 타임아웃(아래 0.2) 판정에 쓴다.

### 0.2 코퍼스 점검

**사이트 선정 기준** (20~30곳):
- 로그인 없이 보이는 공개 첫 화면 또는 대표 문서 1장.
- 범주를 고르게: 국내 포털·뉴스·쇼핑·공공·커뮤니티, 해외 검색·백과·개발 문서·코드 호스팅·뉴스·SPA 프레임워크 사이트, 그리고 기준선용 초간단 페이지.
- `robots.txt` 가 그 경로를 막는 사이트는 넣지 않는다 — `compat-corpus.sh record` 가 사이트마다 robots.txt 를 **직접 1회** 받아 판정(허용/금지)과 날짜를 report 에 적고, 금지면 그 사이트를 건너뛴다. 봇 차단(403·챌린지 페이지)은 "차단" 으로 기록만 하고 우회하지 않는다.
- **녹화 중 부작용 최소화**: 페이지 스크립트가 실행되므로 광고·분석 비콘이 실제로 나간다(제3자에 가짜 노출). record 모드는 **GET 만** 허용하고 POST·`sendBeacon`·`fetch(method≠GET)` 는 네트워크로 보내지 않고 실패시킨다(`MB_NET_CACHE` 가 켜져 있고 오프라인이 아닌 녹화 중에는 캐시 계층이 항상 이렇게 한다 — 끄는 스위치 없음; 캐시는 조사 전용이라 일반 사용엔 영향 없음). 이 차이로 일부 스크립트 경로가 달라지는 것은 허용.

**초안 목록** (확정은 0단계 시작 때 robots 확인 후; 바뀌면 이 표를 고친다):

| 범주 | 사이트 |
|---|---|
| 국내 포털·뉴스 | www.naver.com, www.daum.net, www.yna.co.kr, www.hani.co.kr |
| 국내 쇼핑·서비스 | www.11st.co.kr, www.kakaocorp.com |
| 국내 공공·문서 | www.korea.kr, ko.wikipedia.org/wiki/웹_브라우저 |
| 국내 개발 | velog.io (SPA), d2.naver.com |
| 해외 검색·백과 | www.google.com, en.wikipedia.org/wiki/Web_browser |
| 해외 개발 문서 | developer.mozilla.org/en-US/docs/Web/HTML, docs.rs/tokio, www.rust-lang.org, crates.io (SPA) |
| 코드 호스팅·Q&A | github.com/tokio-rs/tokio, stackoverflow.com 질문 1장 |
| 해외 뉴스·커뮤니티 | news.ycombinator.com, www.bbc.com/news, old.reddit.com |
| 기업·마케팅 (CSS 무거움) | www.apple.com, www.microsoft.com |
| 기준선 | example.com, www.w3.org |

봇 차단이 예상되는 곳(쿠팡·나무위키 등)은 **넣지 않는다** — 차단 페이지는 엔진 정보가 없고 녹화가 차단 회피처럼 보일 수 있다. 녹화 중 예상 밖으로 차단되면 `blocked` 로만 남긴다.

**예의**: 녹화는 사이트당 문서 1회 + 하위 리소스 1회씩, 사이트 사이 2초 간격, 순차. UA 는 엔진 기본(데스크톱 Chrome 문자열) — **결정 기록**: 식별 토큰(`… mini-browser-compat`)을 붙이면 사이트가 다른 내용(봇용 페이지)을 줄 수 있어 측정 목적에 반하므로 붙이지 않는다. 대신 방문 횟수를 1회로 묶고 robots 를 지키는 것으로 예의를 갖춘다. **보충 녹화**(엔진이 새 요청을 내기 시작해 `offline-miss` 가 생길 때): 단계당 최대 1회, 같은 사이트의 GET 하위 리소스만, 같은 간격. 이후 모든 실행은 `MB_NET_CACHE_OFFLINE=1`. 2차 캐시는 요청 정보 줄이 없어(`4588aba`) 전부 미스이므로 원래 5곳도 이때 한 번 다시 녹화된다.

**도구**: `scripts/compat-corpus.sh` (신규) — 사이트 목록 `scripts/compat-sites.tsv`(이름·URL·기대 컨트롤, **커밋됨** — 이 문서 표는 요약, tsv 가 정본)를 돌며:

```sh
# 녹화 (한 번)
MB_NET_CACHE=$PWD/compat-cache/net scripts/compat-corpus.sh record
# 재생 (몇 번이든)
MB_NET_CACHE=$PWD/compat-cache/net MB_NET_CACHE_OFFLINE=1 scripts/compat-corpus.sh run
```

`run` 은 사이트마다 `timeout 120 compat_probe` 를 돌리고(이 맥은 `/usr/bin/timeout` 있음 확인; 없는 환경은 `gtimeout` → 둘 다 없으면 스크립트가 에러로 멈춤 — 조용히 상한 없이 돌지 않게) 종료 코드·stderr 로 판정해 `compat-cache/corpus-report.tsv` 한 줄씩: `이름 · 결과(ok/nav-fail/panic/timeout/blocked) · 로드 ms · 요소 수 · 텍스트 블록 · 버린 선언(ours/browser-too) · 버린 규칙(ours/browser-too) · 슬리버 · [mb-budget] 줄 수 · 스크립트 에러 수`. `compat_probe` 는 이를 위해 `summary.txt` 외에 기계용 `summary.tsv` 한 줄을 추가로 쓴다. 패닉은 별도 프로세스라 종료 코드 101 로 잡힌다. probe 의 settle 대기(20초 벽시계, `compat_probe.rs` 124행)가 끝까지 간 경우는 `settle-timeout` 으로 따로 기록(느린 사이트는 기계 속도에 따라 관찰이 달라지므로). 결과 열에 `offline-miss` 수도.
- **안전장치**: `MB_NET_CACHE` 는 절대 경로만 받고, 캐시 디렉터리가 없거나 비었으면 `run` 은 즉시 에러(새 클론·워크트리에서 30곳 전부 `nav-fail` 로 찍히는 것 방지). 출력 경로가 `git check-ignore` 로 무시 대상인지 확인하고 아니면 멈춘다(실사이트 원문이 커밋되는 사고 방지).

**판정**: 모든 `nav-fail`/`panic`/`timeout` 은 원인 확인 → 손으로 쓴 최소 재현 → 실패 테스트 → 수정. `blocked` 는 표에 사유만.

### 0.3 퍼징

**결정**: `cargo-fuzz` 는 nightly 가 필요한데 툴체인이 1.94.1 stable 로 고정이다. 그래서 **새 의존성 없는 결정적 생성기**를 저장소 안에 둔다. `cargo-fuzz` 는 나중에 커버리지 유도가 필요해지면 `fuzz/` 디렉터리로 별도 추가(워크스페이스 밖, CI 밖).

- **생성기** `crates/mb-engine/tests/fuzz_layout.rs`: 시드 고정 xorshift 로 HTML+CSS 를 구조적으로 만든다.
  - HTML: 깊이 0~40 의 중첩, 태그 풀(`div span p a button input table tr td ul li img label form`), 텍스트 길이 0~200(한글·공백·긴 단어 포함), `hidden`/`style=` 속성.
  - CSS: 속성 풀(`display` 전 값, `position`, `float`, `width/height`(음수·0·%·거대값), `margin/padding`, `flex*`, `grid-template-*`(영역·트랙 수 불일치 포함), `overflow`, `white-space`, `line-height`, `font-size`(0·음수 포함)), 셀렉터는 태그·클래스·자손.
  - 실행: `html::parse` → `css::parse` → `style_tree` → `layout_tree(800)` → `build_document_view` 페인트 목록까지. 각 케이스를 `catch_unwind` 로 감싸고 시간 측정.
- **두 모드**: 일반 `cargo test` 에선 시드 200개(수 초), `#[ignore]` 붙은 긴 실행은 `MB_FUZZ_CASES=100000 cargo test -p mb-engine --test fuzz_layout -- --ignored`.
- **판정**: 패닉, 레이아웃 좌표에 NaN/무한대, 상자 폭 1e7px 초과(2차 GitHub 의 187M px 같은 것) — 여기까지는 두 모드 모두. **케이스당 1초 초과**(크기 상한이 있으니 넘으면 초선형)는 `--ignored` 긴 실행에서만 본다 — 기본 `cargo test` 는 debug 빌드·병렬 실행이라 시간 판정이 플래키해진다. 기본 모드는 대신 케이스당 상한 5초를 넘으면 실패(무한 루프만 잡는 넉넉한 값).
- **발견 → 회귀 테스트**: 실패 시 시드와 생성된 HTML/CSS 를 출력하고, **축소기**(자식 하나씩 제거·선언 하나씩 제거를 반복해 여전히 실패하는 가장 작은 입력)를 돌려 몇 줄짜리 입력을 얻는다. 그걸 손으로 정리해 해당 모듈의 단위 테스트로 넣는다(`layout/mod.rs` tests 등, 이름은 증상으로). 퍼저 시드를 테스트로 쓰지 않는다 — 생성기가 바뀌면 의미가 없어지므로.
- CSS 파서 단독 퍼징도 같은 파일에: 임의 토큰열(괄호 불균형, 이상한 단위, 거대 숫자, `!important` 위치)을 `css::parse`·`parse_inline_style`·`parse_selector` 에 — 패닉만 본다.

### 0.4 표적 코드 검토

신뢰할 수 없는 입력이 흐르는 경로만, 목록을 뽑아 하나씩:

```sh
grep -naE '\?;|\.unwrap\(\)|\.expect\(|\[[a-z_]+\]|\[[a-z_]+ *[-+] *1\]' \
  crates/mb-runtime/src/{navigation,resource}.rs crates/mb-engine/src/layout/*.rs \
  crates/mb-engine/src/render/*.rs crates/mb-dom/src/{html,style/*,css/*}.rs | grep -v '#\[cfg(test)\]'
```

각 지점을 셋 중 하나로 분류해 검토 표(이 문서 부록으로 추가)에 남긴다: **안전**(불변식이 코드로 보장 — 주석 추가), **수정**(`get()`/건너뛰기로 바꾸고 테스트), **위험하지만 보류**(이유). 우선순위는 `grid.rs`(2차 패닉 자리, 인덱스 계산 많음) → `table.rs`(열 인덱스) → `inline.rs` → `resource.rs`/`navigation.rs` 남은 `?`.

### 0.5 스크립트 최상위 에러 위치 보고

**현재**: `state/mod.rs` 의 `install_document` 가 `eprintln!("script error: {err}")` 만 한다 — 에이전트 관찰엔 안 보이고(리스너 에러는 Tier 1.5 #18 에서 `console.error` 로 올렸지만 최상위는 빠짐), 메시지는 QuickJS 의 `not a function` 뿐이라 무엇이 없는지 모른다(naver 에서 `MB_JS_STACK=1` 로 원문을 찾아가야 했다).

**현재 코드 경로**(설계 전제):
- 최상위: `js/mod.rs::execute_with_url` 이 에러를 `format_caught_error`(~605행)로 **문자열**로 만들어 돌려주고(~378–406행), `install_document` 는 그 문자열을 `eprintln!` 한다(`state/mod.rs` ~460행).
- 리스너·타이머·rAF: JS 부트스트랩의 `try/catch`(`js/event.rs` ~119·153행, `js/timers.rs` ~173·208행)가 잡아 `__mb_report_error` 로 넘기고, 거기서 `String(err)` 로 문자열화(`js/console.rs` ~85–90행) — **`format_caught_error` 를 지나지 않는다.**
- 관찰의 콘솔 출력은 이미 상한이 있다(`mb-agent/src/observation.rs` ~814–840행: 개수 줄 + 최근 warn/error 3개, 각 200자).

**설계**:
- **포맷터 하나로 모은다**: `js/error_location.rs`(신규) `fn locate(stack: &str) -> Option<Location{url, line, col, func}>` — QuickJS 스택의 **위치가 있는 첫 프레임**(`(native)`/`(missing)` 프레임은 건너뜀)을 파싱. 최상위는 `format_caught_error` 가, 리스너·타이머는 `__mb_report_error` 가 `err.stack` 을 Rust sink 로 함께 넘겨(`String(err)` 대신 `{message, stack}`) 같은 함수를 탄다.
- **최상위 에러를 콘솔로**: `execute_with_url` 반환을 `Result<(), ScriptError{message, location}>` 로 바꾸고, `install_document` 가 Rust 에서 콘솔 sink 에 `console.error("Uncaught in script: …")` 를 직접 넣는다(JS 를 다시 부르지 않음). stderr 줄은 유지(사람용).
- **메시지 형식**: `{message} ({near}) at {url}:{line}:{col}` — 위치·near 를 **URL 앞**에 둬서 200자 잘림에 URL 만 잘리게. URL 은 **쿼리·프래그먼트 제거**(AGENTS.md 의 "URL 로그는 쿼리 제외" — 광고·SDK URL 에 토큰이 흔하다; stderr·관찰·`--log` 모두). 인라인 스크립트는 `{문서URL 쿼리 제거}#inline-{n}:{line}:{col}`(n = `collect_script_sources` 의 인덱스).
- **`not a function` 보강 `near`**: QuickJS 는 호출 대상 이름을 안 준다. 원문에서 열 위치 직전의 멤버 식(`[\w$.]+` 를 뒤로)을 잘라 `near "document.getElementsByTagName"`. 주의: quickjs-ng 의 호출 위치 열은 `(` **다음** 토큰을 가리키므로(`rquickjs-sys` 의 `quickjs.c` `emit_source_loc`), 먼저 `(` 와 공백을 뒤로 건너뛴 뒤 식을 자른다. 원문은 `BrowserState` 가 스크립트 이름 → 원문 맵으로 보관(최상위 실행 후에도 리스너 에러에서 찾을 수 있게; 문서 교체 때 비움). 잘라낸 식은 페이지 문자열이므로 trusted 블록엔 절대 안 넣는다(page 블록 `console:` 만 — AGENTS.md 위조 방지 규칙과 충돌 없음).
- `<eval>`/`new Function` 프레임은 위치를 `<eval>` 로만 표시(원문 맵 없음).
- **결정 필요(0a 에서)**: Google 에선 이동할 때마다 `console: 6 errors` 가 붙어 LLM 이 페이지 디버깅으로 빠질 수 있다. `prompts.rs` 서버 안내에 "제3자 스크립트 콘솔 에러는 흔하고 대개 무시해도 된다" 한 줄을 넣을지 — 넣으면 `prompts` 지문이 바뀌어 eval 기준선이 갈린다(G5c 규칙).

### 테스트 (0단계)

- `install_document_reports_top_level_script_error_with_location` (mb-runtime main_tests): `<script>document.nope()</script>` → 콘솔 메시지에 `not a function`, `#inline-0:1:`, `near "document.nope"` 포함.
- `listener_error_has_location` — `addEventListener('click', () => x.y())` 후 클릭 → 위치 붙음(리스너 경로도 같은 포맷터).
- `error_location_strips_query` — `https://ad.example/sdk.js?token=abc` 에서 난 에러 → 메시지에 `token` 없음.
- `near_skips_paren_before_call` — 열이 `(` 다음을 가리키는 스택으로도 `near` 가 맞음.
- `format_caught_error_parses_first_frame` (js/mod.rs 단위): 스택 문자열 몇 가지(이름 있음/없음/`<eval>`) → 기대 문자열.
- 관찰 테스트(golden.rs 옆): 에러가 page 블록 `console:` 에만, trusted 블록엔 없음.
- 퍼저 기본 200 시드가 일반 테스트에 포함 (+1 테스트).
- 0.2·0.3·0.4 에서 찾은 버그마다 최소 재현 테스트.
- 리소스 수집 함수 시그니처 변경(`Result` 제거) 후 기존 resource 테스트 green.

### 측정·완료 기준 (0단계)

- 코퍼스 전체에서 **nav-fail 0 · panic 0 · timeout 0** (`blocked` 제외, 120초 상한, release 빌드; debug 는 panic·nav-fail 0). `[mb-budget]` 줄 0(release).
- **타임박스**: 30곳이면 2차(5곳에서 막는 버그 4개)보다 많이 나올 수 있어 끝이 열린 목표다. 0단계에 5일을 넘기면 남은 실패를 "알려진 문제 → 담당 단계" 표(부록)로 넘기는 것을 완료로 인정한다 — 단 panic 은 넘기지 않는다.
- 퍼저 100,000 케이스 무결(패닉·NaN·1초 초과·1e7px 상자 0).
- 0.4 검토 표 완성(모든 지점 분류).
- 코퍼스의 스크립트 에러가 전부(`<eval>` 프레임 제외) `url:줄:열` 을 달고 나옴; naver 광고 2건이 `near "document.getElementsByTagName"` / `near "console.debug"` 로 읽힘.
- AGENTS.md 에 Robustness Rules 절.

### 위험 (0단계)

- 코퍼스 녹화가 사이트를 다시 방문한다(원래 5곳 포함) — 1회로 제한, 간격 유지.
- 퍼저가 많이 찾으면 0단계가 길어진다 → 패닉·무한 시간만 0단계에서 고치고, "모양이 틀림" 류는 해당 단계로 넘긴다.
- 스크립트 에러를 콘솔로 올리면 관찰 `console:` 이 시끄러워진다(Google 6건). 상한은 이미 있으니(최근 3개·200자) 출력 크기는 문제 없고, 문제는 LLM 의 주의 — 위 "결정 필요" 항목.
- 워치독의 벽시계는 결정성 예외 — stderr 이외로 새지 않는지 코드 리뷰 체크 항목.

**노력**: M (3~4일 — 코퍼스 1일, 퍼저+축소기 1일, 검토 1일, 에러 위치 0.5일, 발견 수정은 별도).

**커밋 나누기**: 0a = 0.1 규칙 + 0.4 검토 표·수정 + 0.5 에러 위치 + 3차 골든·eval 시나리오(코드만으로 끝남) · 0b = 0.2 코퍼스 스크립트·probe 분류·`scripts/compat-sites.tsv`·녹화(사이트 방문은 여기서 한 번)·새 기준선 측정 · 0c = 0.3 퍼저·축소기 + 발견 수정. 각각 green 으로 따로 커밋, 진행 표엔 0 한 줄로.

---

## 1단계 — `!important` + 값 파서 묶음

### 목표

2차에서 버려진 선언 대부분(GitHub 6,533 · naver 458 · Google 345 · 위키 233 · HN 24)을 살린다. 원인은 `css/mod.rs` 의 `DeclHandler::parse_value` 가 `parse_entirely` 안에서 돌아, 전용 파서 없는 속성은 토큰이 둘 이상이면 선언째 버려지는 것.

### 세부 항목

1. **`!important`**: 파싱 + 캐스케이드 중요도.
2. **`border` 계열 shorthand**: `border`, `border-top/right/bottom/left`, `border-width`, `border-style`, `border-color` (1~4값).
3. **쉼표 목록**: `font-family`(전체 목록 보관), `box-shadow` 여러 겹(첫 겹만 그리되 파싱은 성공), `transition`/`animation`(파싱만 — 아무도 안 읽음, 버림 통계에서 빠지게).
4. **커스텀 속성 원문 보관**: `--x: 255,255,255`, `--y: 0 5px 2px rgba(…)` → 임의 토큰열.
5. **`rgb(var(--x))` / `rgba(var(--x), .5)`**: 원문 치환 후 재파싱 (아래 설계).
6. **`flex` shorthand 보완**: `flex: 1 1 auto`, `flex: 0 0 120px`, `flex: none/auto/initial`.
7. **`background-size`**: `224px 242px`, `cover`, `contain`, `auto` (페인트까지 — 스프라이트 위치가 이것에 의존).
8. **`clip: rect(…)` / `clip-path: inset(…)`** 파싱만(값 보관 — 네 변 값과 `auto` 를 일반형으로) — 3단계에서 사용.
9. **`light-dark(a, b)`**: 라이트 고정이니 첫 인자로 파싱. 못 읽으면 선언째 버려져 색이 빠진다.
10. **전역 키워드·`all`·`inset` 점검**: `initial`/`inherit`/`unset`/`revert`/`revert-layer` 와 `all: unset`(버튼 리셋에 흔함), `inset` shorthand 의 현재 지원을 확인하고, 없으면 `revert`/`revert-layer` = `unset` 근사로 넣는다(UA 시트 되돌리기는 범위 밖).

### 설계 결정

**`!important`**
- `DeclHandler::parse_value` 끝에서 `input.try_parse(cssparser::parse_important).is_ok()` — 모든 속성 파서는 `!` 앞까지만 소비해야 하므로, 디스패치 전에 값 슬라이스에서 `!important` 를 먼저 떼는 방식이 간단하다: `parse_until_before(Delimiter::Bang, …)` 로 값을 파싱하고 남은 것에서 `!important` 를 확인.
- `Declaration` 에 `important: bool` 필드 추가. shorthand 가 펼친 longhand 는 같은 플래그를 물려받는다.
- 세부: `--x: red !important` 는 원문에서 `!important` 를 떼고 플래그만 남긴다(값은 `red`). `!ie`·`!important foo` 같은 뒤꼬리는 선언째 버린다. `@keyframes` 안의 `!important` 선언은 무시(스펙).
- 캐스케이드(`style/cascade.rs::specified_values`): 현재 `(specificity, rule_order)` 정렬 후 순서대로 덮어씀. 바꿔서 **두 번** 적용 — ① 일반 선언 전부(지금과 같음) + 인라인 `style` 일반, ② 그다음 중요 선언 전부(같은 정렬) + 인라인 `!important`. 스펙 순서(UA 일반 < 작성자 일반 < 인라인 일반 < 작성자 중요 < 인라인 중요)와 맞다. UA 중요·사용자 스타일시트는 없음.
- 성능: 규칙당 중요 선언 존재 여부를 파싱 때 `Rule::has_important` 로 기록해 두 번째 패스를 대부분 건너뛴다.

**값 파서 구조**
- 새 전용 파서는 `css/shorthand.rs`(border), `css/parse.rs`(목록) 에. 각 파서는 자기 몫만 소비하고 남기면 실패(지금과 같은 계약).
- **실패 정책**: 현재 범용 `parse_value`(`css/parse.rs` ~28–59행)는 **단일 토큰**이면 모르는 ident·모르는 단위(`Keyword("100vw")`)·문자열을 `Keyword` 로 보관하고, 토큰이 여럿이거나 받을 수 없는 토큰일 때만 선언째 버린다. 1단계는 이 정책을 바꾸지 않는다(바꾸면 영향이 너무 넓다). 새 전용 파서는 "일부만 이해하고 나머지 무시"를 하지 않는다 — 잘못 적용하는 것보다 버리는 게 안전. 단일 토큰 `Keyword` 폴백 수는 [측정 프로토콜](#범위와-순서) 대로 따로 센다.

**`border` 계열 — 현재 키 체계가 표준과 다르다**: 폭은 shorthand 이름 키(`border-top` 등)에 저장되어 `edge_sizes(node, "border")`(`layout/block.rs` ~28행, `inline.rs`, `table.rs`, `taffy_bridge.rs` ~313–322행)가 읽고, 색은 단일 `border-color` 하나(`render/display_list.rs` ~868행), `border-style` 은 페인터가 안 읽는다(폭 > 0 이면 실선). **결정: 1단계는 기존 키 체계를 유지**하고 그 위에 —
- 펼친 결과를 기존 키로 매핑: `border-top: 2px dashed red` → `border-top` 폭 2px + **면별 색 키 `border-top-color`**(새로 추가, 페인터가 있으면 `border-color` 보다 우선).
- `border: none` / `border-style: none|hidden` → 해당 면 폭 0(지금은 폭이 남아 테두리가 그려질 수 있다).
- 작성자가 쓴 `border-top-width` 같은 진짜 longhand 는 같은 기존 키로 접는다.
- 점선·이중선 등 `border-style` 페인트는 범위 밖(실선). 진짜 longhand 체계로의 전환은 4차 후보로 기록 — `defaults.rs`·`presentational.rs`·layout·taffy 를 다 건드리는 일이라 이번 단계의 목표(버림 감소)와 분리한다.

**커스텀 속성과 `var()` — "미해결 값"
- 지금 `Value::Var { name, fallback }` 는 **값 전체가 `var()` 하나**일 때만 동작하고(`style/variables.rs` 가 최상위 Var 만 치환), 커스텀 속성 값은 단일 토큰만 파싱된다.
- 새로 `Value::Unparsed(String)` (정규화된 원문 토큰열) 추가:
  - `--*` 선언은 **항상** `Unparsed` 로 보관(원문 그대로 — 스펙의 "토큰열").
  - 일반 속성 값에 `var(` 가 들어 있으면 파싱하지 않고 `Unparsed` 로 보관.
  - **shorthand 는 파싱 때 longhand 마다 펼쳐 보관**(스펙의 pending-substitution): `padding: 5px var(--x)` → `padding-top/right/bottom/left` 각각에 `Value::Unparsed{ shorthand: Some("padding"), raw }`. 이유: `PropertyMap` 은 `BTreeMap<String, Value>`(`style/mod.rs` ~17행)이고 캐스케이드는 이름으로 덮어쓰기만 하므로(`cascade.rs` ~118–123행), shorthand 이름 하나로 보관하면 `.a{padding-left:3px} .a.b{padding:var(--x)}` 에서 어느 쪽이 이겨야 하는지 캐스케이드 후엔 알 수 없다. longhand 단위로 넣으면 캐스케이드가 지금처럼 맞게 돈다.
- 캐스케이드 후 `resolve_var_references` 확장: `Unparsed` 값의 `var(--n[, fb])` 를 원문 치환(재귀, 순환 감지 그대로) → `shorthand` 가 있으면 **shorthand 파서로** 다시 파싱해 자기 longhand 몫만 꺼내고, 없으면 그 속성 이름의 파서(`parse_declaration_value(name, …)`)로.
- 파싱 실패 → "invalid at computed-value time" = **`unset`**: 상속 속성이면 부모 계산값, 아니면 초기값. **캐스케이드의 그 앞 선언으로 되돌아가지 않는다** — `.a{background:red} .b{background:var(--undef)}` 의 브라우저 결과는 투명이다(흔한 오해라 테스트로 고정).
- **커스텀 속성은 정의한 노드에서 치환까지 끝낸 뒤 상속한다**(스펙: 계산값 상속). `:root{--a: var(--b)}` 아래 자식이 `--b` 를 다시 정의해도 `--a` 는 루트에서 풀린 값이다. 노드마다 "치환 완료된 커스텀 속성 맵"을 부모에게서 받아 자기 정의만 풀어 덮으므로 비용도 준다.
- 메모 키는 `(shorthand 또는 속성 이름, 치환된 원문)`.
- 기존 `Value::Var` 는 호환을 위해 남기되 파서는 새 경로만 생성(Var 는 1단계 끝에서 제거 후보로 표시).
- 비용: 재파싱은 `var()` 가 든 선언에만, 노드마다. GitHub 처럼 수천 개면 노드×선언으로 커질 수 있다 → `(속성 이름, 치환된 원문)` → 파싱 결과 메모(스타일 패스당 `HashMap`, 결과는 순서 무관이라 결정성 문제 없음).

**`rgb(var(--x))`**: 위 경로로 자연히 해결 — `color: rgb(var(--x))` 는 `var(` 를 포함하므로 `Unparsed`, 치환 후 `rgb(255,255,255)` 로 재파싱.

**`font-family` 목록**: `Value::Keyword` 에 원문 목록(`"Verdana, Geneva, sans-serif"`)을 넣지 않고 `Value::FontFamilies(Vec<String>)` 신설. 현재 소비자는 monospace 판정 두 곳(`layout/inline.rs::inline_font_family`, `render/display_list.rs::font_family_keyword`) — "목록에 `monospace` 계열 이름이 있으면 monospace" 로 바꾼다. 실제 폰트 선택(cosmic-text `Family::Name`)은 범위 밖.

### 테스트 (1단계)

모두 `css/mod.rs` 또는 `style/mod.rs` 단위 테스트, 한 줄 CSS:
- `important_declaration_is_kept_and_flagged` — `.a{width:1px !important}`.
- `important_beats_more_specific_normal` — `#x{color:red} .a{color:blue !important}` → blue. `important_inline_beats_important_author`.
- `border_shorthand_expands_to_longhands` — `border:1px solid #ccc`, `border-top:2px dashed red`, `border:none`, `border-width:1px 2px`.
- `font_family_list_parses`, `box_shadow_list_keeps_first_layer`, `transition_parses_without_drop`.
- `custom_property_keeps_raw_tokens` — `--x: 255,255,255`.
- `rgb_of_var_resolves_after_cascade` — `:root{--x:255,0,0} p{color:rgb(var(--x))}` → red.
- `shorthand_with_var_expands_after_substitution` — `padding: 5px var(--x)`.
- `invalid_after_substitution_falls_back_to_inherited` — `--x: foo; color: var(--x)` → 부모 색.
- `iacvt_does_not_fall_back_to_earlier_declaration` — `.a{background:red} .b{background:var(--undef)}` 둘 다 붙은 요소 → 배경 없음.
- `shorthand_var_and_longhand_cascade_in_order` — `.a{padding-left:3px} .a.b{padding:var(--x)}` 와 그 반대 순서 모두 브라우저와 같은 결과.
- `custom_property_inherits_substituted_value` — `:root{--b:1px; --a:var(--b)} div{--b:9px; width:var(--a)}` → 1px.
- `custom_property_important_strips_flag`, `important_garbage_suffix_drops`, `border_none_zeroes_width`, `border_side_color_paints`, `light_dark_picks_first`.
- `flex_shorthand_three_values_with_auto_basis`, `background_size_two_lengths_and_keywords`.
- 페인트: `background_size_scales_sprite` (render 단위, 4×4 이미지 → 지정 크기).
- 퍼저 CSS 풀에 `!important`·shorthand·`var()` 추가.

### 측정·완료 기준 (1단계)

- 코퍼스 "버린 선언"(`ours` + IACVT + `Keyword` 폴백 합산) 이 0단계 새 기준선 대비 **80% 이상 감소**(2차 5곳 환산 7,593 → 1,500 이하; 남는 건 `calc()`(5단계)와 진짜 미지원 속성). 사이트별 표를 [진행 표](#진행-표) 에.
- `!important` 버림 0.
- naver: `rgb(var(--atcp-…))` 색 47건 살아남, 테두리 보임(미리보기 전후).
- 0단계 기준 유지(nav-fail/panic/timeout 0).
- [에이전트 영향 기준](#범위와-순서): 기대 컨트롤 전부 발견 + 사라진 요소 목록 판정. 특히 `display:none !important` 가 살아나 **숨겨야 할 것이 숨는지**(위키 12건). 2.1 이 먼저 끝나 있으므로 반응형 숨김은 데스크톱 폭에서만 적용된다.
- **probe 변경**: IACVT 수·`Keyword` 폴백 수·"파싱만" 속성 열.

### 위험 (1단계)

- **골든이 회귀를 못 잡는 쪽이 진짜 위험**: 기존 골든은 author CSS 가 거의 없어(`font-family` 한 줄) 변화가 작다 — 그래서 0a 의 `compat3` 골든이 필요하다. 테두리 shorthand 가 UA 기본과 상호작용하면 `page_height` 가 바뀔 수 있다.
- `!important` 가 살아나면 지금까지 우연히 보이던 요소가 숨는다 → 관찰 요소 수가 **줄어드는 게 정상**인 경우가 있다. 사이트별로 줄어든 요소가 실제 Chrome 에서도 안 보이는지 확인(사람 비교 — 같은 페이지를 실제 브라우저로).
- `Unparsed` 재파싱 비용 — GitHub 로드 시간(2차 debug 21초)이 늘면 메모를 먼저.
- `Value` enum 변경이 `match` 를 많이 건드린다(mb-engine) — 컴파일러가 잡아 준다.

**노력**: M (3~4일 — shorthand 별 longhand 펼침·border 키 매핑·IACVT 가 처음 추정보다 넓다).

---

## 2단계 — `@media` 평가 → `@layer` → `@supports`

### 목표

모든 `@media` 가 참인 지금 상태를 끝낸다. GitHub 다크 모드, HN·위키의 모바일 규칙, 위키의 인쇄용 규칙, Google 의 forced-colors 규칙이 데스크톱 라이트에서 빠지게. 그다음 지금 통째 버리는 `@layer`(GitHub CSS 46%)와 `@supports` 본문을 살린다.

### 2.1 `@media` 평가

**세부 항목**
- 파싱 때 조건을 버리지 않고 규칙에 붙인다: `Rule::media: Option<Rc<MediaQueryList>>` (같은 블록 규칙들이 공유). 중첩 `@media` 는 조건 목록을 AND 로 합친다.
- 지원 조건:
  - 미디어 타입: `all`, `screen` = 참, `print`, `speech` 등 = 거짓. `only` 무시, `not` 지원.
  - 폭·높이: `width`, `min-width`, `max-width`, `height`, `min-height`, `max-height`, **범위 문법**(`width>=768px`, `400px<=width<=700px`, `width<calc(48rem - .02px)` — 이 마지막은 5단계 전까지 `calc` 를 아주 좁게: 상수 `rem/em/px` 덧뺄셈만 여기서 직접 계산).
  - 단위: `px`, `em`/`rem`(=16px, 미디어 쿼리는 초기 글꼴 크기 기준 — 스펙), `vw`/`vh` 는 뷰포트.
  - `prefers-color-scheme: light` 참 / `dark` 거짓 (라이트 고정 — 에이전트 스크린샷이 라이트인 것과 같은 이유).
  - `prefers-reduced-motion: no-preference` 참, `reduce` 거짓. `forced-colors: none` 참, `active` 거짓. `-ms-high-contrast` 거짓.
  - `hover: hover`, `pointer: fine`, `any-hover/any-pointer` 같음. `orientation` 은 폭·높이로. `resolution`/`-webkit-min-device-pixel-ratio` 는 1(에이전트 기준) — 관전 창 Retina 와 무관하게 결정적이어야 하므로.
  - 데스크톱 기본값으로 **참**: `scripting: enabled`(JS 가 돈다), `display-mode: browser`, `prefers-contrast: no-preference`, `(color)`, `color-gamut: srgb`, `dynamic-range: standard`, `inverted-colors: none`, `update: fast`, `prefers-reduced-transparency: no-preference`.
  - 모르는 조건(오타 등) → 그 쿼리는 **거짓**(스펙: 알 수 없는 미디어 기능 = unknown → not all). `not` 을 붙여도 거짓.
  - 문법: 쉼표 목록 = OR, **무효 쿼리는 그 항목만** `not all`(목록 전체가 죽지 않음). `not` 은 쿼리 전체를 부정.
  - `@custom-media`(GitHub 6) → 범위 밖, 그걸 쓰는 쿼리는 모르는 조건 = 거짓.
- 평가 입력: `MediaContext { viewport_width, viewport_height }` 만. 테마는 상수.
- 캐스케이드: `style_tree_with_state` 에 `&MediaContext` 를 넘기고, `specified_values` 의 규칙 순회에서 `rule.media` 가 거짓이면 건너뛴다. 평가는 **스타일 패스 시작 시 한 번**, 규칙 블록별 결과를 벡터로 캐시(규칙 수만큼 bool) — 노드마다 평가하지 않는다.
- 뷰포트 높이가 스타일 경로에 처음 들어간다: `build_document_view` 시그니처에 `viewport_height` 추가. `BrowserState` 의 뷰 캐시 키(`cached.viewport_width == viewport_width` 비교, `state/mod.rs` ~1003행)는 높이를 **그대로 넣지 않고** `(viewport_width, media_results, vh_height)` 로 바꾼다:
  - `media_results` = 문서의 미디어 쿼리 목록 평가 결과 비트셋(`Vec<bool>` 또는 `u64` 청크). 리사이즈해도 결과가 같으면 키가 같다.
  - `vh_height` = 스타일 패스가 `vh`/`vmin`/`vmax` 단위를 한 번이라도 해석했으면 뷰포트 높이, 아니면 0(5단계에서 채워짐; 2단계에선 항상 0).
  - 결과: 높이만 바뀌는 리사이즈(관전 창 세로 드래그)는 쿼리 경계를 넘을 때만 재스타일. 폭 변경은 레이아웃 때문에 지금처럼 재계산.
  - 비트셋 계산 자체는 쿼리 수만큼의 비교라 매 프레임 키 확인에 써도 싸다(GitHub 수백 개 — 실측해서 진행 표 메모에).
- `matchMedia` JS API 는 범위 밖(4차) — 다만 같은 평가 함수를 쓰도록 `css::media::evaluate(&MediaQueryList, &MediaContext) -> bool` 를 공개 함수로.
- `<link rel=stylesheet media="print">` 와 `<style media="…">` 속성도 같은 평가(현재 무시됨 — 코퍼스에서 흔함). **주의 — 시트가 합쳐진다**: 외부 시트는 `navigation.rs` ~212행에서 `join("\n")` 으로 문자열 하나가 되고(`HistoryEntry.stylesheet: String`), `<style>` 도 전부 하나로 합쳐진다(`state/mod.rs` ~1234행). 결국 파싱되는 시트는 [외부, 인라인] 둘뿐이라 시트별 `media` 정보가 사라진다. **결정**: 합칠 때 `media` 속성이 있는 시트를 `@media {속성값} { … }` 로 감싼다(`resource.rs` 가 `<link>` 의 media 를 같이 들고 오게). 가장 작은 변경으로 같은 의미.

### 2.2 `@layer` 펼치기

**왜 `@media` 뒤인가**: GitHub 의 버려진 `@layer` 블록 안에 `prefers-color-scheme: dark` 블록이 49개 있다. `@media` 평가 없이 `@layer` 만 펼치면 다크 규칙 49개가 **추가로** 적용되어 GitHub 이 더 어두워진다. 평가가 먼저 있으면 펼친 레이어 안의 다크 블록은 거짓으로 빠진다.

**설계**
- `@layer name { … }` / `@layer { … }` → 본문 규칙을 펼친다. `@layer a, b;` (선언만) → 순서 등록.
- 우선순위 근사: 스펙은 "레이어 안 규칙 < 레이어 밖 규칙"(일반 선언 기준; 중요 선언은 반대), 레이어끼리는 선언 순서. 구현: `Rule::layer_rank: u16` — 레이어 밖 = `u16::MAX`, 레이어는 첫 등장 순서대로 0,1,2…. 캐스케이드 정렬 키를 `(layer_rank, specificity, rule_order)` 로(일반 패스). 중요 패스에선 `layer_rank` 를 뒤집는다(스펙대로 레이어 안 중요가 이김). 1단계의 두 패스 구조에 그대로 얹힘.
- 중첩 레이어(`@layer a { @layer b {…} }`)는 이름 `a.b` 로 평탄화. 순위는 **레이어 트리의 후위 순서**: 자식 레이어가 부모 직속 규칙보다 앞(낮음)이다 — 스펙상 `a` 에 바로 있는 규칙이 `a.b` 를 이긴다. 즉 `@layer a { @layer b {…} .x{…} } @layer c {…}` → `a.b`=0, `a`(직속)=1, `c`=2. 구현: 파싱 중 레이어 트리(이름별 첫 등장 순서로 자식 정렬)를 만들고, 시트 파싱이 끝난 뒤 후위 순회로 `layer_rank` 를 채운다(규칙엔 레이어 id 만 두고 순위는 나중에 — 뒤에 나오는 `@layer a.c;` 선언이 순서를 바꿀 수 있으므로). 이름 없는 `@layer { }` 는 매번 새 익명 레이어.
- 여러 스타일시트의 레이어는 문서 전체에서 하나의 순서를 공유한다(같은 이름 = 같은 레이어, 첫 등장 순). 위 2.1 처럼 시트는 [외부, 인라인] 둘로 합쳐지고, 인라인 시트는 DOM 리비전마다 따로 재파싱되므로(`refresh_inline_stylesheet`, `state/mod.rs` ~1090행) 순위는 `css::parse` 안이 아니라 **`BrowserState` 에서 두 시트의 레이어 트리를 합쳐 매기는 후처리**로 한다. 실제 문서 순서(link 와 style 이 섞인 순서)가 아니라 "모든 link 다음 모든 style" 이 되는 것은 **기존 캐스케이드 순서의 근사**이기도 하다 — 기록만 하고 이번엔 안 고친다.
- 근사 명시: 스펙상 조건이 거짓인 `@media`/`@supports` 안에서 처음 나온 레이어는 순서에 들어가지 않는다(미디어 결과가 바뀌면 순서도 바뀜). 실사이트는 대부분 맨 앞 `@layer a, b;` 문장으로 순서를 고정하므로 이번엔 **조건과 무관하게 등장 순**으로 매긴다.

### 2.3 `@supports` 펼치기

**설계**: 조건을 실제로 평가한다 — "참으로 펼치기"는 `@supports not (display:grid)` 같은 폴백 블록을 잘못 적용하므로. `not`/`and`/`or`, `selector(…)` 는 셀렉터 파서로(4단계 전엔 `:is()` 등이 거짓 — 정상). 모르는 형식 = 거짓.

`(prop: value)` 판정 — "파서가 받아들이면 참"은 **틀리다**: 지금 파서는 모르는 값을 `Value::Keyword` 로 떨어뜨리고(예: `Keyword("100vw")`), 1단계 뒤엔 `var()` 가 든 값은 무조건 `Unparsed` 라, 거의 모든 조건이 참이 된다. 그래서:
1. 속성이 우리가 아는 속성이어야 한다(아래 `css/properties.rs` 표에 있는 이름).
2. 파싱 결과가 그 속성에 **의미 있는 타입**이어야 한다 — 키워드 속성이면 그 속성의 허용 키워드 목록 안, 길이 속성이면 `Length`/`Percent`(5단계 뒤 `Calc`), 색이면 `Color`. 아무 데나 붙는 `Keyword` 폴백·`Unparsed`·`Var` 는 거짓. `var()` 가 든 조건은 스펙상 참이지만 실사이트에선 거의 없으니 거짓으로 둔다(보수적).
3. **파싱은 되지만 렌더링을 안 하는 값은 거짓** — 폴백 블록이 켜지는 쪽이 실제 화면에 가깝다. 목록을 `css::supports::UNRENDERED` 상수로 둔다(초기: `position: sticky`, `display: contents`, `backdrop-filter`, `mask*`, `container-type`, `aspect-ratio`(미구현이면) 등 — 0단계 끝에 실제 구현 상태로 확정). 기능을 구현하면 목록에서 빼는 것을 그 커밋의 체크 항목으로.

- 커스텀 속성 조건 `(--x: 0)` 은 스펙상 항상 참(흔한 "변수 지원" 검사) — 참으로 둔다(위 `var()` 거짓과 별개).
- `selector(…)` 안은 **비관용** 파싱 — `selector(:is(:foo))` 는 거짓(Chrome 과 같음).
- Chrome 이 받는 `-webkit-` 속성(`-webkit-appearance`, `-webkit-backdrop-filter`, `-webkit-line-clamp` 등)은 표준 이름과 같은 판정으로.

**속성 표가 없다(새로 만든다)**: 위 1·2번은 "우리가 아는 속성 + 허용 값" 등록표가 필요한데, 지금 `parse_declaration_value`(`css/mod.rs` ~554–621행)는 14개쯤만 특별 처리하고 나머지는 범용 `parse_value` 로 간다. `css/properties.rs`(신규) 에 `속성 이름 → 값 종류(키워드 목록 / 길이 / 색 / 전용 파서)` 표를 만든다. 1단계의 `Keyword` 폴백 집계와 ours/browser-too 분류(모르는 속성 판정)도 이 표를 쓴다 — 그래서 **표 자체는 0b 에서 먼저** 만들고 2.3 은 그걸 쓴다.

판정 함수는 `css::supports::evaluate(&SupportsCondition) -> bool` 로 공개(나중의 `CSS.supports()` JS API 가 같은 함수를 쓴다).
**왜 `@layer` 뒤인가**: 규모가 작고(GitHub 7.8KB), 레이어 순위 장치가 먼저 있으면 레이어 안 `@supports` 도 같은 코드로 처리.

`@container`(GitHub 21블록, 4.8KB) 는 범위 밖 — 계속 건너뜀(크기 질의는 레이아웃 후 재스타일이 필요해 별도 설계).

### 테스트 (2단계)

`css/media.rs` 단위(한 줄 쿼리 → bool, 1280×800):
- `media_width_queries` — `(min-width:768px)` 참, `(max-width:767px)` 거짓, `(width>=768px)` 참, `(400px<=width<=700px)` 거짓, `(width<calc(48rem - .02px))` 거짓.
- `media_types` — `print` 거짓, `screen and (min-width:1px)` 참, `not print` 참, `only screen` 참.
- `media_preferences` — `(prefers-color-scheme:dark)` 거짓, `(prefers-reduced-motion:reduce)` 거짓, `(forced-colors:active)` 거짓, `(hover:hover)` 참.
- `media_unknown_feature_is_false`, `media_not_unknown_is_false`, `media_em_uses_16px`, `media_desktop_defaults_true`(`scripting`·`display-mode`·`color-gamut` 등), `media_list_invalid_item_only_drops_that_item`.
- `style_media_attribute_wraps_sheet`(`<style media=print>` 내용이 적용 안 됨).
- `supports_custom_property_true`, `supports_selector_is_not_forgiving`.
- 캐스케이드: `media_rule_applies_only_when_matching` (뷰포트 2개로 같은 문서), `nested_media_is_and`, `link_media_attribute_is_evaluated`.
- `layer_rules_lose_to_unlayered`, `later_layer_beats_earlier`, `important_in_layer_beats_unlayered_important`, `dark_media_inside_layer_stays_off`, `parent_direct_rules_beat_child_layer`(`@layer a{@layer b{p{color:red}} p{color:blue}}` → blue), `layer_statement_fixes_order`(`@layer b, a;` 뒤 a 블록이 먼저 나와도 a 가 이김), `same_layer_name_across_sheets_shares_rank`.
- `supports_true_branch_applies`, `supports_not_branch_skipped_when_supported`, `supports_unknown_property_false`, `supports_keyword_fallback_value_false`(`(width: 100vw)` 는 5단계 전 거짓), `supports_unrendered_value_false`(`(position: sticky)` 거짓 → `not` 블록 적용), `supports_selector_function`.
- 상태: `resize_restyles_media_rules` (mb-runtime — 폭 바꿔 같은 요소 색이 바뀜).

### 측정·완료 기준 (2단계)

- GitHub 미리보기가 **라이트**(페이지 배경 흰색 계열) — 전후 PNG.
- 코퍼스 `css.txt` 의 "@media 조건" 표에서 "wrong at 1280x800" 범주가 **적용 0**(probe 를 "평가 결과" 열을 찍도록 갱신).
- GitHub "버린 CSS 바이트"(at-rule 본문 스킵) 46% → 5% 이하(`@container` 와 `@custom-media` 만 남음).
- HN 레이아웃이 2차와 같거나 나음(모바일 규칙 10개가 빠짐) — 골든이 아니라 미리보기 비교.
- 에이전트 찾은 컨트롤 수(사이트별) 감소 없음; 모바일 전용 메뉴 버튼 등은 사라지는 게 정상 — 실제 Chrome 데스크톱과 비교.

### 위험 (2단계)

- **관찰이 뷰포트 높이에 의존**하게 된다(높이 쿼리). 골든은 800×600 고정이라 결정성은 유지되지만, 높이를 바꾸는 테스트가 있다면 결과가 달라질 수 있다.
- `UNRENDERED` 목록이 실제 구현과 어긋나면(기능을 구현했는데 목록에 남음) 폴백 블록이 계속 켜진다 — 위 체크 항목 + `supports_unrendered_list_matches_layout` 같은 테스트로 목록 항목마다 "정말 렌더링 안 함" 을 고정할 수 있는 건 고정.
- 레이어 정렬을 잘못 넣으면 지금 맞게 보이던 사이트가 틀어진다(특히 GitHub Primer 는 레이어 + 비레이어 혼합) — 코퍼스 전후 미리보기로 확인.
- 뷰 캐시 키 변경 → 비트셋 키라 높이만 바뀌는 리사이즈는 쿼리 경계에서만 재스타일. 비트셋 계산 비용(쿼리 수백 개 × 매 프레임)은 MB_PROFILE 로 확인 — 비싸면 리사이즈 이벤트 때만 다시 계산.

**노력**: M (3~4일; `@media` 1.5(시트 media 감싸기 포함), `@layer` 1(BrowserState 후처리), `@supports` 0.5 + 속성 표는 0b 에 1일).

---

## 3단계 — overflow 클리핑

### 목표

sr-only 텍스트(`.blind`, `.sr-only`, `visuallyHidden`: `position:absolute; width:1px; height:1px; overflow:hidden; clip:rect(0 0 0 0)`)가 1px 폭 상자에 한 글자씩 세로로 그려지는 것(2차 슬리버: GitHub 17, naver·위키 다수)을 없앤다. 넘치는 캐러셀·잘린 목록도 같이.

### 세부 항목

1. `overflow: hidden | clip | scroll | auto` (축별 `overflow-x/-y`) → 그 상자의 **패딩 상자**로 자손 페인트를 자른다.
2. `clip: rect(0 0 0 0)` / `rect(0,0,0,0)` / `rect(1px,1px,1px,1px)` (absolute/fixed 에만) 와 `clip-path: inset(50%)` / `inset(100%)` → 면적 0 클립 = 서브트리 안 그림.
3. 둥근 모서리 클립(`border-radius` + overflow)은 **범위 밖** — 사각형만.
4. **선행 수정 — 퍼센트 높이 기준**: 레거시 레이아웃은 `height: %` 를 **부모 폭** 기준으로 푼다(`layout/block.rs` ~212행 주석에 명시, flex/grid/inline/table 도 같음). `relative_offset` 의 `top/bottom %` 도 폭 기준이다(`layout/properties.rs` ~100–105행). 지금은 넘쳐 보이기만 해 티가 안 나지만, 클리핑이 붙으면 `height:100%; overflow:hidden` 래퍼(앱 셸·캐러셀)가 엉뚱한 높이에서 잘려 **내용이 사라진다**. 스펙대로: 포함 블록 높이가 확정이면 그 기준, 아니면 `height` → `auto`, `min-height` → 0, `max-height` → `none`, `top/bottom` → `auto`. 3단계 첫 하위 커밋으로.

### 설계 결정

- **현재 페인터 구조가 제약이다**: `render/display_list.rs` 는 쌓임 맥락마다 positioned 자손을 트리에서 **뽑아내** z-레이어로 따로 그린다(`collect_positioned_into`, ~232행 — 부모의 alpha·transform 을 같이 들고 감). 그래서 "overflow 상자의 자식들을 `ClipGroup` 으로 감싼다" 는 트리 래핑은 **틀린다** — absolute/relative 자손은 그 그룹 밖에서 그려져 안 잘린다. sr-only 상자가 static 부모 안의 relative 래퍼 안에 있는 흔한 모양에서 바로 깨진다.
- **결정: 상자별 유효 클립을 한 번 계산해 표로 둔다.** `layout/clip.rs`(신규) `compute_clips(root: &LayoutBox) -> ClipTable` — 레이아웃 후 한 번 트리를 걸으며 상자마다 `Option<Rect>`(페이지 좌표, 교집합 완료)를 채운다. 페인트·히트 테스트·에이전트 관찰·probe 가 **같은 표**를 본다(넷이 서로 다른 규칙을 갖는 버그 차단). 조회 전용이라 HashMap 이어도 결정성 문제 없음(순회하지 않음) — 그래도 `NodeId` 인덱스 `Vec` 이 더 싸면 그쪽.
- **클립하지 않는 상자**:
  - **루트와 body**: 스펙상 루트의 `overflow` 는 뷰포트로 전파되고, 루트가 `visible` 이면 body 의 `overflow` 가 뷰포트로 간다(body 상자 자체는 안 자름). 뷰포트 스크롤 모델이 없으니 **둘 다 클립하지 않는다**. 이게 없으면 흔한 `body{overflow-x:hidden}`·모달 열림 `body{overflow:hidden}` 에 `html,body{height:100%}` 가 겹칠 때 첫 화면 아래 페이지 전체가 사라지고 히트 테스트도 죽는다.
  - `display: inline` 비대체 요소: `overflow` 가 적용되지 않는다(스펙).
- **축 규칙**: `overflow-x:hidden; overflow-y:visible` 이면 계산값의 `visible` 이 `auto` 가 되어 **두 축 다** 자른다. 그래서 실질적으로 "한 축이라도 visible 이 아니면 사각형 클립"(`clip`+`visible` 조합만 한 축).
- **포함 블록 규칙(스펙 근사)**: 걷는 동안 클립 두 개를 들고 간다.
  - `clip_flow` — 일반 흐름 자손용. overflow 상자를 지나면 `clip_flow ∩= 그 상자의 패딩 상자`.
  - `clip_abs` — `position:absolute` 자손용 = **가장 가까운 positioned 조상에 들어설 때의** `clip_flow`. positioned 상자를 지날 때(자기 overflow 를 적용한 뒤) `clip_abs = clip_flow`.
  - 상자 자신의 클립: static/relative/sticky → `clip_flow`, absolute → `clip_abs`, fixed → 없음(뷰포트만).
  - **absolute/fixed 상자에 들어가면 그 자손의 `clip_flow` 를 다시 정한다**: absolute → `clip_flow = clip_abs`, fixed → `clip_flow = 없음`. 이게 없으면 바깥 static overflow 클립이 안쪽 글자에만 적용되어 "상자 배경은 보이는데 글자만 사라지는" 드롭다운이 생긴다.
  - **transform 조상**: transform 이 있는 상자는 absolute/fixed 의 포함 블록이 되므로 positioned 상자처럼 `clip_abs` 를 갱신한다(레이아웃이 이미 그렇게 포함 블록을 잡는지 먼저 확인하고 맞춘다).
  - `AnonymousBlock`(노드 없음, `layout/mod.rs` ~86행)은 부모 상자의 클립을 물려받는다 — 표는 `NodeId` 키라 익명 상자는 조회 시 가장 가까운 노드 조상으로.
  - 예: `div{overflow:hidden}`(static) > `div{position:relative}` > `span{position:absolute}` → relative 를 지날 때 `clip_abs` 가 이미 overflow 를 포함하므로 **잘린다**(실제 브라우저와 같음). `div{overflow:hidden}`(static) > `span{position:absolute}`(포함 블록이 바깥) → 안 잘린다.
- **transform**: 이미 `Scale`/`Rotate` 가 있다(`css/mod.rs` ~148–163행; `display_list.rs` ~254–305행 — rotate 는 `TransformGroup`, scale 은 사각형에 구워 넣음). 클립 표는 레이아웃 좌표(transform 전)로 만들고, **페인트 때** 상속 affine 으로 변환한다: translate·scale 이면 사각형을 같이 변환, 회전이 끼면 그 아래 클립은 **적용하지 않는다**(보여야 할 것을 숨기는 쪽보다 넘치는 쪽이 덜 나쁨) — 주석 + 테스트.
- **표시 목록**: 명령마다 클립을 붙이지 않고, 페인트 함수가 표를 조회해 **같은 클립이 이어지는 명령 묶음**을 `DisplayCommand::ClipGroup(Rect, Vec<DisplayCommand>)` 하나로 감싼다(z-레이어로 뽑혀 나간 상자도 자기 클립을 표에서 찾으니 위치와 무관). Push/Pop 짝 버그 없음.
- **래스터 — `Mask` 만으로는 안 된다**: tiny-skia `Mask` 는 `fill_rect`/`fill_path` 에만 먹는다(`raster/shapes.rs`, `image.rs`, `gradient.rs`). 그런데 **글자**(`blit_swash_mask`/`blit_swash_color`·비트맵 폴백, `raster/text.rs` ~366–510행), **그림자**(`shadow.rs` ~72행), `blend.rs` ~61행 `paint_through` 는 `pixmap.data_mut()` 에 직접 쓰는 바이트 루프라 마스크를 무시한다 → 걸친 클립(캐러셀)에서 글자가 안 잘린다. **결정**: 래스터에 `scissor: Option<IntRect>` 를 내려보내 바이트 루프는 쓰기 전에 사각형 검사, tiny-skia 경로는 같은 사각형으로 `Mask`(또는 경계 교집합). 클립이 사각형뿐이라 scissor 하나로 충분할 수 있다 — 구현 때 단순한 쪽.
- **래스터 빠른 길**: 그룹 안 명령의 경계 상자가 클립 안에 완전히 들어가면 scissor 없이 그리고, 완전히 밖이면 건너뛴다. 면적 0 클립(sr-only)은 그룹 자체를 생략 — 래스터 비용 0.
- **`clip: rect()` 일반 계산**: 패턴 매칭(`rect(0 0 0 0)` 등) 대신 네 변 값(`auto` = 그 변의 테두리 상자 가장자리)으로 사각형을 계산하고 면적 0 판정. absolute/fixed 에만.
- **히트 테스트**(`view.rs` `walk_for_hover` ~354행): 같은 `ClipTable` 로 클립 밖 지점은 그 상자의 **기록만** 건너뛰고 자손으로는 계속 내려간다(자손 absolute 는 다른 클립일 수 있다). 사람 입력과 에이전트 합성 클릭이 같은 경로라 둘 다 일관.
- **에이전트 클릭 대상 — 꼭 같이 고친다**: `mb-agent/src/browser.rs` 의 `target_rect`/`click_point`(~1018–1077행)는 레이아웃 상자를 그대로 쓰고 "크기 0" 만 본다. 히트 테스트만 클립을 보면, 클립된 요소를 누를 때 상자 중심이 그 요소를 건너뛰어 **밑에 있는 다른 요소를 조용히 누른다**. sr-only "건너뛰기" 링크(naver 8개, GitHub "Skip to content")는 1×1 이라 지금 "zero size" 검사도 통과한다. 그래서: `target_rect` = 상자 ∩ `ClipTable` 클립. 교집합이 비면 `NotInteractable { reason: "it is clipped out of view" }` + 힌트(포커스 후 Enter — `press` 도구 경로), 일부만 보이면 **보이는 부분의 중심**을 누른다.
- **에이전트 관찰의 "화면 안" 판정**: **지금처럼 a11y 식 포함 유지**. sr-only 텍스트는 실제 접근성 트리에도 있고(스크린 리더용으로 만든 것) 에이전트가 읽어야 할 정보다(예: GitHub "Navigation Menu" 제목, 아이콘 버튼의 이름). 클릭은 위 "에이전트 클릭 대상" 규칙대로 클립을 보므로, "관찰엔 보이지만 클릭은 거절" 이 일관된 에러로 나온다. 캐러셀 밖(클립 밖이지만 면적 있는) 요소를 "viewport 밖" 으로 칠지는 이번엔 **하지 않는다** — 스크롤 컨테이너 모델이 없어 에이전트가 그 요소로 갈 수단이 없고, 숨기면 정보만 잃는다. 결정 기록을 concept.md 에.
- 스크롤 가능한 컨테이너(`overflow: auto/scroll`)의 내부 스크롤은 범위 밖 — 자르기만 한다(내용 일부가 안 보이게 됨. 지금은 넘쳐 겹쳐 보이는데, 잘리는 쪽이 실제 브라우저의 첫 화면과 같다).

### 테스트 (3단계)

- `overflow_hidden_clips_child_paint` (render 단위, 픽셀): 10×10 `overflow:hidden` 안 20×20 빨간 자식 → (15,15) 흰색. `anti_alias=false` 주의(AGENTS.md tiny-skia).
- `sr_only_text_paints_nothing`: `.sr{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0)}` 안 텍스트 → 표시 목록에 `Text` 명령 없음.
- `clip_path_inset_50_hides_subtree`.
- `static_overflow_parent_does_not_clip_absolute_child`.
- `nested_clips_intersect`.
- `relative_wrapper_lets_overflow_clip_absolute_grandchild` (위 예시 첫 번째 — 트리 래핑이었다면 실패했을 경우).
- `fixed_child_escapes_overflow_clip`.
- `positioned_z_layer_keeps_its_clip` (z-index 있는 absolute 자식이 뽑혀 나가 그려져도 잘림).
- `click_outside_clip_misses` (view 히트 테스트).
- `body_overflow_hidden_does_not_clip_page` (`html,body{height:100%;overflow:hidden}` 아래 1000px 지점 텍스트가 그려짐).
- `absolute_box_resets_flow_clip_for_its_text` (바깥 static overflow 안의 absolute 드롭다운 — 배경과 글자가 같이 보임).
- `overflow_x_hidden_clips_both_axes`, `inline_overflow_hidden_does_not_clip`, `rotated_ancestor_skips_clip`.
- `percent_height_with_indefinite_parent_is_auto`, `percent_top_uses_height_basis`.
- `partially_clipped_text_is_scissored` (raster — 글자 바이트 루프도 잘림).
- mb-agent: `click_on_sr_only_link_errors_not_misclicks`, `click_partially_clipped_hits_visible_part`.
- mb-agent: `sr_only_text_still_observed` — 관찰 `text:` 에 sr-only 문장이 남음(결정 고정).

### 측정·완료 기준 (3단계)

- 코퍼스 슬리버(한 글자당 3px 미만) 중 **sr-only 패턴에 속한 것 0** — probe 가 슬리버마다 "조상에 클립 있음" 열을 찍도록 갱신. 남는 슬리버는 다른 원인으로 분류.
- naver "검색" 세로, GitHub "Navigation Menu" 세로, 위키 "주 메뉴" 세로가 미리보기에서 사라짐(전후 PNG).
- 관찰 요소·텍스트 수 변화 없음(a11y 포함 결정대로).

### 위험 (3단계)

- 클립 판정을 잘못하면 **보여야 할 것이 사라진다**(가장 나쁜 실패) — static 부모/absolute 자식 규칙이 핵심. 코퍼스 미리보기에서 "2단계 대비 사라진 텍스트" 를 probe 로 셀 수 있게(텍스트 상자의 클립 후 면적 0 개수).
- 래스터 마스크 비용 — 클립 그룹이 수백 개인 페이지(GitHub)에서 프레임 시간 확인.
- 표시 목록 구조 변경 → `ClipGroup` 처리가 필요한 곳을 **grep 으로는 다 못 찾는다**: `_ => None` 와일드카드로 평평하게 훑는 곳은 컴파일러도 안 잡는다 — `mb-mcp/src/overlay.rs` ~669행 `text_obstacles`(최상위 `Text` 만 봄 → 그룹 안 글자를 놓쳐 배지가 글자를 덮음), `view.rs` ~100·123행 `document_height`, 테스트의 `find_map(Text)`(render 17곳, main_tests 5곳). **결정**: 재귀 순회 헬퍼 `render::walk_commands(&[DisplayCommand], &mut impl FnMut(&DisplayCommand))` 를 먼저 도입해 이 곳들을 옮기고(`TransformGroup` 안도 같이 훑게 되어 기존 잠재 버그도 정리), 그 뒤 `ClipGroup` 추가. `scale_display_list` 는 클립 사각형도 같이 배율.
- `ClipTable` 은 레이아웃마다 다시 계산 — 노드 수만큼 한 번 걷기라 선형이지만 GitHub 크기에서 MB_PROFILE 로 확인.

**노력**: M (3~4일 — 퍼센트 높이 선행 수정, scissor 래스터, 순회 헬퍼, 에이전트 클릭 경로 포함).

**probe 변경**: 슬리버마다 "조상에 클립 있음" 열, 텍스트 상자의 클립 후 면적 0 개수(사라진 텍스트).

---

## 4단계 — 셀렉터

### 목표

지원 안 하는 의사 클래스 하나 때문에 **규칙 통째로**(콤마로 묶인 다른 셀렉터까지) 버려지는 것을 줄인다. 2차: `:is()` 127+, `:where()` 23, `:focus-visible` 107, `:checked/:disabled` 56+, `:focus-within` 1+. 그리고 `::before`/`::after` 가 섞인 콤마 목록 — 실제 브라우저에선 유효한 셀렉터라 같은 규칙의 다른 셀렉터가 살아 있는데, 우리는 `PseudoElement` 가 빈 enum(`css/selector.rs` 127행)이라 규칙 전체를 버린다.

### 세부 항목과 설계

| 항목 | 구현 | 의미 결정 |
|---|---|---|
| `:is()`, `:where()` | `css/selector.rs` 의 `SelectorParserImpl` 에 `fn parse_is_and_where(&self) -> bool { true }`. 매칭은 selectors crate 가 한다 | 명세 그대로(`:where` 명시도 0 — crate 가 처리) |
| `:focus-visible` | `NonTSPseudoClass::FocusVisible` → `state.focus == Some(id) && state.focus_visible` | **Chrome 휴리스틱**: 텍스트 입력·textarea·contenteditable 은 포커스되면 항상 참, 그 밖에는 **마지막 포커스 이동이 키보드**(Tab·화살표·`press`)였을 때만 참. `BrowserState` 가 포커스를 옮길 때 원인을 기록(결정적). "focus 와 같게" 는 GitHub 류의 `:focus:not(:focus-visible){outline:none}` 때문에 에이전트가 버튼을 클릭할 때마다 Chrome 에 없는 포커스 링이 스크린샷에 남아 기각 |
| `:focus-within` | `FocusWithin` → focus 노드가 자신 또는 자손 | `dom_select.rs` 의 `ancestor_chain_engaged` 와 같은 방식(조상 체인) |
| `:checked` | 속성 `checked` 존재(input checkbox/radio) 또는 `option[selected]`; `<select>` 에 `selected` 가 하나도 없으면 **첫 option** | 이 엔진은 현재 상태 = 속성 (spa-roadmap "IDL 속성 반사" 와 일치). 라디오 그룹 상호 배타가 클릭 경로에서 지켜지는지 확인 |
| `:disabled` / `:enabled` | `disabled` 속성이 있는 폼 컨트롤(`button input select textarea optgroup option fieldset`) + `fieldset[disabled]` 자손(첫 `legend` 제외는 생략) / 그 반대(폼 컨트롤만) | 스펙의 단순판 |
| `::before` `::after` `::placeholder` `::marker` `::selection` `::first-line` `::first-letter` `::backdrop` `::file-selector-button` + **모든 `::-webkit-*`**(Selectors 4 레거시 규칙: 모르는 `-webkit-` 의사 요소는 유효·매칭 없음) | `PseudoElement` 에 변형(+ `WebkitUnknown(String)`) 추가, `Parser::parse_pseudo_element` 구현(selectors 0.42 기본은 Err), `::-webkit-scrollbar-thumb:hover` 같은 모양을 위해 `accepts_state_pseudo_classes` → true. `match_pseudo_element` 는 이미 항상 `false`(`dom_select.rs` ~235–242행) — 그대로 | **유효하지만 아무것도 매칭 안 함** — 콤마 형제 셀렉터를 살리는 것까지만. 생성 콘텐츠는 범위 밖. `querySelector('a::before')` 는 빈 결과(브라우저와 같음). 목록 밖 접두사(`::-moz-*`)는 지금처럼 버림 = Chrome 과 같아 `browser-too` |
| `:has()` | **범위 밖** — selectors crate 의 `parse_has` 는 relative selector 캐시·무효화가 필요. 4차 후보 | |

- `css/mod.rs` ~377행 주석("`parse_is_and_where` / `parse_has` 를 켜지 않는다")도 같이 고친다.
- 각 의사 클래스의 `ToCss` 구현도 추가(직렬화 — `querySelector` 에러 메시지 등에서 씀).
- JS `matches`/`closest`/`querySelector` 도 같은 파서라 자동으로 지원됨 — 테스트 하나로 고정.

### 테스트 (4단계)

- `is_and_where_match` — `:is(.a,.b) p`, `:where(#x) p` 명시도가 `p` 보다 낮음.
- `focus_visible_true_for_text_input_click`, `focus_visible_false_for_button_click`, `focus_visible_true_after_tab`, `focus_within_matches_ancestors`.
- `is_forgiving_keeps_valid_arguments` (`:is(.a, :unknown) p` 가 `.a p` 로 동작).
- `select_without_selected_checks_first_option`, `radio_click_unchecks_group`.
- `webkit_pseudo_with_hover_keeps_rule` (`::-webkit-scrollbar-thumb:hover, .x{…}` 의 `.x` 가 살아남).
- `checked_matches_checkbox_attribute`, `disabled_and_enabled_on_controls`, `fieldset_disabled_disables_descendants`.
- `unsupported_pseudo_still_drops_rule` (`:has()` — 계약 고정).
- `pseudo_element_in_list_keeps_sibling_selectors` (`a::before, .x{color:red}` → `.x` 빨강), `pseudo_element_never_matches_element`, `moz_pseudo_element_still_drops_rule`.
- mb-runtime: `label_click_toggles_checked_style` — 위키 드롭다운 축소판(`<input type=checkbox id=t><label for=t>메뉴</label><div class=m>`, `.m{display:none} #t:checked ~ .m{display:block}`)에서 **label 을 합성 클릭** → 다음 프레임 스타일에서 `.m` 이 보임. 에이전트가 실제로 누르는 경로(라벨 → 체크박스 활성화 → 속성 → 재스타일) 전체를 고정.
- mb-runtime: `query_selector_supports_is_and_checked`.

### 측정·완료 기준 (4단계)

- 코퍼스 "버린 규칙(셀렉터)" 에서 위 의사 클래스 범주 **0**. 남는 범주는 `:has()`, `::-moz-*` 등 `browser-too`, `:target`·`:dir()` 등.
- 위키 드롭다운(체크박스 해킹 `:checked ~ .content`)이 접힌 상태로 보이고, 에이전트가 "주 메뉴" 라벨을 누르면 펼쳐짐(관찰에 메뉴 링크가 나타남).
- `::before/::after` 때문에 버려진 규칙 0(`ours` 기준).

### 위험 (4단계)

- `:is()` 가 살아나면서 GitHub 의 `:hover` 상태 규칙 등이 적용 — 관전 창에서 마우스 따라 모양이 바뀌는 범위가 늘어남(정상).
- 스타일 패스 비용: `:focus-within` 은 조상 체인 검사 — 노드마다 O(깊이). 포커스가 없으면 즉시 거짓으로 빠지게.

**노력**: S (1~1.5일).

---

## 5단계 — `calc()/min()/max()` + 값 속 `var()` + 그리드 트랙 함수

### 목표

GitHub 간격(`margin-top: calc(var(--base-size-8) + var(--Layout-row-gap))` 류 수백), Google·위키의 `calc(100% - 16px)`, 위키 그리드(`15.5rem minmax(0,1fr)` — 2차에서 본문이 밀리고 잘린 원인)를 살린다.

### 세부 항목

1. `calc()` — `+ - * /`, 괄호, 중첩 `calc()`, 단위 혼합(`px`, `%`, `em`, `rem`, `vw`, `vh`, `ch`, `pt`, 무단위 수).
2. `min()`, `max()`, `clamp()`.
3. 값 속 `var()` — 1단계 `Unparsed` 경로가 이미 처리(치환 후 재파싱 시 `calc` 가 여기서 파싱됨). 이 단계에선 `calc(var(--a) * -1)` 같은 조합의 테스트만 추가.
4. 그리드 트랙: `minmax(a, b)`, `repeat(n, …)`, `repeat(auto-fill|auto-fit, minmax(…))`, `fit-content()`.
5. `vw`/`vh` 단위 전반(지금은 `Keyword("100vw")` 로 떨어짐).

### 설계 결정

- **표현**: `Value::Calc(Box<CalcExpr>)`, `CalcExpr = Sum(Vec<(Sign, Product)>) | …` 대신 정규화된 형태 `CalcSum { px: f32, percent: f32, em: f32, rem: f32, vw: f32, vh: f32 }` + `Min/Max/Clamp(Vec<Calc>)` 노드. 곱·나눗셈은 파싱 때 숫자 쪽으로 접는다(`calc(2 * 8px)` → px 16; 두 길이의 곱은 명세상 무효 → 선언 버림).
- **`Value::Length` 직접 매칭이 많다 — 변경이 한 곳에 모이지 않는다**: `length_value` 외에 `taffy_bridge.rs` 12곳(`dimension` ~234, `dimension_with_intrinsic` ~243, `lpa_*` ~266–283, `lp` ~287, `number_value` ~336, `has_percent_inset_or_padding` ~722), `flex.rs` ~270·294, `grid.rs` ~142·197, `inline.rs` ~655–657, `table.rs` ~134·178, `display_list.rs` ~424·512·597·940, `view.rs` ~456. 특히 `matches!(…, Value::Length(..))` 로 "폭이 명시됨" 을 판정하는 곳은 `Calc` 를 `auto` 로 오판한다. **결정**: 5단계 첫 하위 커밋에서 `properties::length_like(node, name) -> Option<LengthLike>`(Length·Percent·Calc 를 한 타입으로) 헬퍼를 만들어 이 곳들을 옮긴 뒤 `Calc` 를 추가한다.
- **해석 시점**: `em`/`rem` 은 스타일 패스에서 이미 px 로 바꾸는 자리(`style/mod.rs` 의 font-size 해석)에서 함께 → 남는 건 `px + percent + vw/vh`. `vw/vh` 는 2단계에서 생긴 `MediaContext` 로 스타일 때 px 로. 그러면 레이아웃에 남는 건 `px + percent` — `layout/properties.rs::length_value(node, name, base)` 가 `Value::Calc` 를 받아 `px + percent/100*base` 로 계산(`min/max/clamp` 는 재귀). 레이아웃 코드의 다른 부분은 `length_value` 만 부르므로 변경이 한 곳에 모인다.
- **taffy 브릿지 — 전제 정정**: taffy 0.15.1 에는 `calc` 가 **있다**(기본 feature: `Dimension::calc(ptr)`, `LayoutPartialTree::resolve_calc_value(ptr, basis)`). 다만 우리가 쓰는 고수준 `TaffyTree` 는 `resolve_calc_value` 가 **0.0 고정**이다(`taffy_tree.rs` ~405행). 그리고 처음 안이었던 "boundary 로 보내 레거시가 계산" 은 불완전하다 — boundary leaf 도 자기 `width`/`min/max-width`/`margin` 은 `boundary_taffy_style`(~624–653행)이 taffy 로 넘기므로 calc margin 은 0, calc max-width 는 auto 가 된다. "기준 폭을 알면 브릿지에서 px 로" 도 `build_taffy_node`(~591행)가 compute 전에 스타일을 만들어 기준을 아는 경우가 거의 없다.
  - **5단계 시작 때 스파이크(0.5일)**: (a) 저수준 trait(`LayoutPartialTree` 등)을 우리 트리에 직접 구현해 `CalcSum` 포인터를 넘기고 taffy 안에서 `basis` 로 풀게 하는 안, (b) `TaffyTree` 를 유지하고 calc 가 있는 상자를 `px`/`%` 중 하나로 근사하는 안 — (a) 가 정답이지만 브릿지 재작성 규모를 재 보고 결정, 결과를 이 절에 기록.
  - 흔한 모양 `width: min(100% - 2rem, 1200px)` 을 가진 flex/grid 컨테이너가 기준 사례.
- **그리드**: `TrackSize` 는 `Copy` 라(`css/mod.rs` ~130행) `Box` 를 넣지 않는다 — `MinMax(TrackBreadth, TrackBreadth)`(Box 없는 min/max 전용 enum), `FitContent(f32)` 추가. `repeat(n)` 은 파싱 때 펼침, `repeat(auto-fill|auto-fit, …)` 은 `Value::TrackList` 모양을 바꿔(`TrackList{ before, auto: Option<(AutoKind, Vec<TrackSize>)>, after }`) 레이아웃 때 개수 결정 — 개수는 **gap 포함**, max 가 확정이면 max·아니면 min 으로 센다, `auto-fit` 은 빈 트랙을 접는다.
- **그리드 해석은 가능하면 taffy 로**: taffy 에는 `minmax`·`repeat(auto-fill)`·`fit-content` 와 스펙의 fr 알고리즘("find the size of an fr")이 이미 있다. 레거시 `grid.rs::resolve_grid_columns` 에 처음 안의 근사("min 을 고정 예산에 넣고 남는 공간을 fr 로 분배")를 넣으면 스펙과 다르다 — `minmax(300px,1fr) 1fr` 를 400px 에 두면 스펙 300/100, 근사 350/50. 또 `1fr` 단독은 `minmax(auto,1fr)` 라 긴 내용(pre·표)이 줄지 않는다. **결정**: 새 트랙 함수가 든 그리드는 taffy 경로로 보내고(`taffy_bridge.rs` grid 트랙 변환에 매핑), 레거시 `grid.rs` 확장은 `minmax(0, Nfr)`(위키 모양 — 근사로도 정확) 정도로 최소화.
- **퍼센트 기준은 속성마다 다르다** — `CalcSum.percent` 를 아무 데서나 "부모 폭" 으로 풀면 틀린다:
| 속성 | `%` 기준 | 해석 시점 |
|---|---|---|
| `width`, `margin-*`, `padding-*`(세로 포함), `left/right`, `text-indent` | 포함 블록 **폭** | 레이아웃(`length_value(…, base)`) |
| `height`, `top/bottom`, `min/max-height` | 포함 블록 높이(모르면 `auto` 취급 — 지금과 같음) | 레이아웃 |
| `font-size` | 부모 글꼴 크기 | **스타일** — 결과는 px |
| `line-height` | 자기 글꼴 크기 | **스타일** |
| `transform: translate()` | 자기 상자 | 페인트 |
| `flex-basis`, 그리드 트랙 | 컨테이너 내용 폭 | 레이아웃 |
  `font-size`·`line-height` 안의 `calc` 는 스타일 패스에서 완전히 px 로 접어 이후 단계에 `Calc` 가 남지 않게 한다(글꼴 크기는 자식 `em` 의 기준이라 레이아웃까지 미룰 수 없다). 속성→기준 매핑은 `length_value` 를 부르는 쪽이 이미 `base` 로 넘기므로, 새 코드는 "스타일 때 풀어야 하는 속성" 목록만 필요.
- `ch` 는 **이미 있다**(`style/mod.rs` ~231–305행, `Unit::Ch` = 0.5em, 테스트 있음). `ex` 도 0.5em 으로 같이. 0.5em 이면 `max-width:65ch` 본문이 Chrome 보다 10~20% 좁다 — 번들 폰트의 `0` 전진폭(monospace 는 0.6em)을 재는 개선은 결정적이라 가능하지만 이번엔 기록만.
- 뷰포트 단위 확장: `dvh/svh/lvh/dvw/svw/lvw` = 데스크톱에선 `vh/vw` 와 같음, `vmin/vmax` 도 같이(`min-height:100dvh` 흔함).
- 퍼센트 세부: `min-height`/`max-height` 의 % 가 확정 안 되면 각각 0/`none`(3단계 선행 수정과 같은 규칙), `flex-basis` % 는 **주축** 크기(column 이면 높이), `gap` % 는 그 축 기준, `line-height` 의 무단위 calc 결과는 길이가 아니라 **배율**로 상속.
- 0 으로 나누기·NaN → 선언 무효(파싱 때) 또는 0(레이아웃 때 기준이 0) — 0단계 불변식 2.
- `vw/vh` 를 해석하면 그 사실을 스타일 패스 결과에 표시 → 2.1 뷰 캐시 키의 `vh_height` 가 채워진다.

### 테스트 (5단계)

- 파서: `calc_mixed_units_parses`, `calc_nested_and_parens`, `calc_multiply_divide_fold`, `calc_length_times_length_is_invalid`, `min_max_clamp_parse`, `viewport_units_parse`(`vw vh dvh svh lvh vmin vmax`).
- 스타일: `font_size_calc_percent_uses_parent_font`(부모 20px, `calc(50% + 2px)` → 12px), `line_height_calc_resolves_at_style_time`.
- 상태: `height_resize_restyles_only_with_vh`(vh 안 쓰는 문서는 높이만 바꿔도 뷰 재사용, `100vh` 쓰면 재계산).
- 레이아웃: `width_calc_percent_minus_px` (`calc(100% - 16px)` in 200px → 184), `margin_calc_of_vars` (1단계 경로와 결합), `clamp_respects_bounds`.
- 그리드: `grid_minmax_fr_track`(위키 모양 축소판: `15.5rem minmax(0,1fr)` 두 열 → 248px + 나머지), `grid_minmax_min_not_added_to_fr_share`(`minmax(300px,1fr) 1fr` @400 → 300/100), `grid_repeat_n`, `grid_repeat_auto_fill_count_includes_gap`, `grid_auto_fit_collapses_empty`.
- 브릿지: `calc_width_on_flex_container_resolves`(스파이크 결과에 맞춰), `explicit_width_check_accepts_calc`(`matches!(Length)` 오판 회귀).
- `line_height_unitless_calc_inherits_as_factor`, `flex_basis_percent_uses_main_axis`.
- 퍼저 CSS 풀에 `calc()`/`minmax()` 추가 — 0 나누기·거대값.

### 측정·완료 기준 (5단계)

- 코퍼스 "버린 선언" 에서 `calc`/`min`/`max`/`var` 원인 **0**; 총 버림(`ours` 합산)이 0단계 새 기준선 대비 **95% 감소**.
- 위키 미리보기: 본문이 왼쪽 사이드바 오른쪽에 붙어 뷰포트 안에 들어옴(2차 "오른쪽으로 밀려 잘림" 해소), 가장 넓은 상자 ≤ 뷰포트 폭.
- GitHub 가장 넓은 상자 187M px → 뷰포트 폭 수준(원인이 calc 가 아니면 0단계 퍼저/조사로 분리).

### 위험 (5단계)

- taffy/레거시 경계 이동 — calc 가 든 상자가 boundary 로 빠지면 flex/grid 안에서 배치 모양이 바뀔 수 있다. 코퍼스 전후 미리보기.
- `Value` 변경 범위 큼(1단계 `Unparsed` + 여기 `Calc`) → 1단계 끝에 `Value::Var` 제거 여부를 정하고 여기서 같이 정리.

**노력**: M (4일 — taffy calc 스파이크 0.5 + `length_like` 이주 0.5 포함; 스파이크가 (a) 브릿지 재작성으로 가면 +2일, 그때 일정 재산정).

---

## 진행 표

버린 선언·규칙 목표(≤1,500 · ≤380 등)는 [공통 규칙](#범위와-순서)대로 `ours` 기준이다 — 0단계에서 기준선을 둘로 나눈 뒤 목표 수치도 그 비율로 다시 확인한다.

단계를 마칠 때마다 이 표와 [compat-survey.md](compat-survey.md) 의 사이트별 수치를 갱신한다. 수치는 `scripts/compat-corpus.sh run` 의 `corpus-report.tsv` 합계(코퍼스 전체)와, 비교용으로 2차 5곳 합계를 따로.

| 단계 | 상태 | 커밋 | 테스트 수 | 코퍼스 fail/panic/timeout | 버린 선언 (5곳) | 버린 규칙 (5곳) | sr-only 슬리버 | 에이전트 컨트롤 누락 사이트 | 메모 |
|---|---|---|---|---|---|---|---|---|---|
| 2차 기준 | ✅ | `78bedff`·`4588aba` | 804 | (5곳) 0/0/0 | 7,593 (ours/browser-too 분리는 0단계에서) | 1,931 (같음) | 19 (GitHub 17·위키 2; naver "검색" 은 2글자라 probe 기준 밖) | naver(모듈) | 출발점 |
| 0 견고성 | ⏳ | | | 목표 0/0/0 (전체, release) | 새 기준선(ours/browser-too) | 새 기준선 | 새 기준선 | | 0a·0b·0c, 코퍼스 동결 |
| 1 값 파서 | ⏳ | | | 0/0/0 | 목표 ours −80% (2차 환산 ≤1,500) | — | — | | 2.1 뒤에 실행 |
| 2 @media/@layer/@supports | ⏳ | | | 0/0/0 | — | — | — | | 2.1 먼저, 2.2·2.3 은 1 뒤. GitHub 라이트 |
| 3 클리핑 | ⏳ | | | 0/0/0 | — | — | 목표 0 | | |
| 4 셀렉터 | ⏳ | | | 0/0/0 | — | 목표: 대상 범주 0 | — | | |
| 5 calc/트랙 | ⏳ | | | 0/0/0 | 목표 ours −95% (2차 환산 ≤380) | — | — | | 위키 레이아웃, taffy 스파이크 결과 |

"에이전트 컨트롤 누락 사이트" = [compat-survey 에이전트 관점 표](compat-survey.md#에이전트-관점-요약) 처럼 사이트마다 사람이 기대하는 주요 컨트롤(검색창·검색 버튼·주 내비·본문 링크)을 관찰이 찾는지. 코퍼스가 커지면 사이트별 기대 컨트롤 목록을 `compat-cache/sites.tsv` 옆 열로 적고 probe 가 이름 부분 일치로 자동 판정한다(목록은 사람이 처음 한 번 작성).

미리보기 전후 PNG 는 `compat-cache/previews/<단계>/<사이트>.png` 에 남긴다(git-ignored). 문서에는 관찰한 변화를 글로만 적는다.
