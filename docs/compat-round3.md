# 호환성 3차 설계 — 견고성 + CSS 파서·캐스케이드

> 상태: **설계 확정 (2026-11-26, 1차 검토 + 다각도 검토 — 코드 대조·웹 표준·실행/측정 — 반영) · 0a 완료(`d6801bb`, 829) · 0b 완료(`15775d6`, 842) · K4 완료(`0e0ccc9`, 843) · 0c 퍼저 완료(미커밋, 862) → 0단계 완료** (K5 nav-fail 1 은 [부록 B](#부록-b--0b-코퍼스에서-찾은-것-알려진-문제--담당) 대로 4차로 넘김) — **2.1 `@media` 평가 완료(`8904659` probe · `4bd8146` 엔진, 902 — [구현 메모](#21-구현-메모-2026-11-27--설계와-달라진-점-포함)) → 2.1a 위키 그리드 회귀 수정(`873808c`, 915 — [메모](#21a-위키-그리드-회귀-수정-2026-11-27))** → **1단계 P probe(`e470230`, 919 — [메모](#1단계-구현-메모-2026-11-27)) → 1r `<html>` 문서 요소(`9de59c9`, 932) → 1a `!important`(`80766d8`, 943) → 1p 퍼센트 높이(`1ca47b2`, 955) → 1q scrollHeight·단위 없는 0(`994d8c8`, 961) → 1b 값 파서(`35f1218`, 980) → 1c 커스텀 속성·`var()`(미커밋, 996) → **1단계 완료**(합산 지표 5곳 738 ≤ 1,478)** → 다음 2.2·2.3 · 근거 = [compat-survey.md](compat-survey.md) (호환성 2차 실사이트 조사) · 기준선 `4588aba` 804 → 조사 캐시 2차 수정 후 **806** → 0a **829** → 0b **842** → K4 **843** → 0c **862** → R1 **876** → 2.1 **902** → 2.1a **915** → 1단계 P **919** → 1r **932** → 1a **943** → 1p **955** → 1q **961** → 1b **980** → 1c **996** · 진행은 [진행 표](#진행-표)

이 문서만 읽고 어느 단계든 이어받을 수 있게 쓴다. 단계마다 **목표 → 세부 항목 → 설계 결정 → 테스트 → 측정·완료 기준 → 위험 → 노력** 순서다. 2차 조사의 수치·용어(슬리버, 버린 선언, `compat_probe`, `MB_NET_CACHE`)는 [compat-survey.md 방법](compat-survey.md#방법) 참고.

## 범위와 순서

| 단계 | 내용 | 왜 이 순서 | 노력 |
|---|---|---|---|
| **0** | 견고성 점검(불변식·코퍼스·퍼징·`?`/unwrap 검토) + 스크립트 에러 위치 | 2차 버그 6개 중 4개가 "페이지 전체 실패/패닉/무한 시간". CSS 를 고치기 전에 같은 종류가 더 없는지 체계적으로 걸러야 이후 측정이 의미 있다. 에러 위치는 이후 모든 진단을 빠르게 한다. 하위 커밋 **0a**(에러 위치·불변식·코드 검토) → **0b**(코퍼스) → **0c**(퍼저) | M (3~4일) |
| **1** | `!important` + 값 파서 묶음 | 5/5 사이트, 버린 선언의 대부분. 가장 싸고 넓다 | M (3~4일) |
| **2** | `@media` 평가 → `@layer` 펼치기 → `@supports` 펼치기 | 5/5, GitHub 다크 모드. `@layer` 는 반드시 `@media` 평가 **뒤** ([이유](#22-layer-펼치기)). **2.1 은 1단계보다 먼저 실행**(아래 실행 순서) | M (3~4일) |
| **3** | overflow 클리핑 (+ `top/bottom` % 기준 — 높이 % 는 [1p](#1p--퍼센트-높이-기준-2026-11-28) 로 당김) | 4/5 사이트의 sr-only 세로 글자 | M (3~4일) |
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

**확정(0b, 2026-11-26 녹화)**: 위 25곳 그대로 `scripts/compat-sites.tsv` 에(실제 URL: 위키 문서·`stackoverflow.com/questions/11227809/…`·`www.microsoft.com/en-us`·`d2.naver.com/home`). robots 금지로 **녹화 안 함 2곳** — `www.11st.co.kr`, `old.reddit.com`(둘 다 `User-agent: *` 에 `Disallow: /`). 녹화했지만 **403 차단 2곳** — stackoverflow, w3.org. 결과는 [부록 B](#부록-b--0b-코퍼스에서-찾은-것-알려진-문제--담당).

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

#### 0b 구현 메모 (도구 — 설계와 달라진 점 포함)

- **경로**: 캐시 `compat-cache/net/`(새로 녹화, 3차 동안 동결 — 2차 캐시는 `compat-cache/round2/` 로 옮겨 섞지 않음). 녹화 기록 `compat-cache/corpus/recorded.tsv`(이름·URL·robots 판정·근거 규칙·HTTP 코드·UTC 날짜·결과), robots 원문 `compat-cache/corpus/robots/<이름>.txt`, 실행 결과 `compat-cache/corpus/<라벨>/<이름>/`(probe 출력 + stdout/stderr) 와 **`compat-cache/corpus/<라벨>/corpus-report.tsv`**(위 설계의 `compat-cache/corpus-report.tsv` 대신 라벨별 — 단계 전후 실행을 나란히 두려고). `run [라벨]`(기본 `latest`), `diff 라벨A 라벨B` = 사이트별 요소 줄 diff(`elements.txt` 를 정렬해 `comm`, 사라진 `-`/새 `+`).
- **robots**: 스크립트가 `curl` 로 사이트당 1회, `*` 그룹만(UA 에 식별 토큰이 없으므로), RFC 9309/Google 규칙 — 가장 긴 패턴이 이기고 같으면 Allow, `*`·`$` 지원. 4xx = 제한 없음, 5xx·네트워크 실패 = 전부 금지(보수적). robots 요청과 문서 요청 사이에도 2초.
- **한 번만**: `recorded.tsv` 에 있는 사이트는 `record` 가 다시 방문하지 않는다(동결). 보충 녹화는 `record --supplement 이름…` 로만(robots 허용이었던 사이트만) — 위 보충 녹화 규칙을 지키는 건 사람 몫.
- **`run` 은 항상 오프라인**: 스크립트가 `MB_NET_CACHE_OFFLINE=1` 을 직접 export 한다(설계의 명령줄에 안 붙여도 네트워크에 안 나감). `record` 는 반대로 그 변수가 있으면 거부.
- **GET 만**: 캐시가 켜져 있으면(녹화·재생 모두) GET 이 아닌 요청은 보내지 않고 실패, 개수를 `non_get_refused` 열에. 오프라인 미스는 `offline_miss`·`miss_kinds`(확장자로 css/js/img/font/other) 열과 `summary.txt` 의 `miss:` 줄(쿼리 제거). 둘 다 `net::survey_stats()` — 프로세스 전역 카운터, probe 만 읽는다(결정성 무관: 관찰·행동엔 안 들어감).
- **결과 판정**(`summary.tsv` 의 `result`): 오류 페이지(현재 URL 없음)면 `nav-fail`, 그 문구가 `http status 401/403/429/503` 이면 `blocked` → settle 이 20초 벽시계를 넘겼으면 `settle-timeout` → 오프라인 미스가 있으면 `ok*` → `ok`. 스크립트가 종료 코드 124 → `timeout`, 101·134(스택 오버플로 abort) → `panic` 으로 덮어쓴다. robots 금지는 `skipped`, 녹화 안 된 사이트는 `not-recorded`.
- **열**(`compat_probe --header` 가 정본 — 스크립트가 앞에 `name · robots · budget_lines` 를 붙임): `url result load_ms elements viewport_elements text_blocks decl_drop_ours decl_drop_browser rule_drop_ours rule_drop_browser slivers script_errors console_errors offline_miss miss_kinds non_get_refused controls_found controls_missing decl_kept rule_kept keyword_fallback parse_only at_skipped_ours at_skipped_browser at_skipped_bytes import nesting media decl_reasons css_bytes document_height final_url`. `script_errors` = `Uncaught …` 콘솔 줄, `console_errors` = error 수준 전부. `load_ms` = `navigate`(settle 포함) 벽시계.
- **IACVT 열은 1단계 probe 변경으로 미룸**: 지금 엔진엔 "계산값 시점 무효" 가 없다 — 풀리지 않은 `var()` 는 `Keyword("initial")` 로 바뀔 뿐(`style/variables.rs`)이고 `Unparsed` 가 아직 없어 선언 단위로 셀 지점이 없다. 1단계 ①(probe 커밋)에서 `Unparsed` 치환 실패 수를 이 표에 넣는다. → **1단계 P 에서 추가**(`iacvt`·`var_keyword_fallback`·`decl_drop_total` 열 — [1단계 구현 메모](#1단계-구현-메모-2026-11-27)). 옛 엔진의 IACVT = 값 전체가 `var()` 인데 못 풀린 것(정의 없음·폴백 없음, 또는 순환).
- **기대 컨트롤**: `scripts/compat-sites.tsv` 셋째 열 `역할:이름부분;…`(`a|b` = 둘 중 하나 — IP 로 언어를 고르는 사이트용). 전체 페이지 관찰에서 역할 일치 + 이름 부분 일치(대소문자 무시)면 발견.
- Chrome 스크린샷(설계 "녹화 시점에 `compat-cache/chrome/`")은 **이번엔 생략** — 쓸 수 있는 브라우저 도구가 화면을 파일로 저장하지 못하고, 찍으려면 사이트를 한 번 더 방문해야 해서(예의 규칙 위반). 사람 비교가 필요하면 그 단계에서 사람이 직접.

#### 0b 분류표 (동결 — 바꾸려면 이 표를 먼저 고친다)

버림 사유는 `css/report.rs` 의 `DropReason`, ours/browser-too 는 `DropReason::origin`(1:1 고정). "아는 속성" = `css/properties.rs` 의 표(Chrome 이 받는 표준 속성 전부 + 커스텀 `--*` + 표에 있는 속성의 `-webkit-` 별칭 + Chrome 의 `-webkit-` 전용 속성 목록). 선언 사유는 아래 순서로 처음 맞는 것:

| 대상 | 사유 코드 | 판정 기준 | 분류 |
|---|---|---|---|
| 선언 | `nested-rule` | 블록 안 중첩 규칙(원문에 `{`) — `&:hover{…}`, `.child{…}` | ours (선언 수에서 빼고 `nesting` 열에만) |
| 선언 | `unknown-property` | 속성 표에 없음 — 오타, `-moz-`/`-ms-`/`-o-`/`-khtml-`, 모르는 `-webkit-`, `*zoom` 같은 IE 이름 핵 | browser-too |
| 선언 | `legacy-hack` | 아는 속성 + 값에 `progid:`·`expression(`·`\9`·`\0`·`!ie` | browser-too |
| 선언 | `important` | 값이 `!important`(`!` 와 단어 사이 공백·주석 허용)로 끝나고, 그것을 뗀 선언은 파서가 받는다 — 플래그 자체가 버림의 원인(**1a 에서 고침**: 전엔 `!important` 가 든 버림 전부였다. 1a 뒤엔 0 이어야 하고, 뗀 값도 안 받히면 그 값으로 `multi-token`/`invalid-value`; `!important` 뒤에 토큰이 더 있으면 `invalid-value`) | ours |
| 선언 | `multi-token` | 전용 파서 없는 속성인데 최상위 토큰 2개 이상(함수·괄호 = 1개) | ours |
| 선언 | `invalid-value` | 그 밖(전용 파서가 거절, 또는 토큰 1개를 아무도 못 받음) — Chrome 이 받을지 모름 | ours (애매하면 ours) |
| 규칙 | `foreign-selector` | 셀렉터에 `::-moz-`·`:-moz-`·`::-ms-`·`:-ms-`·`::-o-`·`:-o-` (Chrome 도 규칙째 버림) | browser-too |
| 규칙 | `unsupported-selector` | 그 밖 — `::-webkit-*` 도 여기(Chrome 은 상당수를 받음) | ours |
| at-rule | `foreign-at-rule` | 이름이 다른 엔진 접두사(`@-moz-document`) | browser-too |
| at-rule | `skipped-at-rule` | `@media` 외 본문을 건너뛴 at-rule(`@supports`·`@layer`·`@font-face`·`@keyframes`·`@import`…, `@charset` 은 cssparser 가 먹어서 안 옴) | ours |

- **`keyword_fallback`**: 남은 선언 중 속성 표의 값 종류가 `Other` 가 아닌(길이·숫자·색·키워드 목록·이미지·전용 파서) 속성인데 값이 그 종류가 받지 않는 `Keyword`(전역 키워드 `inherit/initial/unset/revert/revert-layer` 제외) — `width: 100vw`, 따옴표 문자열 등.
- **`parse_only`**: 남은 선언 중 아래쪽(레이아웃·페인트·인덱스)이 아무도 안 읽는 속성(probe 의 `CONSUMED` 목록 밖, `--*` 제외). 1단계에서 `transition`/`animation` 이 "파싱만" 으로 살아나면 버림이 이 열로 옮겨 간다.
- 토큰이 하나인 모르는 속성(`-moz-appearance: none`)은 지금 파서가 **버리지 않고 보관**한다 — 버림이 아니라 `parse_only` 로 잡힌다(정상: 아무도 안 읽음).
- 2차 기준선(7,593 / 1,931)은 다른 날 스냅샷이고 2차 캐시는 요청 정보 줄이 없어 재생이 안 되므로 **둘로 나눠 다시 적지 않는다** — 새 캐시의 같은 5곳 합계가 대신한다(아래 [진행 표](#진행-표) 0행).

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

**0c 구현 메모 (설계와 달라진 점 — 실측·발견 때문)**:
- **파일 하나, 테스트 6+3개** (`crates/mb-engine/tests/fuzz_layout.rs`). 기본 `cargo test`: `layout_fuzz_default_seeds_hold_the_invariants`(시드 200, 케이스당 5초 상한) · `css_token_fuzz_never_panics`(500개) · `image_bytes_fuzz_never_panics_or_balloons`(300개, D2) · `ten_thousand_levels_of_nesting_do_not_overflow_the_stack`(D1) · 생성기·축소기 자체 검사 2개(`fuzz_generator_covers_its_spec` — 깊이 ≥30·두 값 풀·영역 불일치·한글이 실제로 나오는지, `shrinker_reduces_to_the_failing_part`). `#[ignore]` 긴 실행 3개: `layout_fuzz_long_run`·`css_token_fuzz_long_run`·`image_bytes_fuzz_long_run`(1초 판정 추가). 환경 변수: `MB_FUZZ_CASES`(기본 1만)·`MB_FUZZ_SEED`(시작 시드, 기본 1)·`MB_FUZZ_FONTS=1`(macOS 시스템 폰트를 깔아 cosmic-text 셰이핑 경로로 — 없으면 조용히 장난감 측정)·`MB_FUZZ_DUMP=1`(이미지 입력을 돌리기 **전에** 찍음 — abort 는 보고 없이 프로세스를 죽이므로). 긴 실행은 `--test-threads=1` 로(동시 실행이면 1초 판정이 흔들림).
- **케이스마다 스레드 하나** + `recv_timeout` 으로 상한(무한 루프여도 테스트가 멈추지 않고 실패로 끝남). 패닉 훅은 퍼즈 케이스 구간에서만 조용히(위치만 기록) — 처음엔 전역으로 껐다가 테스트 자신의 단언 메시지까지 삼켜서 고침(`quietly`).
- **값 풀 둘 — 1e7px 판정이 의미 있게**: 첫 실행에서 "너무 넓음" 판정이 **정상 CSS** 에서도 났다 — 중첩 `margin: -52%` 는 단계마다 폭이 ~2배(Chrome 도 같음), `padding: 16646px` 를 단 인라인 상자 300개짜리 `nowrap` 줄은 정말로 1e7px. 그래서 케이스의 80% 는 **유계 풀**(길이 ≤ 1500px·음수 % 는 −5% 까지·상대 글자 크기는 줄어들기만(50–100%, `1em`)·% 폭 ≤ 120%) — 정상 배치로는 40단계에서도 1e7 에 못 닿으니 넘으면 엔진의 증폭 버그. 20% 는 **무계 풀**(`1e9px`·`1e30px`·`3e38px`·`1e39px`(f32 넘침 → inf)·`1000%`·`20em`·`repeat(100000, …)`·`span 1000000000`·`-60%`·`20000px`) — NaN/inf·할당·시간만 보고 폭 판정은 건너뜀. 무계 여부는 케이스 텍스트의 표식(`e39`·`1000%`…)으로 판정해서 축소 중에도 다시 분류된다. 폭 판정은 `|폭| > 1e7`(음수 폭 증폭도 잡으려고 — 부록 C F2).
- **축소기**: 규칙 제거 → 노드 제거 → 노드 풀기(자식으로 대체) → 선언 하나 → 속성 하나 → 텍스트 반으로, 같은 **종류**(패닉·비유한·너무 넓음·느림)의 실패가 남는 첫 후보를 받는 탐욕법, 2만 걸음 상한. 한계: 증폭은 깊이에 비례해서 커지므로 단계를 빼면 1e7 아래로 내려가 축소가 일찍 멈춘다(F2·F3·F11 은 축소 결과를 보고 손으로 줄임 — 빠진 큰 케이스를 다시 그리는 `zz_repro` 류 임시 예제로; 커밋 안 함).
- **CSS 토큰 퍼징 강화**: 처음 버전은 셀렉터 자리도 토큰열이라 규칙이 거의 다 버려져 값 파서까지 못 갔다(10만 입력에 살아남은 선언 1만). 셀렉터는 75% 정상(`div`, `.a`, `p span` …), 값은 70% 가 속성별 **거의-정상 템플릿**(그리드 트랙·그림자·변환·그라디언트·`flex`·`font` …)의 숫자를 이상한 값(`1e39`·`-1`·`1e-39`·`2147483648`)·단위(`qq`·`fr`·`deg`)로 바꾸고 가끔 토큰 하나·`!important` 를 끼운 것 → 10만 입력에 **27만 선언**이 파서를 통과. 기본 테스트가 "500개에 100선언 이상" 을 단언해서 다시 헛돌지 않게. 살아남은 시트는 고정 작은 페이지에 적용해 캐스케이드·레이아웃·페인트·**래스터(400×300)** 까지 — 래스터는 0.3 파이프라인 밖이지만 에이전트 스크린샷·창이 지나는 길이라 넣었다(부록 C F9).
- **이미지(D2)**: 구조적·저비용 — SVG 템플릿(크기 `0`/`-1`/`1e9`/`1e39`/`NaN`/`100%`, `viewBox`, `scale(1e30)`, `r="1e30"`, `stdDeviation="1e6"`, `<use>` 자기참조, `font-size="1e9"`) + PNG(IHDR 에 이상한 크기, **CRC 를 맞춰** 디코더가 크기 검사까지 가게)·GIF·BMP·JPEG 헤더 + 꼬리 잡음/자르기/비트 뒤집기. 판정: 패닉·시간·`pixels.len() == w*h`. 래스터 디코더 자체는 `image` crate 몫(상류에서 퍼징됨) — 우리가 보는 건 **크기를 주장하는 헤더가 할당·시간으로 이어지는 글루**. 기본 테스트는 "300개 중 30개 이상 실제 디코드" 를 단언(처음엔 잡음이 SVG 를 다 깨뜨려 12개뿐이었음).
- **D1 케이스는 자식 프로세스에서**: 깊이 10,000 은 스택 넘침 = abort 라 `catch_unwind` 가 못 잡는다. 같은 테스트 바이너리를 이 테스트 이름으로 다시 띄우고(`MB_FUZZ_DEEP_CHILD=1`) 부모가 종료 코드를 본다. 자식은 **8MB 스레드**(브라우저가 실제로 도는 mb-mcp·mb-shell 메인 스레드 크기). 깊이 상한을 10만으로 바꿔 돌리면 `has overflowed its stack`/SIGABRT 로 실패하는 것을 확인함. 감싸개는 블록 위주(`div`·flex·grid·`ul`·`form`·`section`) — 인라인 감싸개는 O(n×깊이)([부록 C](#부록-c--0c-퍼저에서-찾은-것) R1)라 이 테스트만 수십 초가 됨.
- **측정(release, `--test-threads=1`, 2026-11-27, 모든 수정 뒤 최종)**: 레이아웃 **15만 케이스 139.3초**(0.93ms/케이스, 최장 84ms, 무계 풀 28,460개, 최대 트리 770노드) 실패 0 · CSS 토큰(+래스터) **15만 입력 16.8초**(선언 409,491개 통과) 패닉 0 · 이미지 **15만 입력 71.6초**(22,749개 디코드, 최대 4096×4096, 최장 374ms) 실패 0 · 실제 폰트 셰이핑(`MB_FUZZ_FONTS=1`) 레이아웃 **10만 케이스 184.1초**(1.84ms/케이스, 최장 127ms) 실패 0 · debug 빌드(넘침 검사 켜짐) 레이아웃 1만(최장 194ms)·CSS 2만 무결. 수정 전 첫 2만 실행은 실패 서명 상한 10개(NaN/inf 9 + 증폭 1)를 3초 만에 채우고 멈췄고, F11 은 실제 폰트 5만 실행에서만 나왔다(장난감 측정은 같은 시드에서 폭이 덜 커짐).

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

**0a 구현 메모 (설계와 달라진 점 — 실측·코드 제약 때문)**:
- **열 위치 실측**: 위 "열은 `(` 다음" 은 일부만 맞다. rquickjs 0.11(quickjs-ng)에서 `not a function` 의 열을 재 보면 호출 모양마다 다르다 — 최상위 `document.nope()` → `.`(1:9), 함수 본문 안 `window.nope()` → 속성 이름 첫 글자, `foo()` → 이름 **앞** 공백, `a.b.c (1)` → 이름 뒤 공백, `foo( 2 )` → `(` 뒤(설계가 말한 경우). 그래서 `near` 는 세 갈래로 찾는다: ① 열 앞(공백 건너뜀)이 `(` 면 그 앞 식 ② 열에서 시작하는 멤버/이름 줄이 `(` 로 이어지면 그 줄 ③ 아니면 열 주변의 멤버/이름 줄(`(0, document.qq)()` 류). 60자 넘는 사슬은 꼬리만. `near` 는 `not a function`·`not a constructor` 메시지에만 붙인다(`cannot read property 'x'`·`x is not defined` 는 메시지가 이미 이름을 말함).
- **`<input>` 프레임**: rquickjs 는 `eval`·`new Function`·**우리 부트스트랩 JS** 를 모두 `<input>` 으로 라벨한다. 첫 프레임 그대로 쓰면 부트스트랩 shim 이 던진 에러가 페이지 대신 `<eval>` 을 가리키므로, 아래에 페이지 프레임이 있으면 그걸 쓰고 페이지 프레임이 하나도 없을 때만 `at <eval>`(줄·열 없음).
- **원문 맵 위치**: 설계는 `BrowserState` 보관이었지만 리스너·타이머 에러는 JS→Rust 콜백(`__mb_report_error_native`) 안에서 포맷되어 `BrowserState` 에 닿을 수 없다. 그래서 `JsRuntime` 이 라벨 → 원문 맵(`Rc<str>`)을 들고 콜백과 공유한다. 런타임은 문서마다 새로 만들어지므로 "문서 교체 때 비움" 은 그대로 성립. 조회만 하고 순회하지 않음(결정성).
- **반환형**: `execute_with_url` → `Result<String, Box<ScriptError>>` (완료 값 문자열은 기존 테스트가 써서 유지, `Box` 는 clippy `result_large_err`). `ScriptError::summary()` = 콘솔 줄, `Display` = stderr 줄(+`MB_JS_STACK` 이면 원 스택).
- **라벨**: 인라인은 `{쿼리 뺀 문서 URL}#inline-{n}`, n 은 인라인 스크립트만 세는 0부터 카운터(전엔 `#inline-script-{1부터}`). 외부 스크립트 라벨은 **원래 `src` 값 유지**(이전 결정 — HTML 에서 grep 가능; 표시할 때 쿼리만 뗌). 라벨 문자열이 QuickJS 의 `filename` 이자 원문 맵 키.
- **리스너 메시지 모양**: `String(err)` 그대로라 `TypeError: not a function …` 처럼 에러 이름이 붙는다(최상위는 `exc.message()` 라 이름 없음). Tier 1.5 #18 의 모양을 바꾸지 않으려고 둠.
- 측정: 최상위 `Uncaught in script: not a function (near "document.nope") at http://…/script-error#inline-0:1:9`, 리스너 `Uncaught in click listener: TypeError: not a function (near "window.nope") at about:blank#inline-1:2:10`.
- **`prompts.rs` 결정**: 0a 에선 안 바꿨다(사용자 결정 사항). 권고 = **지금은 넣지 않기** — 현재 eval 시나리오 페이지(agent-demo)엔 스크립트 에러가 없어 새 콘솔 줄이 기존 eval 에 영향이 없는데, 안내 한 줄을 넣으면 `prompts` 지문만 갈려 기준선을 다시 재야 한다. 0b 코퍼스로 사이트별 `console:` 에러 수를 잰 뒤, 제3자 에러가 있는 손으로 쓴 eval 페이지로 "LLM 이 페이지 디버깅으로 빠지는지"를 한 번 보고 넣을지 정한다.

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

### 1단계 구현 메모 (2026-11-27)

네 부분으로 나눠 부분마다 멈추고 부모 세션이 검증·커밋한다: **P**(probe) → **1a**(`!important`) → **1b**(값 파서) → **1c**(커스텀 속성·`var()`).

#### P — probe 변경 + "전" 측정

**설계와 달라진 점**: `Keyword` 폴백(`keyword_fallback`)·"파싱만"(`parse_only`) 열은 0b 에 이미 있다([0b 구현 메모](#0b-구현-메모-도구--설계와-달라진-점-포함)) — 그대로 쓴다. 새로 넣은 것은 **계산값 시점** 두 열과 합계 열:
- **엔진**(`style/mod.rs`): `style::style_tree_with_report(…) -> (StyledNode, StyleReport)` — `style_tree_with_state` 와 같은 패스에 `Cascade::report: Option<&RefCell<StyleReport>>` 만 붙인 것(브라우저 경로는 `None`, 비용 0 — 같은 트리임을 테스트로 고정). `StyleReport { iacvt, var_keyword_fallback }` 은 `style::declaration_key(이름, 선언된 값)`(= `"{name}: {value:?}"`) 의 집합 — 노드 하나에서라도 그렇게 된 선언. 기록 지점은 `variables.rs::resolve_var_references` 한 곳: 치환 실패(정의 없음 + 폴백 없음, 순환) → `iacvt`, 치환이 그 속성의 값 종류가 안 받는 `Keyword` 를 남김(`--w:100vw` → `width:var(--w)`, 판정은 정적 `keyword_fallback` 과 같은 `properties::lookup`·`accepts_keyword`) → `var_keyword_fallback`. 1c 에서 `Unparsed` 재파싱 실패도 같은 함수에서 기록한다(엔진 커밋이 이 함수 본문을 바꾸는 것 — 2.1 의 `media_verdict` 와 같은 방식).
- **probe**: 로드 뒤 마지막 프레임과 같은 스타일 패스(기본 상호작용 상태, 1280×800)를 `style_tree_with_report` 로 한 번 더 돌리고, 시트의 남은 선언 중 키가 보고에 있는 것을 센다. 같은 규칙 안의 같은 키는 한 번만(1c 에서 shorthand 가 longhand 넷으로 펼쳐져도 선언 하나). 열 `iacvt`·`var_keyword_fallback`·**`decl_drop_total`** = `decl_drop_ours + iacvt + keyword_fallback + var_keyword_fallback`([측정 프로토콜](#범위와-순서)의 합산 지표 — 1단계 목표는 이 열로 본다). `css.txt` 에 두 목록(속성별 수·견본).
- 테스트 +4(`style_report_counts_unresolved_var_as_iacvt`·`…_cyclic_var_as_iacvt`·`…_substituted_keyword_the_property_rejects`·`style_tree_without_report_matches_with_report`) — 915 → **919**. 새 문법이 없어 퍼저 풀은 그대로.

**"전" 측정**(라벨 `step1-before`, 동결 캐시·오프라인·release, 엔진 = `873808c` + probe): 기존 열은 `grid`(2.1a) 와 `load_ms` 외 **25곳 모두 같다**(결과·버림·요소 수·기대 컨트롤·슬리버). `[mb-budget]` 0.

| | 버린 선언 ours | `Keyword` 폴백 | IACVT | var 폴백 | **합계 `decl_drop_total`** | 파싱만 |
|---|---:|---:|---:|---:|---:|---:|
| 2차 5곳 | 7,371 | 113 | 2,143 | 0 | **9,627** | 2,210 |
| 20곳 | 14,078 | 679 | 3,447 | 0 | **18,204** | 9,192 |

IACVT 사이트별: github 1,962 · yna 960 · kakaocorp 172 · naver 93 · google 88 · docsrs 75 · mdn 63 · apple 30 · velog 2 · enwiki 2(나머지 0).

**목표 환산 주의**: 동결 목표 ≤1,478 은 ours 만(7,389, 0b)에서 −80% 로 잡은 값이다. 합산 지표의 "전" 은 5곳 9,627 이라 같은 −80% 는 ≤1,925 — 1단계 완료 판정은 **합계를 동결 목표 ≤1,478 과 비교**(더 엄격)하고 ours 단독도 같이 적는다.

**발견 — `:root` 가 페이지 내용에 닿지 않는다(기존 버그, 1단계 설계 밖)**: `html.rs::parse` 는 `<html>` 요소를 arena 에 넣지 않고 `<head>`·`<body>` 를 **따로 루트**로 둔다(`roots()[0] == <body>` 테스트 픽스처를 지키려던 옛 결정). 그런데 `dom_select.rs::is_root` 는 `roots().first()`(= `<head>`) 를, 뷰는 `roots().last()`(= `<body>`) 를 스타일한다. 그래서 실사이트에서 `:root{--…}` 디자인 토큰은 `<head>` 에만 붙고 `<body>` 로 상속되지 않으며, `html{…}`·`html[data-*]`·`html.dark` 규칙은 어디에도 안 맞고, `:root/html{font-size}` 가 `rem` 기준이 되지 않는다(`document.documentElement` 도 없음). 실측(임시로 `is_root` 를 `last()` 로 바꾼 일회성 실험, 되돌림): IACVT **3,447 → 1,139** — github 1,962→839 · yna 960→0 · kakaocorp 172→158 · google 88→7 · docsrs 75→19 · mdn 63→8 · apple 30→11 · naver 93→93(naver 토큰은 `:where(:root,:host)` = 4단계 몫). 즉 1c 의 `var()` 경로가 들어가도 이 버그가 있으면 GitHub·yna 의 토큰은 여전히 못 푼다. `last()` 로 바꾸는 것 자체는 해결이 아니다 — `:root{font-size}` 가 `<body>` 에 특정성 (0,1,0) 으로 붙어 `body{font-size}` 를 이기는 등 틀린 값을 만든다. 선택지는 부모 세션 보고에(진행 표 메모). → **사용자 결정: 선택지 1(`<html>` 을 진짜 문서 요소로) — [1r](#1r--html-을-문서-요소로-2026-11-27) 에서 고침.**

#### 1r — `<html>` 을 문서 요소로 (2026-11-27)

P 의 발견을 1a 전에 고친다(사용자 결정: 문서 요소를 DOM 에 둔다 — 스펙 트리).

**변경**:
- **파서**(`html.rs::parse`): 원문에 `<html` 이 있으면(실사이트 전부) 루트는 **`<html>` 하나**, 그 아래 html5ever 가 만든 `<head>`·`<body>`(합성된 것 포함)와 그 사이 공백 텍스트 — 스펙 트리 그대로. `<html>` 의 속성 보관(`html[data-theme]`·`html.dark`). `<head>`/`<body>` 만 쓴 원문과 fragment(테스트 픽스처·오류/텍스트 문서 — `<div>` 하나)는 **옛 동작 그대로**(루트 = 쓴 래퍼 / 최상위 형제) — 테스트 수백 개의 `roots()[0] == <body>` 를 안 건드린다. 그래서 `<html` 없는 문서엔 문서 요소가 없다(`document_element() == None`, JS `documentElement` 는 `null`) — 실사이트엔 해당 없음.
- **`Document` 도우미**(`dom.rs`): `document_element()`(루트인 `<html>`), `body()`·`head()`(문서 요소의 첫 `<body>`/`<head>` 자식, 없으면 그 이름의 루트). 인덱스 계산 대신 이것을 쓴다.
- **스타일**: 바꿀 것 없음 — `is_root` 는 첫 루트(= `<html>`), 뷰는 `roots().last()`(= `<html>`)를 스타일하므로 `:root`·`html{}` 이 맞고, 상속이 html → body 로 흐르고, rem 기준(`raw_root` 의 `font-size`)이 `<html>` 의 것이 된다.
- **레이아웃**(`taffy_bridge.rs`): 루트 `<html>` 은 초기 블록 서식 맥락이라 **자식과 여백이 겹치지 않는다**(CSS 2.1 8.3.1). taffy 는 스크롤 컨테이너에서만 겹침을 멈추므로 루트 `<html>` 의 taffy 스타일만 `overflow: hidden` 으로(레이아웃 전용 — 페인트는 CSS `overflow` 를 읽음). 이게 없으면 `<body>` 의 8px 여백이 `<html>` 밖으로 빠져 문서 높이가 줄었다.
- **캔버스 배경**(`view.rs::canvas_background`): `<html>` 의 `background-color`, 투명이면 `<body>` 의 것을 뷰포트·문서 전체(`max(뷰포트 높이, document_height)`)에 맨 아래 `SolidRect` 로(CSS Backgrounds 3 §2.11.2). 색만 — 루트 배경 **이미지**의 캔버스 전파는 안 함(자기 상자에만). 상자 자신도 같은 색을 다시 칠한다(반투명 색만 두 번 보임 — 근사). 스크롤 상한(`document_height(commands)`)은 그대로(캔버스 높이가 이미 그 최댓값).
- **JS**: `document.documentElement` 추가, `body`/`head` 는 `Document::body()`/`head()`(옛 "아무 데서나 첫 `<body>`" 검색 제거).
- **에이전트**: 색인·관찰은 루트를 순회만 해서 그대로 — 골든에 html/head 줄이 안 생김(아래).
- **커스텀 속성 공유 스코프**(`style/variables.rs::CustomScope`): 첫 코퍼스 실행에서 `[mb-budget]` 3곳(github 5.3초·yna 3.7초·enwiki 2.2초, 전엔 0). `:root` 토큰(GitHub ~1,500개)이 이제 모든 노드에 닿는데, 노드마다 부모의 `--*` 를 전부 자기 `PropertyMap` 에 복사하고 치환용 `HashMap` 을 또 만들었기 때문(노드 × 토큰). → `--*` 는 `PropertyMap` 에서 빼고(아래에서 아무도 안 읽음) `Rc<CustomScope>` 사슬로 상속 — `--*` 를 선언한 노드에서만 새 고리, 조회는 사슬을 거슬러 올라감(깊이 상한 256 이내). 치환 의미는 그대로(노드에서 푼다 — "정의한 노드에서 풀고 상속"은 1c). 결과 github 803ms(1r 전 1,319ms보다 빠름)·yna 978·enwiki 409, `[mb-budget]` 0. 1c 설계의 "치환 완료된 커스텀 속성 맵을 부모에게서 받아 자기 정의만 덮는다"의 공유 부분을 앞당긴 것.
- **깊이 상한**: `<html>` 이 깊이 1, `<body>` 2 — 상한 256 은 트리 전체를 센다(Blink 의 파서 상한도 문서 기준). 테스트 `html_root_counts_as_depth_one_for_the_cap`.

**테스트 +13**(919 → 932): 파서 4(`document_with_html_tag_has_the_html_element_as_its_single_root`·합성 head/body·fragment/body-only 그대로·깊이), 스타일 6(`root_custom_property_reaches_body_descendants`·`html_attribute_selector_matches_the_document_element`·`rem_resolves_against_the_html_font_size`·`root_font_size_does_not_override_a_body_rule`·`html_color_inherits_into_the_body`·`custom_properties_inherit_without_being_copied_into_each_node`), 캔버스 3. JS `document_body_and_head_accessors_resolve` 에 `documentElement`·`body.parentNode` 단정 추가. 실패 먼저 확인(8개 + 캔버스 1 + JS 1 실패 → 구현).

**골든·스냅샷**:
- 관찰 골든 6개: 요소 ID 가 +2(새 `<html>` 노드와 head·body 사이 공백 텍스트가 먼저 번호를 받음 — NodeId = ID). ID 를 정규화하면 남는 차이는 `page_height` 4곳 +4(links_and_text 344→348 · long_page 1,880→1,884 ×2 · signup_form 261→265): 마지막 자식의 아래 여백(12/16px)이 `<body>` 의 8px 와 겹쳐 그 값이 `<html>` 안에 남는다 — Chrome 과 같은 높이. 전엔 body 자기 여백 8 만 더했다. html·head 줄은 안 생김. compat3 은 ID 만.
- 렌더 스냅샷 2개(`solid_color_box`·`border_radius`): `<body style="background:…">` 가 이제 캔버스 전체를 칠한다(전엔 내용 아래 흰 띠) — Chrome 과 같음.
- eval: `eval/runs/*.jsonl` 은 과거 실행 기록(ID 포함)이라 그대로 — 시나리오 정의는 ID 를 안 씀.

**코퍼스**(라벨 `html-root`, `step1-before` 대비, 동결 캐시·오프라인·release): 결과·기대 컨트롤·오프라인 미스·버린 선언(ours)·`Keyword` 폴백은 25곳 동일, `[mb-budget]` 0, 20곳 `load_ms` 합 5,251 → 4,834.
- **IACVT 3,447 → 318**(5곳 2,143 → 128): github 1,962→28 · yna 960→0 · google 88→7 · docsrs 75→3 · mdn 63→9 · apple 30→11 · kakaocorp 172→158 · naver 93(그대로 — `:where(:root,:host)` = 4단계) · enwiki 2→7(이제 적용되는 `html.…` 규칙 안의 미정의 `var()`). 합산 지표 `decl_drop_total` 5곳 9,627 → **7,612**, 20곳 18,204 → 15,075. P 실험값(1,139)보다 더 줄었다 — 실험은 `:root` 만 옮겼고 `html{}`·상속 경로는 그대로였으므로.
- 스크립트 에러 korea-kr 32→31·google 6→5 — `document.documentElement` 를 읽던 스크립트가 이제 산다.
- **요소 목록 diff 2곳**: github **+1** `link "Homepage"` = 헤더 로고(`<a href="/" aria-label="Homepage">`) — 크기가 `var(--base-size-…)` 토큰이라 전엔 IACVT → `initial` 로 상자가 없었다; Chrome 에서도 왼쪽 위에 보임(정상). enwiki **−3** `button "Appearance"`·`link "Printable version"`·`link "Save"` = MediaWiki 의 JS 전용 컨트롤을 `<html class="client-nojs">` 일 때 숨기는 규칙(`.client-nojs .vector-appearance-landmark`·`.client-nojs #t-print`·`.client-nojs #ca-bookmark` `{display:none}`, 캐시 CSS 로 확인)이 이제 맞는다. Chrome 은 시작 스크립트가 `client-js` 로 바꿔 보이지만 우리는 그 인라인 스크립트가 `document.cookie.match`(우리 `document.cookie` 없음 — 기존 JS 갭, 1r 전에도 같은 에러)에서 죽어 `client-nojs` 로 남는다 → 페이지 자신의 no-JS 화면과 같다(세 컨트롤 다 JS 없이는 안 움직임). 후속: `document.cookie`(4차 작은 JS API 목록).
- **문서 높이**(요소 목록은 그대로): apple 26,617→4,271 — `<html class="no-js …">` 에 `html.no-enhanced … .inline-media-wrapper`·`html.no-js [data-films-media] video {display:none}` 가 맞아 큰 미디어 블록이 숨음(우리 JS 가 `no-js` 를 안 지워서 — no-JS 화면과 같음; 26k 는 미디어가 펼쳐진 값). rustlang 9,772→4,507 — `html{font-size:62.5%}`/`75%` 가 이제 rem 기준(Tachyons 류 rem 간격이 줄어듦, Chrome 과 같은 계산). mdn 6,755→9,224 · github 9,205→8,382 · enwiki 16,790→18,008 · kakaocorp 3,046→3,118 · naver 314→294 · docsrs 6,029→6,044 · google 753→749 · daum 22→18 — 토큰·`html` 규칙·rem 기준·body 여백이 `<html>` 안에 남는 것. 사이트별 Chrome 대조는 안 했다. 슬리버 20곳 325 그대로(enwiki 71→70, docsrs 3→4).

**퍼저**: 생성기가 이미 `<!DOCTYPE html><html>…` 완전 문서를 만들어 새 경로를 탄다. 긴 실행(release) 레이아웃 2만 + CSS·이미지 기본 수 무결(0 실패 서명).

**남은 것**: `<html` 없이 `<head>`/`<body>` 만 쓴 원문은 옛 동작(문서 요소 없음) — 실사이트엔 없지만 손으로 쓴 픽스처에 `:root` 를 쓰면 이 차이에 걸린다. 루트 배경 이미지의 캔버스 전파. `document.cookie`.

#### 1a — `!important` (2026-11-27)

설계 결정대로. 달라진 점·세부:
- **파싱**(`css/mod.rs` `DeclHandler::parse_value`): `input.parse_until_before(Delimiter::Bang, …)` 안에서 속성 파서를 돌리고(값 파서는 `!` 를 못 본다 — `parse_until_before` 가 `parse_entirely` 라 각 파서의 "남기면 실패" 계약 그대로), 남은 것에 `try_parse(cssparser::parse_important)`. 그 뒤에 무엇이 남으면(`!important foo`, `!ie`, `!imp`) cssparser 의 선언 파서가 선언째 버린다(Chrome 과 같음). `Declaration::important` — shorthand 가 펼친 longhand 전부에 같은 플래그. `--x: 5px !important` 는 값 `5px` + 플래그(1c 의 원문 보관도 이 경로라 `!important` 는 원문에 안 들어간다). `Rule::has_important` = 선언 중 하나라도.
- **`@keyframes`**: 본문을 통째로 건너뛰므로(`AtRulePrelude::Reject`) 그 안의 `!important` 는 자연히 무시 — 테스트로 고정.
- **캐스케이드**(`style/cascade.rs`): 매칭 규칙을 지금처럼 `(특정성, 순서)` 로 정렬한 뒤 ① 작성자 일반 → ② 인라인 일반 → ③ `has_important` 규칙의 중요 선언(같은 정렬) → ④ 인라인 `!important`. `apply_declarations(…, important)` 가 플래그로 거른다. 프레젠테이션 힌트·UA 기본은 ① 앞, `<center>` 심은 ④ 뒤(그대로).
- **버림 분류**(`css/report.rs`, [0b 분류표](#0b-분류표-동결--바꾸려면-이-표를-먼저-고친다) `important` 행을 먼저 고침): 끝의 `!important` 를 떼고 **보고를 잠시 치운 채** 다시 파싱해 받히면 `important`(플래그가 원인 — 1a 뒤엔 버그), 아니면 뗀 값으로 `multi-token`/`invalid-value`. 분류가 다시 파싱하므로 `parse_declaration_block` 은 분류를 `report::record` 클로저 밖에서 한다(같은 `RefCell` 을 두 번 빌리지 않게).
- **테스트 +11**(932 → 943): 파서 6(`important_declaration_is_kept_and_flagged` — 공백·주석·대소문자, `has_important` · `important_garbage_suffix_drops` · `custom_property_important_strips_flag` · `shorthand_longhands_inherit_important` — margin/padding/border-radius/flex/background · `important_inside_keyframes_is_ignored` · `inline_style_important_is_flagged`), 캐스케이드 5(`important_beats_more_specific_normal` · `…_cascade_by_specificity_then_order` · `important_inline_beats_important_author`(인라인 일반 < 작성자 중요도 같이) · `important_shorthand_beats_more_specific_normal_longhand` · `important_inside_a_non_matching_media_block_does_not_apply`), 분류 단정 갱신. 실패 먼저 확인(컴파일 실패 → 캐스케이드 3·분류 2 실패 → 구현).
- **퍼저**: 구조 생성기의 규칙·인라인 선언 1/8 에 ` !important`(`flagged_declaration`) + 생성기 검사(200 중 규칙 ≥50·인라인 ≥10). CSS 토큰 풀은 원래 `!important` 변형을 뿌렸다 — 이제 그 선언이 캐스케이드까지 간다. 긴 실행(release) 레이아웃 2만 + CSS·이미지 무결.
- **골든 변화 없음**: `compat3` 의 `.hide-sm{display:none !important}` 는 `max-width:600px` 블록 안이라 800px 에서 꺼진 채(머리 주석 2번 문구를 "Chrome 과 같음"으로 갱신 — 주석은 노드가 아니라 ID 불변).

**코퍼스**(라벨 `important`, `html-root` 대비, 동결 캐시·오프라인·release): `[mb-budget]` 0, `load_ms` 합 거의 같음(4,834 → 4,901).
- **버림 사유 `important` 5,097 → 0**(20곳). 버린 선언 ours 5곳 7,371 → **4,387**(20곳 14,078 → 9,643), 합산 지표 5곳 7,612 → **4,645**(20곳 15,075 → 10,680). 남김 선언 121,905 → 126,860. 살아난 선언 중 일부가 IACVT(+14)·`Keyword` 폴백(+26)·파싱만(+144)으로 옮겨 감 — 값 파서(1b)·`var()`(1c) 몫. 동결 목표 ≤1,478 까지는 1b·1c.
- **오프라인 미스 → `ok*` 2곳**: github 2(`/images/spinners/octocat-spinner-{dark,light}mode.svg` — 로딩 스피너), yna 1(`bg_map02.jpg`) — `!important` 로 살아난 `background-image` 가 새 요청을 낸다. 보충 녹화는 하지 않음(0.2 규칙: 단계당 1회, 사람 결정). 요소 목록엔 영향 없음.
- **요소 목록 diff 4곳** — 캐시 CSS 로 하나씩 확인:
  - yna **−4**: `button "화면 상단으로 이동"`(`btn-scroll-top display-none`)과 탭 링크 3개(`div.display-none` 안의 스와이퍼) — `.display-none{display:none!important}`. 페이지 JS 가 필요할 때 클래스를 뗀다 → Chrome 첫 화면에서도 숨음(정상).
  - microsoft **−2**: `button "More"` = `li#overflow-menu.x-hidden`, `button ""` = `#close-epb`(`div#epb.x-hidden` 안의 배너 닫기) — `.x-hidden{display:none!important}`(정상, JS 가 필요할 때 보임).
  - docsrs **−1**: `button "Copy item path"` — `#copy-path{display:none !important}` 가 rustdoc 의 **`noscript-….css`** 에 있다(`<noscript><link rel=stylesheet>`). Chrome 은 JS 가 켜져 있으면 `<noscript>` 안을 파싱하지 않아 이 시트를 안 받는다. 우리는 파서가 scripting 꺼짐으로 돌아 `<noscript>` 안의 `<link>` 까지 모아 적용한다 — **기존 버그**(1a 가 `!important` 를 살려 드러남). 후속: 스크립트를 돌리는 문서에선 `<noscript>` 안 `<link>`/`<style>` 를 건너뛸 것.
  - enwiki **+3**: `link "Log in"`·`"Create account"`·`"Donate"`(기대 컨트롤 2/3 → **3/3**) — 아래 헤더 변화로 0폭이던 사용자 링크가 자리를 얻었다. Chrome 에서도 보이는 링크라 결과는 맞지만 경로가 틀린 레이아웃이다.
- **enwiki 헤더 레이아웃 이상(기존 버그가 드러남 — 1a 범위 밖, 진단만)**: 문서 높이 18,008 → **1,280**(관찰의 `page_height`), 헤더 268 → 1,292px(Chrome ~50px). 원인 사슬:
  1. 레거시 레이아웃은 `height: N%` 를 **부모 폭** 기준으로 푼다 — `block.rs:212`·`inline.rs:540/660/697`·`flex.rs:110/181`·`grid.rs:200`·`table.rs:401` 모두 `length_value(node, "height", <width>)`. 스펙은 확정된 부모 높이 기준, 없으면 `auto`. 이것은 3단계 세부 항목 "퍼센트 높이 기준 바로잡기" 그 자체다. 1a 전에도 드롭다운 체크박스(`position:absolute; width:100%; height:100%`)가 1,172×1,172 였고 헤더가 268px 였다.
  2. 1a 로 `.mw-jump-link:not(:focus){position:absolute !important}`·`.cdx-button--icon-only span+span{position:absolute !important}` 가 살아나 몸통·헤더 서브트리가 레거시 경계로 더 많이 들어간다 → `html,body{height:100%}` 가 1,280(뷰포트 **폭**), `.mw-logo{height:100%}` 도 1,280.
  3. 관찰의 `page_height` 는 루트 상자 높이(`margin_box_bottom(layout_root)`)라 넘치는 내용(~19,000px)을 안 센다 — 스크롤 상한은 그리기 명령 기준이라 스크롤은 된다.
  - 합성 재현: 헤더 마크업 + 그 규칙 하나(`.vector-dropdown .vector-dropdown-checkbox{position:absolute; width:100%; height:100%}`)만으로 체크박스가 폭과 같은 높이가 된다(캐시에서 줄인 입력, 저장소 밖).
  - 선택지는 부모 보고에: (a) 그대로 커밋하고 3단계에서 고침, (b) 3단계의 퍼센트 높이 항목만 1a 직후로 당김(레거시: 부모 높이가 확정이 아니면 `auto`), (c) 관찰 `page_height` 를 루트 상자 대신 내용 넘침까지 재게(Chrome 의 scrollHeight) — (b)와 독립.
  - → **사용자 결정 (b)**: [1p](#1p--퍼센트-높이-기준-2026-11-28) 로 고침.

#### 1p — 퍼센트 높이 기준 (2026-11-28)

3단계 세부 항목 4 의 높이 부분을 1b 전으로 당겼다(1a 에서 enwiki 헤더가 1,292px 가 된 원인). CSS 2.1 §10.5: `height`/`min-height`/`max-height` 의 % 는 포함 블록의 **높이** 기준이고 그 높이가 **확정**일 때만 — 아니면 `auto`·`0`·`none`.

**설계**:
- **확정 높이를 위에서 아래로**(`layout/properties.rs`): 높이는 아래에서 위로 계산되므로, 상자가 자식을 배치하기 **전에** 자기 확정 내용 높이를 정하고(`definite_height` = 길이, 또는 확정 포함 블록 기준 % — 재귀적으로; `min/max` 로 클램프) 자식 배치 동안 패스 범위 상태로 깐다(`with_containing_block_height`, 인라인 크기 메모처럼 스레드 지역 + 패닉에도 복원). 그래서 컨테이너의 자식 배치 안에서 `containing_block_height()` = 그 컨테이너의 자기 확정 높이 — flex(열 방향 주축·행 방향 교차축)·grid(`container_explicit_height`)가 그것을 읽는다. 함수 시그니처 수십 개를 바꾸지 않으려고 고른 방식.
- **레거시 지점 전부**: `block.rs` `layout_node`(옛 212행 — 자기 높이를 자식 전에, 자식은 그 범위 안에서), `inline.rs` 인라인 블록 배치(`layout_inline_block_node_as`)·줄 채우기 크기(`inline_block_outer_size`)·인라인 내용 크기(`inline_content_size`), `flex.rs` 두 곳, `grid.rs`, `table.rs` 셀. 레거시가 **안 읽던** `min-height`/`max-height` 를 이제 읽는다(px·확정 %; `max` 다음 `min`, 최소가 이김 — §10.7). 인라인 크기 메모 키에 포함 블록 높이 추가(`(NodeId, 폭, CB 높이)`).
- **taffy 와 맞춤**: taffy 는 원래 확정 부모에서만 % 를 푼다. 경계 잎(레거시가 배치하는 서브트리)에는 지금까지 폭만 넘겼는데, 이제 `inputs.parent_size.height`(확정일 때만 `Some`)를 레거시의 포함 블록 높이로 깐다(`measure_boundary`). 텍스트가 든 상자는 거의 다 경계 잎이라 사실상 모든 % 높이가 레거시를 탄다 — 테스트는 경계 유도(absolute 자식) 유무 두 경로로 돈다.
- **초기 포함 블록 = 뷰포트**(표준 모드): `layout_tree_with_viewport(root, w, Some(h))` — taffy 래퍼에 뷰포트 높이를 줘서 `html{height:100%}` = 뷰포트, `body{height:100%}` = 그것의 100%(Chrome 과 같음). `layout_tree(root, w)` 는 높이 없음(루트 % = auto, 옛 호출·테스트용). `view.rs` 가 페이지 영역 높이(창 − `CHROME_HEIGHT`)를 넘긴다. 같은 이유로 2패스 `reposition_absolutes` 의 초기 포함 블록 높이도 루트 상자 높이 대신 뷰포트(있으면) — `position: fixed` 와 위치 지정 조상 없는 absolute 의 `%`·`bottom` 이 페이지 길이만큼 늘던 것(GitHub `scrollMarksContainer{height:100%}` 6,814px).
- **absolute 의 %**: 1패스에서는 정적 위치(흐름 속 부모 기준)로 크기가 정해지므로, 2패스 `reposition_absolutes` 가 % 높이·min·max 를 **포함 블록의 패딩 상자** 기준으로 다시 푼다(`resolve_percent_height` — 그 상자 자신의 높이만, 자식은 1패스 배치 그대로; 근사). 포함 블록이 auto 높이여도 2패스엔 확정이라 enwiki 드롭다운 체크박스(`.vector-dropdown{position:relative}` 안 `height:100%`)가 라벨 높이(17px)가 된다.
- **absolute 자식은 블록화 + shrink-to-fit**(§9.7·§10.3.7): flex·grid·인라인 흐름이 out-of-flow 자식을 `layout_inline_or_inline_block` 으로 놓아 `display:block` absolute 상자가 **인라인 상자**로 배치됐다 — 그 자식들이 컨테이너 폭과 "높이 없음"을 받았다(korea.kr 로고 `a{width:100%;height:100%}` 가 1,160×0 → 관찰에서 사라짐; 1p 전엔 폭 기준 % 가 우연히 가렸다). 네 곳(`flex.rs`·`grid.rs`·`inline.rs` 두 곳)을 `layout_out_of_flow`(= 인라인 블록 경로)로.
- **인라인 블록 높이 근사**: `inline_block_outer_size` 의 높이(인라인 상자 내용 높이의 재료)가 선언 높이 없으면 0 이었다 → 자기 인라인 내용 높이(같은 메모 측정이라 여전히 노드·폭당 한 번). 인라인 블록 자식만 가진 인라인 상자(GitHub 브레드크럼 `a{display:inline-flex;height:100%} > span{display:inline-block}`, rustdoc `<a><code>` + `.docblock p code{display:inline-block}`)가 0 높이로 관찰에서 빠지던 기존 버그 — 1p 가 % 를 auto 로 바꾸며 드러남.
- **뷰 캐시**: 레이아웃이 이제 뷰포트 높이에 의존할 수 있다. 루트에 % `height/min-height/max-height` 가 있을 때만(그 아래는 루트를 거쳐서만 뷰포트에 닿으므로) `DocumentView::reads_viewport_height` 가 켜지고, 그때만 `CachedView::page_height` 를 비교한다 — 높이만 바뀌는 리사이즈가 나머지 페이지에선 계속 캐시 적중.
- **범위 밖(기록)**: 쿼크 모드 body/html % 높이 특례(우리 엔진은 표준 모드만). 늘인(stretch) flex 항목의 크기를 자식 % 의 확정 기준으로 보는 규칙(Chrome) — 지금은 항목 자신의 `height` 가 auto 면 그 자식의 % 는 auto. grid 항목의 % 는 영역이 아니라 컨테이너 높이 기준(레거시 근사). `top/bottom %`(3단계 4번 남은 부분).

**시도했다가 되돌린 것**: `inline-flex`/`inline-grid` 를 레거시에서 진짜 flex/grid + 원자 인라인으로(taffy 는 이미 그렇게 매핑) — 브레드크럼은 고쳐지지만 퍼저가 `* { flex: 1 0; display: inline-flex }` 에서 1e7px 폭(0c F11 과 같은 종류: 축소-맞춤 탐색 중 1e6 대리 폭으로 늘어난 flex 항목)을 찾아 되돌리고 위의 좁은 높이 근사로 대신했다. 후속 후보.

**테스트 +12**(943 → 955): 레이아웃 11 — `percent_height_in_an_auto_height_parent_is_auto` · `…_in_a_definite_parent_resolves_against_its_height` · `html_body_percent_height_chain_fills_the_viewport` · `percent_height_chain_breaks_at_an_auto_ancestor` · `absolute_percent_height_resolves_against_the_containing_block_padding_box`(확정·auto 포함 블록 둘 다) · `fixed_and_unanchored_absolute_percent_heights_use_the_viewport` · `percent_height_inside_an_absolute_box_with_a_definite_height` · `absolute_child_of_a_flex_or_inline_container_sizes_as_a_block`(flex·grid·인라인) · `inline_flex_with_percent_height_in_an_auto_parent_keeps_its_content_height` · `min_and_max_height_percentages_need_a_definite_containing_block` · `percent_heights_in_an_auto_height_flex_header_stay_small`(enwiki 모양) — 대부분 taffy·레거시 두 경로로; 상태 1 — `height_only_resize_relayouts_a_percent_height_root`. 실패 먼저 확인(7개 → 구현, 이후 코퍼스에서 찾은 4건도 각각 실패 테스트 먼저). 골든·스냅샷 변화 없음.

**퍼저**: 구조 생성기가 이미 `height/min-height/max-height` 에 % 를 냈다. 규칙 1/8 의 셀렉터를 `html, body` 로(뷰포트 사슬) + 생성기 검사(200 중 ≥20). 퍼저 파이프라인은 `build_document_view(800, 600)` 이라 뷰포트 높이 경로를 탄다. 긴 실행(release) 레이아웃 2만 + CSS·이미지 무결(되돌린 `inline-flex` 시도 때 1건 → 되돌린 뒤 0).

**코퍼스**(라벨 `pct-height`, `important` 대비, 동결 캐시·오프라인·release): 결과·버림·기대 컨트롤·오프라인 미스는 25곳 동일, `[mb-budget]` 0, `load_ms` 합 4,901 → 5,172.
- **enwiki 헤더 1,292 → 124px**(Chrome ~50 — 남은 차이는 로고의 워드마크 이미지·텍스트 줄을 우리 인라인 배치가 세로로 쌓는 것, % 와 무관), 드롭다운 체크박스 1,280 → 17px. 1a 의 enwiki **+3 링크(Log in·Create account·Donate)는 그대로 남는다** — 헤더가 정상 크기가 된 뒤에도 보이고, Chrome 데스크톱에서도 오른쪽 위에 보이는 링크라 맞다(기대 컨트롤 3/3 유지).
- **요소 목록 diff**: docsrs **+15** 뿐 — `tokio::task`·`AsyncRead` 등 본문 안 문서 링크(`<a><code>…</code></a>`, rustdoc `.docblock p code{display:inline-block}`). 인라인 블록 높이 근사가 0 이라 0 높이 상자로 빠져 있었다; Chrome 에서 보이는 링크(정상). 중간 실행에서 나왔던 korea-kr −1(로고 링크)·github −1(브레드크럼 `tokio`)은 위 두 수정으로 없어짐.
- 슬리버 20곳 319 → 340(korea-kr +11·apple +3·enwiki·rustlang·github +2·microsoft +1): absolute sr-only 라벨(`i.sr-only{position:absolute;width:1px;overflow:hidden}`)이 이제 블록화되어 선언 폭 1px 를 지킨다 — 3단계 클리핑이 없애는 바로 그 슬리버, 요소 목록 무관.
- 첫 화면 요소 751 → 829(github 11 → 94: 위쪽의 폭 기준 % 높이 상자가 줄어 내용이 첫 화면으로 올라옴).
- **문서 높이(관찰의 `page_height` = 루트 상자)**:
  - `html, body {height:100%}` 4곳 → 정확히 뷰포트 800: velog 1,280·enwiki 1,280·d2 5,376·google 749 → 800. 루트 상자 자체는 Chrome 과 같다. 하지만 d2(내용 바닥 5,628)·enwiki(17,178)는 내용이 루트를 넘친다 — 관찰의 `page_height` 가 스크롤 가능한 길이를 **못 나타낸다**(스크롤 자체는 그리기 명령 기준이라 됨). velog(그려진 128)·google(766)은 원래 한 화면.
  - 루트 % 없는 곳: yna 1,304 → 16,996 · bbc 26,042 → 6,670 · hani 10,426 → 6,860 · kakaocorp 3,118 → 2,136 · korea-kr 5,588 → 5,247 · github 7,992 → 6,814 · apple 4,271 → 6,768 · docsrs·rustlang·microsoft 소폭 — 이미지·카드 미디어 래퍼의 `height:100%`(yna 71·korea-kr 95·hani 69·apple 40·bbc 27 상자)가 폭 기준 값을 잃었다(auto 또는 확정 부모 기준). yna 1,304 는 폭 기준 % 래퍼가 루트를 잘라 놓은 값이었다. 사이트별 Chrome 대조는 안 했다.
- **(c) 권고 — `page_height` 를 `scrollHeight` 처럼**: 1p 로 `html, body {height:100%}` 사이트에서 `page_height` 가 800 이 됐다(d2 5,376 → 800) — 에이전트가 "한 화면짜리 페이지"로 읽을 수 있다. 1p 와 **같이 커밋하거나 바로 다음**에 넣기를 권한다: `page_height = max(루트 여백 상자 아래, 넘친 자손의 아래)` — 이미 있는 `view::document_height(commands)`(스크롤 상한이 쓰는 값)와 같은 기준으로 맞추면 관찰과 스크롤이 일치한다. 골든은 페이지가 짧아 변화 없을 것으로 예상.
- **발견(후속)**: `top: 0`·`bottom: 0`·`left: 0` 같은 **단위 없는 0** 이 `Value::Number(0.0)` 으로 파싱되고 레거시 `length_value` 가 숫자를 무시해 absolute 오프셋이 적용되지 않는다(정적 위치에 남음) — 실사이트에 매우 흔한 모양. 1p 측정을 흐리지 않게 이번엔 안 고침(테스트는 `0px`). → **[1q](#1q--page_height--scrollheight--단위-없는-0-2026-11-28) 에서 고침.**

#### 1q — `page_height` = scrollHeight + 단위 없는 0 (2026-11-28)

1p 의 두 후속을 1b 전에(사용자 승인).

**1. 스크롤 높이 한 곳**: `DocumentView::document_height`(`view.rs`) = max(루트 여백 상자 아래, 레이아웃 트리 모든 상자의 테두리 상자 아래, 그리기 명령의 가장 아래) — 캔버스 채우기를 넣기 **전**에 잰다(캔버스는 뷰포트를 덮어 짧은 페이지를 뷰포트 높이로 부풀린다). 레이아웃 상자를 세는 이유: Chrome 의 scrollable overflow 는 상자를 센다 — 글자 한 줄만 그린 3,000px 상자도 스크롤된다(그리기만 세면 텍스트 아래에서 멈춘다). 오버플로 클리핑은 없으므로(3단계) 잘린 서브트리도 센다. 소비자:
- 스크롤 상한 `BrowserState::clamp_scroll` — 전엔 `view::document_height(commands)`(그리기 + 캔버스).
- 관찰 `page_height` — `ObserveInput::document_height` 로 받는다(전엔 `margin_box_bottom(layout_root)`, 1p 뒤 `html,body{height:100%}` 에서 800). 에이전트 `scroll`·`scroll_into_view` 는 상태의 스크롤(같은 상한)을 거치므로 따로 고칠 곳 없음. 셸·관전 창은 `display_list` → 같은 상한. mb-mcp 의 `max_scroll` 은 관찰 뷰어 패널용이라 무관.
- Chrome 의 `scrollHeight` 는 뷰포트 높이 아래로 안 내려가지만 관찰은 짧은 페이지의 자기 높이를 그대로 낸다(골든 유지 — 에이전트에겐 "한 화면 안에 다 있다"가 더 유용).

**2. 단위 없는 0 = `0px`**(`css::unitless_zero_as_length`): 일반 값 경로에서 `Number(0)` 이고 속성 표가 길이 종류(`ValueKind::Length`)면 `Length(0, Px)`. `line-height` 는 표에선 길이지만 숫자가 배수라 제외, 숫자 종류(`opacity`·`z-index`·`flex-grow`…)는 해당 없음. `margin`/`padding` 단축은 원래 승격하고 있었다. `var()` 치환 뒤에도 같은 규칙(`style/variables.rs::promote_unitless_zero` — `--z: 0; top: var(--z)`). **감사**: 레거시 `length_value` 와 taffy 브릿지의 `dimension`·`lpa_or_auto`·`lp` 모두 `Number` 를 무시(→ auto)했다 — `top/left: 0`(absolute 가 정적 위치에 남음)·`width/height/min-height: 0`·`gap: 0`·`flex-basis: 0` 이 다 무시되던 것. `border-radius` 단축의 `Number(0)` 은 소비자가 0 으로 읽어 결과 같음(그대로). 남은 것: `flex: 1 1 0` 은 flex 단축 파서가 선언째 버린다 — 1b 의 flex 단축 항목. `inset` 단축은 아직 미지원 — 1b 10번.

**테스트 +6**(955 → 961): `view` 2(`document_height_counts_content_overflowing_a_viewport_tall_root` — 루트 300·스크롤 3,000, `document_height_of_a_short_page_is_its_root_margin_box` — 배경 유무), 골든 테스트 1(`page_height_counts_content_overflowing_a_viewport_tall_root` — `page_height: 3000`), CSS 1(`unitless_zero_is_a_px_length_for_length_properties` — 길이 8개 승격·숫자 4개 그대로·`top:5` 그대로), 레이아웃 1(`unitless_zero_offsets_pin_an_absolute_box_to_the_corner`), 스타일 1(`unitless_zero_through_var_is_a_px_length`). 전부 실패 먼저 확인.

**골든**: `compat3` 만 `page_height` 318 → **475** — 300px `overflow:hidden; white-space:nowrap` 캐러셀의 인라인 블록 슬라이드를 우리가 줄바꿈해 세로로 쌓고(Chrome 은 가로로 두고 자름) 클리핑이 없어 아래로 넘친다. 그리기 기준 스크롤 상한은 이미 거기까지였고 관찰이 그것과 맞춰진 것 — 머리 주석 5번(3단계)이 추적하는 갭. 다른 골든·스냅샷 변화 없음.

**퍼저**: 새 문법 없음(생성기가 이미 `0` 과 `0px` 를 섞어 냄). 긴 실행(release) 레이아웃 2만 + CSS·이미지 무결.

**코퍼스**(라벨 `scroll-height`, `pct-height` 대비): 결과·버림·기대 컨트롤·오프라인 미스 25곳 동일, `[mb-budget]` 0, `load_ms` 합 5,172 → 5,030.
- **`page_height`(9곳)**: d2 800 → **5,458** · enwiki 800 → **12,732** — 1p 에서 뷰포트로 줄었던 `html,body{height:100%}` 사이트가 내용 길이를 다시 낸다(velog 800·google 800 은 원래 한 화면이라 그대로). bbc 6,670 → 24,662 · apple 6,768 → 10,298 · yna 16,996 → 20,548 · naver 294 → 759 · korea-kr 5,247 → 5,541 · github 6,814 → 6,945 · kakaocorp 2,136 → 2,102 — 루트 상자 대신 넘친 내용까지(대부분 그리기 기준 스크롤 상한이 이미 그만큼이었다). enwiki 가 1p 때 잰 내용 바닥(17,178)보다 짧은 것과 kakaocorp 의 감소는 단위 없는 0: `top:0`/`left:0` absolute 상자가 정적 위치(아래쪽)에서 포함 블록 모서리로 올라감. 사이트별 Chrome 대조는 안 함.
- **요소 목록 diff 1곳**: korea-kr **−1** `link "2027년 예산안 … 정책포커스 바로가기"`(정책포커스 칸의 `pWiseMain=F1` — 같은 이름의 상단 배너 링크는 그대로). 반응형 썸네일 꼼수 `.thumb{height:0}` + `.thumb_col > li.h_full .item .thumb{padding-top:calc(270 / 307 * 100%)}` 에서 이제 `height: 0` 은 지켜지는데 `calc()` 패딩은 아직 버려져(5단계) 상자가 0 높이 → 링크도 0 높이로 관찰에서 빠짐. 1q 전엔 `height: 0` 을 무시해 우연히 보였다. Chrome 에선 보임 — **5단계 `calc()` 로 돌아옴**. 기대 컨트롤 무관.
- 첫 화면 요소 829 → 907(enwiki 21 → 91·d2 14 → 22 — `top:0` absolute 상자·0 크기 상자가 제자리로).
- 슬리버 340 → 413(kakaocorp 0 → 32·d2 20 → 61 등): sr-only 클래스 `.screen_out`·`.blind`(`position:absolute; width:0; height:0; overflow:hidden`)의 `width/height: 0` 이 이제 지켜져 0×0 상자 밖으로 글자가 넘친다 — 3단계 클리핑이 없애는 종류, 요소 목록 무관.

#### 1b — 값 파서 묶음 (2026-11-28)

설계 결정대로, 1q 에서 넘긴 `inset`·`flex: 1 1 0` 포함. 각 파서는 자기 몫을 다 소비하지 못하면 선언째 버린다(지금 계약). 새 `Value`: `FontFamilies(Vec<String>)`, `BackgroundSize(BackgroundSize)`(`Cover`/`Contain`/`Size(SizeAxis, SizeAxis)`), `ClipBox(ClipBox)`(`Rect`/`Inset` + 네 변 `ClipEdge`).

- **CSS 전역 키워드**(10번 — 점검해 보니 **아무 데서도 처리하지 않았다**: `color: inherit` 가 `Keyword("inherit")` 로 저장돼 상속값을 덮고 검정으로 그려졌다): 파싱은 모든 속성에서 단독 `inherit|initial|unset|revert|revert-layer` 를 받고, shorthand 는 자기 longhand 전부에 넘긴다(`css::longhands_of`). 스타일 패스가 `var()` 치환 **뒤**, 상속 복사 **앞**에서 푼다(`resolve_css_wide_keywords`): `inherit` = 부모 계산값, `initial` = 값 없음(소비자의 기본값 = 초기값; 상속 속성이면 상속 복사도 막음), `unset`/`revert`/`revert-layer`(≈ `unset`, UA 시트 되돌리기 없음) = 상속 속성이면 부모, 아니면 초기. 상속 속성 목록은 `style::INHERITED_PROPERTIES` 하나로. **`all: <전역 키워드>`**: 캐스케이드가 그 선언을 만나는 순간 지금까지 정해진 모든 속성(UA 기본·앞선 선언 포함, `--*` 제외)과 상속 속성을 그 키워드로 바꾼다(`cascade::apply_all`) — 뒤의 선언은 그대로 이긴다. `!important` 패스에서도 같은 자리.
- **`border` 계열**(기존 키 체계 유지): `border`·`border-{side}` = 폭·스타일·색 아무 순서·각 1회, 빠진 것은 초기값(`medium`=3px·`none`·`currentcolor`) — 폭 키 `border-{side}`, 스타일 `border-{side}-style`, 색 `border-{side}-color`. `border-width/-style/-color` 1~4값, `border-{side}-width` 는 폭 키로 접음, `thin/medium/thick` = 1/3/5px. 스타일 패스가 **명시적 `none`/`hidden` 인 면의 폭을 0**으로(CSS 2.1 §8.5.3) — `border: none`, 스타일을 뺀 `border-top: 2px`(Chrome 처럼 안 그려짐). 스타일 키가 **없는** 폭(UA 기본·`border="1"`·작성자의 `border-top-width` 단독)은 예전처럼 그려진다 — 스펙상 초기 스타일 `none` 이면 0 이지만 UA 기본이 스타일 키 없이 폭을 주므로 이 근사를 유지(기록). 페인터는 면별 색 키 → `currentcolor` 키워드면 글자색 → UA 의 단일 `border-color` → 글자색. 점선·이중선은 실선(범위 밖). **둥근 모서리 테두리는 그리지 못한다**(모서리가 각짐) — 1b 전엔 작성자 `border` 가 통째로 버려져 안 보였던 갭(스냅샷 `border_radius` 에서 보임), 후속.
- **`font-family`** → `FontFamilies`(따옴표 문자열 또는 공백으로 이은 식별자들의 쉼표 목록). 소비자 두 곳(`inline_font_family`·`font_family_keyword`)은 `layout::font_family_name` 하나로: 목록에 고정폭 계열(`monospace`·`ui-monospace`·이름에 `mono`/`courier`·`Menlo`·`Consolas`·`Monaco`)이 있으면 `monospace`, 아니면 첫 이름.
- **`box-shadow` 목록**: 겹마다 `inset? && <x> <y> [blur [spread]]? && <color>?`(색 앞·뒤 모두), 모든 겹이 파싱돼야 하고 **첫 outset 겹**을 그린다; 전부 inset 이면 `none`(바깥에 inset 그림자를 그리지 않게). `box-shadow: none` 도 받음(전엔 버려졌다).
- **`transition*`·`animation*`**(`-webkit-` 포함) 파싱만: 어떤 토큰열이든 원문을 `Keyword` 로 — 버림에서 빠지고 `parse_only` 로.
- **`flex` 단축 완성**: `none`(0 0 auto)·`auto`(1 1 auto)·`<grow> <shrink>? || <basis>`; 생략된 basis 는 **`0%`**(스펙 — `flex: 1` = `1 1 0%`; 전엔 basis 를 안 내 auto 였다)·생략된 grow/shrink 는 1·두 인수 뒤의 맨 `0` 은 basis(`flex: 1 1 0`). `flex: initial` 은 전역 키워드 경로. 레거시 flex 가 이제 `%` basis 를 주축 크기(행 = 내용 폭, 열 = 확정 높이) 기준으로 읽는다. 기존 테스트 `flex_shorthand_one_number_sets_grow_only`(250/150 → Chrome 과 같은 266.67/133.33) 갱신.
- **`background-size`**(페인트까지): `cover|contain|<축> <축>?`(길이·%·`auto`; 쉼표 목록은 첫 겹), 스타일 패스가 em/rem 을 px 로. 페인트: `ImageCommand::tile` = 크기를 입힌 이미지 전체의 자리(위치 = 상자 원점 + `background-position`), 상자가 자른다; 래스터는 타일 크기로 확대·축소(`raster/image.rs` 세 번째 모양). **`background-size` 가 없으면 예전 그대로**(늘이기 / 원래 크기 스프라이트) — 크기를 안 쓰는 페이지의 렌더링을 바꾸지 않으려고. `background` 단축: `<position> / <size>`·위치 키워드(`left/center/right/top/bottom` → %)·반복/첨부/상자 키워드를 받고, 위치를 `background-position-x/y` 로 낸다(한 값이면 y = `center`). **단축이 빠진 색·이미지를 초기값으로 리셋**(`transparent`·`none` — Chrome 과 같음; 전엔 색만 쓰면 이미지가 남았다). 위치·크기는 쓴 경우만 낸다(빠진 것을 `0 0`/`auto` 로 리셋하지 않음 — 근사, 기록). `background: none` 도 받음.
- **`clip: rect(…)`·`clip-path: inset(…)`**: `ClipBox` 로 보관(rect 는 쉼표·공백 모두, `auto` 변; inset 은 1~4값 + `round …` 는 받고 버림). 아무도 안 읽음 — 3단계 몫. 다른 `clip-path` 모양(`circle()`·`polygon()`·`url()`)은 예전처럼 버림.
- **`light-dark(a, b)`** → `a`(라이트 고정; 두 인자 모두 파싱돼야 함). 일반 값 경로의 함수라 `border`·`background` 단축 안에서도.
- **`inset`** 단축 → `top/right/bottom/left`(1~4값, `auto`·맨 `0`).
- **속성 표**(`css/properties.rs`): 전용 파서가 생긴 속성(`border`·`border-{side}`·`border-width/-style/-color`·`border-{side}-width`·`font-family`·`background`·`background-size`·`box-shadow`·`clip`·`clip-path`·`inset`)의 값 종류를 `Dedicated` 로 — 0b 분류표 규칙대로 그들의 버림 사유가 `multi-token` → `invalid-value`(2.1a 의 `grid-template` 과 같은 처리). `report.rs` 단정 갱신(`border: … !important` 예시를 아직 전용 파서 없는 `outline` 으로).
- **probe** `CONSUMED`: `all`·`background-size`·`border-{side}-color`·`border-{side}-style` 추가(엔진이 이제 읽음 — 엔진 사실이라 이 커밋).

**테스트 +19**(961 → 980): CSS 11(`border_shorthand_expands_to_longhands`·`font_family_list_parses`·`box_shadow_list_keeps_first_layer`·`transition_parses_without_drop`·`clip_rect_and_clip_path_inset_are_kept`·`light_dark_picks_first`·`flex_shorthand_three_values_with_auto_basis`·`background_size_two_lengths_and_keywords`·`inset_shorthand_expands_to_offsets`·`global_keywords_and_all_are_kept` + 1a 의 분류 단정 갱신), 스타일 3(`border_none_zeroes_width`·`global_keywords_resolve_in_the_cascade`·`all_unset_resets_earlier_declarations_and_ua_defaults`), 페인트 4(`border_side_color_paints`·`font_family_list_with_monospace_reaches_the_text_command`·`background_size_scales_sprite`·`background_size_tile_rasterises_at_the_given_scale` — 4×4 소스 → 지정 크기, 래스터 픽셀), 레이아웃 2(`inset_zero_pins_an_absolute_box_to_the_padding_box`·`flex_one_items_split_the_row_equally` — taffy·레거시 두 경로). 실패 먼저 확인(컴파일 실패 → 구현). **바뀐 기존 테스트**(의도한 스펙 변화): 스타일 없는 `border-left: 4px` 류 4곳에 `solid` 를 붙임(Chrome 에선 안 그려짐), `flex: 2` 의 basis, `background: red` 가 이미지 `none` 도 냄, `var()` 순환의 `initial` 이 "값 없음"으로 풀림, 입력 UA 기본을 덮는 `border-color` 를 면별 키로 확인.

**골든·스냅샷**: 관찰 골든 변화 없음. 렌더 스냅샷 `border_radius` 1개 — 픽스처의 `border: 2px solid #888` 이 이제 그려진다(모서리 각짐 — 위 후속).

**퍼저**: CSS 토큰 풀 속성에 `border-top`·`border-width/-style/-color`·`font-family`·`background-size`·`clip`·`clip-path`·`inset`·`transition`·`animation`·`all` 과 각 값 틀(`light-dark()`·`/ size` 배경·inset 그림자 목록 포함), 구조 생성기에 `border-{side}` 스타일 변형·`border-style: none|hidden`·`inset`·`flex: 1 1 0`/`1`/`auto 2`. 긴 실행(release) 레이아웃 2만 + CSS·이미지 무결.

**코퍼스**(라벨 `value-parsers`, `scroll-height` 대비, 동결 캐시·오프라인·release): 결과는 `ok*` 가 5곳 늘어난 것 외 동일(아래), 기대 컨트롤 25곳 동일, `[mb-budget]` 0, `load_ms` 합 5,030 → 5,041.
- **버린 선언 ours** 20곳 9,643 → **7,123**(−26%), 2차 5곳 4,387 → **3,941**; 합산 지표 5곳 4,645 → **4,213**(20곳 10,680 → 8,086). 사유별(20곳): `multi-token` 4,319 → **1,098**, `invalid-value` 5,324 → 6,025(전용 파서가 생긴 속성의 남은 버림이 이 사유로 옮겨 감 — 대부분 `var()`), `important` 0 유지. `Keyword` 폴백 705 → 659, 파싱만 9,336 → 9,641(`transition`·`animation` 이 버림에서 이리로). 사이트별 큰 감소: yna 1,490→948 · apple 715→340 · kakaocorp 991→690 · korea-kr 256→79 · google 338→200 · d2 146→16 · hani 171→54 · bbc 150→66 · microsoft 124→50 · hn 17→4.
- **남은 것의 정체**(동결 목표 ≤1,478 까지 5곳에서 2,735 더): github 3,452 가 5곳의 88% — 상위가 전부 shorthand 안 `var()`(`padding:5px var(--…)` 392·`border-radius:var(--…)` 239·`border:1px solid var(--…)` 195·`border-color:var(…)` 185·`margin:var(…) 0 0` 173·`background:var(…)` 143) = **1c**, 그다음 `calc()`(`margin-top:calc(var(--…) + …)` 144 등 = 5단계), 4·8자리 `#rgba` 색(`#0006`·`#0000` — `box-shadow` 125·`background-color` 52·`color` 20: **어느 단계에도 없는 작은 갭, 후속 후보**). naver 남은 것도 전부 `var()`(`border`·`rgb(var(--atcp-…))` 47 등) + `-webkit-box-shadow`/`-webkit-transform` 같은 `-webkit-` 별칭이 전용 파서로 안 감(후속 후보).
- **오프라인 미스 → `ok*` 5곳 늘어남**(naver 1·hani 3·kakaocorp 1·korea-kr 3, yna 1 → 21, github 2 → 6 — 전부 이미지): `background:` 단축이 위치·`/ size` 와 함께 이제 파싱돼 그 `url()` 을 새로 요청한다. 보충 녹화 안 함(0.2 규칙).
- **요소 목록 diff 2곳**:
  - korea-kr **+1** `link "… 정책포커스 바로가기"`(`pWiseMain=F1`) — 1q 에서 빠진 그 썸네일 링크. `.thumb` 의 `border:1px solid …`(전엔 multi-token 으로 버려짐)가 위아래 1px 를 줘 상자가 2px 가 되어 다시 관찰에 든다. Chrome 에서도 보이는 링크(높이는 5단계 `calc()` 까지 여전히 틀림).
  - bbc **−1** `button "Open menu"` — `button{…padding:0; background:none; border:0…}` 의 `border:0` 이 전엔 아무도 안 읽는 `border` 키에 들어가 UA 버튼 테두리 1px 가 남았고, 그 덕에 2×2 상자로 보였다. 이제 테두리가 정말 0(Chrome 과 같음)이 되자 버튼의 유일한 내용인 `<svg width="50" height="30">` 이 0×0 이라 버튼도 0 → 관찰에서 빠짐. **기존 갭: 인라인 `<svg>` 가 `width`/`height` 속성으로 고유 크기를 갖지 않는다**(`layout::intrinsic_width` 는 `<img>` 만). Chrome 에선 보이는 버튼. 고치면 모든 사이트의 SVG 아이콘 크기가 바뀌는 넓은 변경이라 1b 에선 안 함 — 후속.
- 문서 높이 소폭(docsrs 6,087 → 6,321 · kowiki +100 · enwiki +147 등 — 테두리·`flex: 1` basis 0%·배경 단축), 슬리버 413 → 411.

**1단계 완료 기준 — 1b 시점(1c 남음)**: 합산 지표 2차 5곳 **4,213** vs 동결 목표 ≤1,478(아직 미달 — 위 분석대로 남은 것의 대부분이 1c 의 `var()`); `!important` 버림 0 ✓; 기대 컨트롤 25곳 전부 이전과 같음 ✓; nav-fail/panic/timeout 그대로 ✓. naver 색·테두리는 `rgb(var(--…))`·`border:1px solid var(--…)` 라 1c 몫.

#### 1c — 커스텀 속성과 `var()` (2026-11-28)

설계 결정대로, 1r 의 `Rc` 스코프 사슬 위에.
- **파서**(`css/mod.rs`): `!` 앞까지의 값 안에서 `--*` 이거나 `var(` 가 (어느 깊이든 — 함수·괄호 안, 폴백 안 포함) 있으면 파싱하지 않고 원문을 남긴다: `Value::Unparsed { shorthand, raw }`(앞뒤 공백 제거한 원문). 커스텀 속성은 그대로(`--e:` 빈 값도 유효), 일반 속성은 `css::longhands_of` 의 longhand 마다 `shorthand: Some(이름)` 으로(longhand 가 자기 이름 하나면 `None`; `border-top-width` 처럼 다른 키에 저장되는 이름도 shorthand 로 표시해 다시 그 파서로). 잘못된 `var()`(`--이름` 이 아님, 이름 뒤 쉼표 아닌 토큰)는 선언째 버림. 파서는 더 이상 `Value::Var` 를 만들지 않는다 — **`Var` 는 제거 후보**(변형·`parse_var_function` 은 남겨 둠; 만나면 IACVT).
- **커스텀 속성**(`style/variables.rs::CustomScope`): 값이 이제 **치환 끝난 원문**(`Option<Rc<str>>`, `None` = guaranteed-invalid — 순환·치환 실패·`initial`). 노드의 자기 `--*` 를 그 노드에서 치환하고(같은 노드의 다른 `--*` 참조는 먼저 풀고, 진행 중 다시 만나면 순환 = invalid) 그 결과를 자손이 상속 — 스펙의 "계산값 상속"(`:root{--a:var(--b)}` 아래 자식의 `--b` 는 `--a` 를 안 바꾼다). `inherit/unset/revert/revert-layer` 는 부모 값 유지, `initial` 은 invalid.
- **치환**(`substitute`): cssparser 로 토큰을 걸으며(중첩 블록 포함) `var(` 자리에 찾은 원문, 없으면 폴백(그 안의 `var()` 도 치환)을 붙이고 나머지는 원문 조각을 그대로 — 위치가 한 원문의 오프셋이라 중첩 블록도 같은 기준. 정의 없음·invalid + 폴백 없음 → 실패. **상한 `MAX_SUBSTITUTED_LEN` 64KiB**(견고성 규칙 3: `--a1: var(--a0) var(--a0)` 사슬은 단계마다 두 배 — 넘으면 invalid; 테스트 40단 = 2^40).
- **재파싱**: 치환한 글을 shorthand(있으면) 또는 속성 이름의 파서로(`css::parse_property_value` = `parse_declaration_value` 를 `parse_entirely`) → 그중 자기 longhand 값. 스타일 패스당 메모 `(파싱 이름, 치환된 글)` → 선언들(`Cascade::var_memo`). 단위 없는 0 승격·전역 키워드는 그 경로가 이미 처리(전역 키워드는 뒤의 `resolve_css_wide_keywords` 가 푼다).
- **IACVT = `unset`**: 치환·재파싱 실패 → 값을 `Keyword("unset")` 로 → 전역 키워드 단계가 상속 속성이면 부모, 아니면 초기값. 캐스케이드의 앞선 선언으로 **되돌아가지 않는다**. 재파싱 결과가 그 속성 값 종류가 안 받는 단일 키워드(`color: var(--x)` + `--x: foo`, 지원 안 하는 단위 `100vw`)여도 같은 처리 — 일반 파서가 아무 식별자나 `Keyword` 로 받는 정책 때문에 그대로 두면 상속을 깨는 값이 남는다; probe 에는 `var_keyword_fallback` 으로 따로 센다.
- **probe 키**(`style::declaration_key`): `Unparsed` 는 `"{shorthand 또는 이름}: {raw:?}"` — 한 shorthand 의 longhand 넷이 같은 키라 probe 가 선언 하나로 센다(P 의 "규칙 안 같은 키 한 번").
- **찾아서 고친 버그**(코퍼스 첫 실행): `var(--x, 폴백)` 에서 `--x` 가 정의돼 있으면 폴백을 읽지 않고 돌아왔는데, cssparser 의 `parse_nested_block` 은 블록을 **`parse_entirely`** 로 돌려 남은 토큰이 있으면 에러다 → 정의된 이름 + 폴백이 전부 IACVT(GitHub `color: var(--fgColor-default,var(--color-fg-default))` 등 846). 폴백 토큰도 소비하게 고침 + 테스트 `defined_var_ignores_an_unresolvable_fallback`.
- **비용**: GitHub `load_ms` 772 → 836(+8%), `[mb-budget]` 0 — 메모 덕에 재파싱은 (이름, 글) 쌍마다 한 번.
- **깊이 상한 — 리뷰 후 추가**(견고성 규칙 2): `var()` 를 찾고 치환하는 걸음이 중첩 블록·폴백·같은 노드의 커스텀 속성 사슬마다 재귀했고 상한이 없었다 — `width: var(--x, ((((…5만…))))`, `var(--a, var(--a, …))` 5만 겹, `--a0: var(--a1); …` 1만 고리면 스택 넘침 = abort(2MB 스레드에서 재현). **`css::MAX_VAR_NESTING` = 32**(`MAX_MEDIA_NESTING` 과 같은 값):
  - 파싱 때(`scan_var_references`)는 블록·`var()` 중첩이 32 를 넘으면 선언째 버린다(더 이른 쪽).
  - 일반 값의 함수 중첩(`parse_value` → `parse_function` 재귀 — 1b 의 `light-dark(light-dark(…))`, 그라디언트 정지점)도 같은 상한: 스레드 지역 깊이 카운터, 넘으면 그 값이 무효.
  - 치환 때는 블록·폴백·사슬 고리를 **한 깊이로 합산**해 32 를 넘으면 IACVT. 커스텀 속성 결과가 "이 걸음이 시작한 깊이 때문에" 실패한 경우는 캐시하지 않아(사슬 맨 위는 캐시) 같은 집합의 짧은 사슬(`--a9990` → 끝까지 10고리)은 순서와 무관하게 풀린다.
  - 테스트 `crates/mb-dom/tests/var_depth.rs`(자식 프로세스 · 2MB 스레드 — `dom_depth.rs` 와 같은 방식, 고치기 전 abort 확인): 다섯 모양 + 상한 안(20고리·폴백 10겹)은 그대로 동작. CSS 퍼저 토큰 수프에 6% 확률로 20~5,000 겹 블록/폴백/`light-dark()`·최대 3,000 고리 사슬. 코퍼스(`var-depth`)는 `var` 와 `load_ms` 외 25곳 전부 동일.

**테스트 +16**(980 → 996; 리뷰 후 깊이 테스트 +3 — `var_depth.rs` 1 + `variables.rs` 단위 2: `substitute` 를 직접 불러 파싱 때 검사 없이 폴백·블록 깊이가 상한에서 멈추는지. cssparser 의 파서당 중첩 상한 75 는 폴백마다 새 파서라 다시 0 부터라 그것만으론 안 막힌다): CSS 4(`custom_property_keeps_raw_tokens`·`var_values_stay_unparsed_and_shorthands_expand_per_longhand`·`parses_var_reference_*` 둘을 `Unparsed` 로 갱신) + 갱신 2(`parses_custom_property_declaration`·`custom_property_important_strips_flag` — 커스텀 속성은 원문), 스타일 11(`rgb_of_var_resolves_after_cascade`·`shorthand_with_var_expands_after_substitution`·`invalid_after_substitution_falls_back_to_inherited`·`iacvt_does_not_fall_back_to_earlier_declaration`·`shorthand_var_and_longhand_cascade_in_order`(두 순서)·`custom_property_inherits_substituted_value`·`var_fallbacks_take_commas_and_nest`·`custom_property_cycle_is_invalid_and_uses_the_fallback`·`exponential_var_expansion_is_bounded`·`unparsed_values_never_reach_the_styled_tree`·`defined_var_ignores_an_unresolvable_fallback`). 실패 먼저 확인. P 의 보고 테스트 3개는 새 경로로 그대로 통과. 골든·스냅샷 변화 없음.

**퍼저**: CSS 토큰 풀에 `--a`·`--b` 선언(순환·두 배 사슬·빈 값·`inherit`)과 값의 15% 를 `var()` 꼴(`rgb(var())`·`calc(var())`·중첩 폴백·정의 없음·닫히지 않은 `var(`)로, 구조 생성기에 `--g/--h` 정의와 `var(--g, 길이)` 사용 + 생성기 검사(200 중 ≥20). 긴 실행(release) 레이아웃 2만 + CSS·이미지 무결.

**코퍼스**(라벨 `var`, `value-parsers` 대비): 결과·기대 컨트롤·오프라인 미스 25곳 동일, **요소 목록 diff 없음**(25곳), `[mb-budget]` 0, `load_ms` 합 5,041 → 5,161. 버린 선언 ours 20곳 7,123 → **1,806**, 5곳 3,941 → **335**; IACVT 20곳 304 → 572(5곳 117 → 248 — 파싱 때 버려지던 `calc(var())`·정의 없는 토큰이 이제 계산값 시점에 세어짐); 합산 지표 20곳 8,086 → **3,037**, 5곳 4,213 → **738**. 첫 화면 요소 902 → 895(github 94 → 90·enwiki 91 → 88 — 토큰이 풀린 간격·테두리로 위쪽이 조금 길어짐, 요소 목록 동일). 문서 높이 소폭(github 6,952 → 7,300 등).

### 1단계 완료 판정 (2026-11-28, 라벨 `var`)

| 기준 | 결과 |
|---|---|
| 버린 선언(ours + IACVT + `Keyword` 폴백 + var 폴백) 2차 5곳 ≤1,478(0b 7,389 의 −80%) | ✅ **738**(ours 335 · IACVT 248 · 폴백 155 · var 폴백 0) — P 의 "전" 9,627 대비 −92%. 20곳 18,204 → 3,037(−83%) |
| `!important` 버림 0 | ✅ 0(1a 부터) |
| naver: `rgb(var(--atcp-…))` 색 47건, 테두리 | ✅ 색 47건과 `border-top:1px solid rgb(var(--atcp-…))` 18건이 더는 버림·IACVT 아님(풀림). 남은 naver IACVT 108(`background-color/color/border: var(--color-…)`)은 토큰이 `:where(:root,:host)` 규칙에 있어 셀렉터째 버려진 것 = **4단계**. 미리보기 PNG 로 테두리를 눈으로 본 것은 아님(텍스트 수치만) |
| nav-fail/panic/timeout 0단계 그대로 | ✅ ok 13 + ok* 7 · nav-fail 1(K5) · blocked 2 · skipped 2 · panic 0 · timeout 0. `ok*` 는 1a·1b 가 살린 배경 이미지의 새 요청(보충 녹화 안 함) |
| 기대 컨트롤 + 사라진 요소 판정 | ✅ 1단계 동안의 요소 diff 는 하위 부분 메모마다 판정(1r github +1·enwiki −3, 1a yna −4·microsoft −2·docsrs −1·enwiki +3, 1p docsrs +15, 1q korea-kr −1, 1b korea-kr +1·bbc −1, 1c 없음). 기대 컨트롤 누락 7/20(naver·daum·hani·korea-kr·velog·d2·google — 전부 JS/ES 모듈 원인, 0b 기준선의 8 에서 enwiki 가 빠짐) |
| `[mb-budget]` 0 | ✅ |

**남은 버림 순위**(5곳 / 20곳 합 — 사이트별 `css.txt` 상위 80개 기준 근사):
1. **`calc()`/`min()`/`max()`/`clamp()`** — 5곳 ~56 · 20곳 ~589 + IACVT 의 `calc(var())` 일부 → **5단계**.
2. **`-webkit-` 별칭**(`-webkit-box-shadow`·`-webkit-transform`·`-webkit-transition` 류가 전용 파서로 안 감) — 5곳 ~14 · 20곳 ~273 → 이름 정규화 한 곳(작은 후속).
3. **4·8자리 `#rgba`** — 5곳 ~126(거의 github) · 20곳 ~141 → `parse_hex_color_str` 에 4·8자리(작은 후속).
4. **IACVT 의 정의 없는 토큰** — 5곳 248: `:where(:root,:host)` 토큰(naver 108 등) = **4단계**, `@layer` 안 토큰(GitHub `@layer primer-brand{…}` 의 `--brand-*`) = **2.2**.
5. 그 밖 소수: `transform` 값(20곳 ~115), `gap` 두 값(~93, multi-token — 전용 파서 없음), `outline`(~61), `grid-template-columns` 의 `repeat()`/줄 이름(~59, 5단계), `overflow` 두 값(~18).



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

#### 2.1 구현 메모 (2026-11-27 — 설계와 달라진 점 포함)

**probe 변경**(엔진 전에 따로, 옛 엔진으로 "전" 측정):
- `css/report.rs`: `AtRuleSeen` 에 `rules`(본문이 낸 규칙 수, 안쪽 `@media` 포함)·`parent`(감싼 `@media` 의 인덱스) — 중첩 블록의 규칙을 한 번만, 안쪽 블록에서 센다. `@media` 프렐류드는 160자에서 자르지 않고 통째로(probe 가 쿼리로 다시 읽음). `css::media_verdict(prelude, w, h) -> MediaVerdict { applies, invalid_queries }` = "엔진이 이 블록을 이 뷰포트에서 적용하는가" — probe 커밋에선 `applies: true`·`invalid_queries: 0`(옛 엔진의 사실), 엔진 커밋이 본문을 `media::evaluate` 로 바꾼다(엔진 커밋에 들어가는 report.rs 한 함수).
- `compat_probe`: 열 `media_rules`·`media_applied`(1280×800 에서 엔진이 적용하는 규칙)·`media_wrong`(`media_kind` 휴리스틱이 "wrong at 1280x800" 이라 한 블록의 규칙 — 감싼 블록 포함)·`media_wrong_applied`(그중 적용된 것 — 목표 0)·`media_invalid`(파싱 실패 쿼리 항목)·`media_attr_sheets`(`media` 속성이 있는 `<link rel=stylesheet>`·`<style>` 수). `css.txt` 의 "@media conditions" 표는 범주별 블록·규칙·적용 규칙 세 수. probe 자체의 인라인 CSS 수집도 `<style media>` 를 엔진처럼 `@media` 로 감싼다(외부 시트는 엔진이 합친 문자열을 그대로 읽으므로 "전" 에는 `<link media>` 규칙이 `@media` 밖으로 세어진다 — `media_attr_sheets` 로 규모만 보임, 20곳 합 4).

**엔진**:
- **`Rule::media: Option<Rc<MediaBlock>>`** — 설계의 `Rc<MediaQueryList>` 대신 블록 래퍼: `MediaBlock { queries: MediaQueryList, parent: Option<Rc<MediaBlock>>, index }`. 중첩 `@media` 의 AND 를 쿼리 목록끼리 합치는 대신 `parent` 사슬로 표현(목록은 OR 라 AND 합성이 곱이 된다). 같은 블록의 규칙은 같은 `Rc` 를 공유. `Stylesheet::media: Vec<Rc<MediaBlock>>` = 시트의 모든 블록(부모가 자식보다 앞) — `index` 가 여기 자리. 공개 평가 함수는 설계대로 `css::media::evaluate(&MediaQueryList, &MediaContext) -> bool`.
- 캐스케이드: `style_tree_with_state(…, &MediaContext)` 가 패스 시작에 시트마다 `media::evaluate_blocks`(블록 수만큼 bool, 부모 결과를 이어받아 재귀 없음) 한 번 → `specified_values` 는 `results[sheet][block.index]` 를 읽어 거짓이면 건너뜀. `rule_order` 는 건너뛴 규칙 자리도 유지(나머지 순서 불변 — `media_does_not_change_rule_order`). `style_tree`·`style_tree_with_hover`(테스트·옛 호출)는 `MediaContext::DESKTOP`(1280×800, 에이전트 기본 뷰포트).
- 지원 범위(`css/media.rs`): 설계 목록 전부 + 범위 문법(`<`·`<=`·`>`·`>=`·`=`, 값이 앞·양쪽, 방향이 섞이면 무효) + 좁은 `calc`(길이 `+`/`-` 만, `-webkit-calc` 포함 — `*`·`/`·숫자 항은 unknown) + 단위 `px em rem vw vh vmin vmax cm mm q in pt pc`(em/rem = 16px). 결정한 값: `device-width/height/aspect-ratio` = 뷰포트(에이전트의 "기기"는 뷰포트), `resolution`·`-webkit-(min-/max-)device-pixel-ratio` = 1dppx, `color` 8·`color-index` 0·`monochrome` 0, `overflow-block/inline: scroll`, `video-dynamic-range: standard`, `orientation` 은 높이 ≥ 폭이면 portrait. **unknown**(= 거짓, `not` 붙여도): 모르는 기능(`-ms-high-contrast`·`prefers-reduced-data`·`grid`·`--custom-media`), 기능이 안 받는 값(`(min-width: wide)`), `ex`/`ch` 단위, 함수 꼴 `<general-enclosed>`. MQ4 의 3값 논리(Kleene): `or` 에 참이 하나 있으면 참, `and` 에 거짓이 하나 있으면 거짓. 무효 쿼리(`screen and`, 타입 뒤 `or`, `only (color)`)는 그 쉼표 항목만 `not all`, 빈 목록(`@media {}`·`media=""`)은 `all`.
- **상한**(견고성 규칙 2·3): `media::MAX_MEDIA_NESTING` = 32 — 쿼리 안 괄호가 그보다 깊으면 unknown, 시트의 `@media` 중첩이 그보다 깊으면 그 블록 규칙을 버림(파서 재귀 상한; 실사이트 1~3). 쿼리 목록 길이·괄호 수는 선형(5만 항목 목록·1만 겹 괄호·1만 겹 블록 테스트). CSS 토큰 퍼저에 `@media` 생성(프렐류드 수프, 1–3겹 중첩, 10% 는 30–200겹으로 상한 넘김, 닫지 않음 10%) + 수프 쿼리를 0×0·`f32::MAX` 뷰포트로 평가 + `scope_sheet` 경로 + 고정 케이스 `media_extremes_stay_bounded`. 긴 실행(release) 10만 입력 무결(8.6초).
- `<link media>`·`<style media>`: 설계 결정대로 합칠 때 `@media {속성값} { … }` 로 감싼다 — `css::media::scope_sheet(css, media)` 한 곳(`resource::load_stylesheets` 가 `<link>` 의 `media` 를 들고 와 받은 본문을 감싸고, `state` 의 인라인 수집이 `<style>` 마다 감쌈). `media` 가 없거나 비었거나 `all` 이면 그대로. **감싸기가 깨지지 않게** 두 가지를 고쳐 쓴다: 속성의 쉼표 항목 중 `{ } ; " ' \` 나 `/*` 가 든 것은 뺀다(프렐류드를 끝내거나 삼킴 — 어차피 무효 쿼리 = `not all`; 다 빠지면 `not all`), 시트 최상위의 짝 없는 `}` 는 `\}`(식별자)로 — 래퍼를 일찍 닫아 나머지가 모든 매체에 적용되는 것을 막고, 원래 `}` 가 버리던 규칙을 똑같이 버린다. 남은 근사: 시트 끝의 **열린** 블록·주석·문자열은 래퍼의 `}` 를 삼켜 뒤에 합쳐진 시트까지 그 `@media` 안에 넣는다 — 감싸기 전에도 `join("\n")` 이 같은 식으로 뒤 시트를 망가뜨리던 기존 근사라 이번엔 안 고친다(시트별 파싱은 2.2 의 레이어 후처리 때 같이 검토).
- **뷰 캐시 키**: `CachedView` 에 `media_results: Vec<u64>`(`css::media::media_bitset` — 모든 시트의 블록 결과를 64비트 묶음으로 + 시트마다 경계 단어 하나)·`vh_height`(2단계는 항상 0) — 높이 자체는 넣지 않는다. 비교는 다른 키가 다 맞았을 때만 계산(마지막 조건). `MediaContext` 의 높이 = 창 높이 − `CHROME_HEIGHT`(페이지 영역; 에이전트 1280×800 이면 800). `build_document_view` 에 `viewport_height`(페이지 영역 높이) 인자 추가. 테스트 `height_only_resize_restyles_only_across_a_query_boundary`.
- **비용 실측**(release, 동결 캐시): GitHub(tokio 저장소) `@media` 블록 **517**(쿼리 534, 블록 안 규칙 2,809, 1280×800 에서 참 307) → 비트셋 **3.6µs/프레임**. 코퍼스 최대 yna 블록 1,571 → **17µs**, apple 489 → 3.7µs, enwiki 88 → 0.5µs. 캐시 적중 프레임에만 드는 비용이고 16ms 프레임의 0.1% — "비싸면 리사이즈 때만" 대안은 필요 없음.
- 실패 테스트 먼저: 캐스케이드 테스트(`media_rule_applies_only_when_matching`·`nested_media_is_and`·`media_does_not_change_rule_order`)는 `style_tree_with_state` 새 인자 전에 써서 실패(컴파일) 확인. 상태·리소스 테스트 4개는 구현 직후에 썼지만 각각 옛 동작(속성 무시·모든 `@media` 참·폭만 키)에서 실패하는 단정이다.
- 골든: `compat3.observation.txt` 만 바뀜 — 머리 주석 3번 그대로(데스크톱 내비 Features·Pricing 보임, "Open navigation" 사라짐, `page_height` 322 → 318). 2번(검색 폼 `.hide-sm`)은 그대로 보임(정상). 다른 골든·스냅샷 변화 없음.

#### 2.1a 위키 그리드 회귀 수정 (2026-11-27)

2.1 뒤 enwiki·kowiki 에서 navbox 링크가 관찰에서 빠지고 슬리버가 폭증한 것(진행 표 2행)을 1단계 전에 고친다. 범위는 코퍼스가 쓰는 만큼 — `grid-template` 단축과 `minmax()` 트랙. 트랙 속 `calc`·`repeat()`·`fit-content()`·줄 이름 `[a]` 는 5단계에 그대로 둔다(지금처럼 선언째 버림).

**원인**(동결 캐시로 진단, 사이트 방문 없음) — 세 겹:
1. `@media screen and (min-width:1120px)` 의 Vector 2022 데스크톱 격자가 이제 적용되는데, `grid-template` 단축은 전용 파서가 없어 버려지고 `grid-template-columns: minmax(0,1fr)` 류는 `minmax()` 때문에 선언째 버려졌다. 영역(`grid-template-areas`)만 남은 그리드가 열을 `1fr` 로 균등 분할.
2. 레거시 그리드(`layout/grid.rs`)는 항목을 **컨테이너 전체 폭으로** 먼저 배치하고 열 위치로 옮기기만 했다(줄이지 않음). 그래서 `pageContent` 가 1,192px 폭으로 x=640 에서 시작해 넘쳤다. enwiki 의 그리드는 조상이 boundary(body 에 out-of-flow 자식)라 taffy 가 아니라 이 레거시가 배치한다.
3. 그 아래 navbox 는 행이 전부 `colspan="2"` 셀인 표다. 레거시 표가 다중 열 셀을 열 크기 계산에서 빼서 열이 0px → 셀 0폭 → 링크 494개가 슬리버. **그리드와 무관한 기존 표 버그**로, 합성 재현은 폭 636·1,192 둘 다 0폭이다. kowiki 의 navbox 는 2.1 전에도 이 버그로 빠져 있었다(이번에 +230 이 처음 보임). enwiki 의 것이 2.1 전엔 왜 0폭이 아니었는지는 확인하지 못했다.

**Chrome 해석**(캐시의 CSS 를 스펙대로 계산):
- `.mw-page-container-inner` — 1280 에서 내용 폭 1,192(= 1280 − 2×2.75rem). `grid-template: min-content 1fr min-content / 12.25rem minmax(0,1fr)` + `column-gap: 24px` → 열 196 / 간격 24 / 972. 영역: siteNotice(1행 전체), columnStart(2행 1열), pageContent(2행 2열), footer(3행 전체).
- `.mw-body`(972px) — `min-content min-content min-content 1fr / minmax(0,59.25rem) min-content` → 1열 = min(948, 972 − columnEnd 의 min-content).
- 우리 결과: 196 / 24 / 972 는 일치. `.mw-body` 는 1열 636 · columnEnd 336 — 우리 자연 폭은 max-content 근사라 Chrome 보다 넓을 수 있다(대조 안 함).

**taffy 0.15.1**: `MinMax` 트랙(최소 length/percent/auto/min-content/max-content, 최대는 여기에 fr·fit-content), `grid_template_areas`, 영역 이름 → `-start`/`-end` 암묵 선 해석(`GridPlacement::NamedLine`)이 모두 있다 → 브릿지는 매핑만 추가. 영역 그리드를 taffy 로 옮기는 일은 이번엔 안 한다 — enwiki 는 어차피 boundary 안이라 효과가 없다.

**변경**:
- **파서**(`css/grid.rs`):
  - `TrackSize::{MinContent, MaxContent, MinMax(TrackBreadth, TrackBreadth)}` + `TrackBreadth` — 5단계 설계의 모양 그대로, `Copy` 유지.
  - `minmax()` 의 최소에 `fr` 이 오면 무효(선언 버림, 브라우저와 같음). 무단위 `0` 은 길이로 받는다. `none` 키워드(`grid-template-areas/-columns/-rows: none`).
  - `grid-template` 단축 세 형태: `none` / `<rows> / <columns>`(영역은 `none` 으로 리셋) / `[<string> <track-size>?]+ [/ <columns>]?`.
  - `css/properties.rs` 의 `grid-template` 값 종류 Other → Dedicated — 전용 파서가 생겼으므로 버림 사유가 multi-token → invalid-value 로 바뀐다(0b 분류표의 규칙 그대로).
- **taffy 매핑**(`taffy_bridge.rs` `track_to_taffy`): 새 변형 세 개.
- **레거시 그리드**(`layout/grid.rs`) — 5단계 설계의 "레거시는 `minmax(0,Nfr)` 정도로 최소화" 보다 넓다. enwiki 의 그리드가 레거시를 타므로 필요했다:
  - `column-gap`/`row-gap`/`gap`(단일 길이).
  - 항목을 **자기 영역 폭으로** 배치: 폭이 없으면 영역을 채운다(`justify-self: stretch`, `inline::layout_grid_item`). 자연 폭 사전 측정은 내용으로 크기가 정해지는 열(`auto`·`min-content`·`max-content`·그런 것이 든 `minmax`)에서 시작하는 항목만 한다. 측정한 상자가 영역에 들어가면 다시 배치하지 않는다 — 항목당 배치 1회, 넘칠 때만 2회.
  - 트랙 해석 `resolve_tracks`: 고정 → 내용 → `minmax(<min>, <길이>)` 는 바닥에서 시작해 남는 공간으로 상한까지 고르게(fr 보다 먼저 — 스펙의 "maximize tracks") → `fr` 은 스펙의 "find the size of an fr"(바닥이 몫보다 큰 트랙은 빼고 다시 나눔, 가중치 합이 1 미만이면 1). 5단계 설계가 근사로는 틀린다고 짚은 `minmax(300px,1fr) 1fr` @400 = 300/100 을 두 경로 모두 테스트로 고정.
  - 높이가 정해지지 않은 그리드의 `fr` 행은 내용 크기 — Chrome 과 같다. 전엔 0 으로 접혔다(위키의 `1fr` 본문 행).
  - `auto` 최소는 0 으로 근사(min-content 를 따로 재지 않음).
- **레거시 표**(`layout/table.rs`): 다중 열 셀도 측정해, 걸친 열(사이 간격 포함)보다 넓으면 그 열들을 고르게 넓힌다. 셀마다 측정은 한 번.
- **테스트 +13**: 파서 6 · 레이아웃 5(영역 항목이 트랙 폭으로, 영역 안 표가 0폭 안 됨, minmax 상한 + min-content, taffy 의 minmax, fr 바닥 300/100 두 경로) · 표 2.
- **퍼저**: 구조 생성기에 `grid-template` 단축 두 형태 + 생성기 검사(단축·`minmax` 각 200 중 ≥10), CSS 토큰 풀에 `grid-template` 와 `minmax`/`min-content` 값. 긴 실행(release) 레이아웃 3만·CSS 10만 무결.

**코퍼스**(라벨 `grid`, `media-after` 대비, 동결 캐시·오프라인·release): 결과·기대 컨트롤·오프라인 미스는 25곳 동일. 수치는 진행 표 2.1a 행.

**남은 것**:
1. enwiki 슬리버 71 + 2.1 전 대비 링크 11개(Blink·Gecko·WebKit 등 엔진 그룹 이름): navbox 의 `th.navbox-group{width:1%; white-space:nowrap}` 셀이다. 레거시 표가 열을 평평한 비율로 줄이고 min-content 바닥이 없어서, 1% 열이 6px 남짓 → 좌우 패딩 28px 에 밀려 0이 된다. 표 알고리즘 몫이고 그리드가 아니다 — 후속 작업.
2. mdn 슬리버 0 → 25: 레이아웃 열이 전부 사용자 속성이다(`grid-template-columns: var(--layout-2-sidebars)` — 줄 이름 `[…]`, `minmax(var(--layout-sidebar-min),1fr)`). 그 분기를 고르는 미디어 쿼리 `(width < calc(1rem * 2 + (15rem + 2rem) * 2 + 31rem))` 도 2.1 의 좁은 calc 밖이라 unknown → 거짓이다(Chrome 1280 에선 참). 그래서 영역만 남은 5열 균등(256px)이 된다. 전엔 항목이 열보다 넓게 배치돼 넘쳤을 뿐이고, 지금은 열 폭에 맞춰져 좁아졌다. 요소 목록·컨트롤 변화는 없다. 1단계(여러 토큰 사용자 속성 → `Unparsed`) + 5단계(줄 이름, calc 곱셈) 몫.

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
4. **선행 수정 — 퍼센트 기준**: ~~`height`/`min-height`/`max-height` %~~ → **[1p](#1p--퍼센트-높이-기준-2026-11-28) 에서 고침**(포함 블록 높이가 확정일 때만, 아니면 `auto`/`0`/`none`). 남은 것: `relative_offset` 의 `top/bottom %` 도 폭 기준이다(`layout/properties.rs` `relative_offset`) — 스펙대로 확정 높이 기준, 아니면 `auto`. 클리핑이 붙기 전에 이것까지(3단계 첫 하위 커밋). 1p 의 `containing_block_height()` 를 그대로 쓰면 된다.

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

GitHub 간격(`margin-top: calc(var(--base-size-8) + var(--Layout-row-gap))` 류 수백), Google·위키의 `calc(100% - 16px)` 을 살린다. 위키 그리드(`15.5rem minmax(0,1fr)` — 2차에서 본문이 밀리고 잘린 원인)는 [2.1a](#21a-위키-그리드-회귀-수정-2026-11-27)에서 `minmax()`·`grid-template` 단축으로 먼저 살렸다.

### 세부 항목

1. `calc()` — `+ - * /`, 괄호, 중첩 `calc()`, 단위 혼합(`px`, `%`, `em`, `rem`, `vw`, `vh`, `ch`, `pt`, 무단위 수).
2. `min()`, `max()`, `clamp()`.
3. 값 속 `var()` — 1단계 `Unparsed` 경로가 이미 처리(치환 후 재파싱 시 `calc` 가 여기서 파싱됨). 이 단계에선 `calc(var(--a) * -1)` 같은 조합의 테스트만 추가.
4. 그리드 트랙: `repeat(n, …)`, `repeat(auto-fill|auto-fit, minmax(…))`, `fit-content()`, 트랙 속 `calc()`·`var()`, 줄 이름 `[a]`(mdn 레이아웃). — `minmax(a, b)`·`min-content`/`max-content`·`grid-template` 단축은 **2.1a 완료**.
5. `vw`/`vh` 단위 전반(지금은 `Keyword("100vw")` 로 떨어짐).

### 설계 결정

- **표현**: `Value::Calc(Box<CalcExpr>)`, `CalcExpr = Sum(Vec<(Sign, Product)>) | …` 대신 정규화된 형태 `CalcSum { px: f32, percent: f32, em: f32, rem: f32, vw: f32, vh: f32 }` + `Min/Max/Clamp(Vec<Calc>)` 노드. 곱·나눗셈은 파싱 때 숫자 쪽으로 접는다(`calc(2 * 8px)` → px 16; 두 길이의 곱은 명세상 무효 → 선언 버림).
- **`Value::Length` 직접 매칭이 많다 — 변경이 한 곳에 모이지 않는다**: `length_value` 외에 `taffy_bridge.rs` 12곳(`dimension` ~234, `dimension_with_intrinsic` ~243, `lpa_*` ~266–283, `lp` ~287, `number_value` ~336, `has_percent_inset_or_padding` ~722), `flex.rs` ~270·294, `grid.rs` ~142·197, `inline.rs` ~655–657, `table.rs` ~134·178, `display_list.rs` ~424·512·597·940, `view.rs` ~456. 특히 `matches!(…, Value::Length(..))` 로 "폭이 명시됨" 을 판정하는 곳은 `Calc` 를 `auto` 로 오판한다. **결정**: 5단계 첫 하위 커밋에서 `properties::length_like(node, name) -> Option<LengthLike>`(Length·Percent·Calc 를 한 타입으로) 헬퍼를 만들어 이 곳들을 옮긴 뒤 `Calc` 를 추가한다.
- **해석 시점**: `em`/`rem` 은 스타일 패스에서 이미 px 로 바꾸는 자리(`style/mod.rs` 의 font-size 해석)에서 함께 → 남는 건 `px + percent + vw/vh`. `vw/vh` 는 2단계에서 생긴 `MediaContext` 로 스타일 때 px 로. 그러면 레이아웃에 남는 건 `px + percent` — `layout/properties.rs::length_value(node, name, base)` 가 `Value::Calc` 를 받아 `px + percent/100*base` 로 계산(`min/max/clamp` 는 재귀). 레이아웃 코드의 다른 부분은 `length_value` 만 부르므로 변경이 한 곳에 모인다.
- **taffy 브릿지 — 전제 정정**: taffy 0.15.1 에는 `calc` 가 **있다**(기본 feature: `Dimension::calc(ptr)`, `LayoutPartialTree::resolve_calc_value(ptr, basis)`). 다만 우리가 쓰는 고수준 `TaffyTree` 는 `resolve_calc_value` 가 **0.0 고정**이다(`taffy_tree.rs` ~405행). 그리고 처음 안이었던 "boundary 로 보내 레거시가 계산" 은 불완전하다 — boundary leaf 도 자기 `width`/`min/max-width`/`margin` 은 `boundary_taffy_style`(~624–653행)이 taffy 로 넘기므로 calc margin 은 0, calc max-width 는 auto 가 된다. "기준 폭을 알면 브릿지에서 px 로" 도 `build_taffy_node`(~591행)가 compute 전에 스타일을 만들어 기준을 아는 경우가 거의 없다.
  - **5단계 시작 때 스파이크(0.5일)**: (a) 저수준 trait(`LayoutPartialTree` 등)을 우리 트리에 직접 구현해 `CalcSum` 포인터를 넘기고 taffy 안에서 `basis` 로 풀게 하는 안, (b) `TaffyTree` 를 유지하고 calc 가 있는 상자를 `px`/`%` 중 하나로 근사하는 안 — (a) 가 정답이지만 브릿지 재작성 규모를 재 보고 결정, 결과를 이 절에 기록.
  - 흔한 모양 `width: min(100% - 2rem, 1200px)` 을 가진 flex/grid 컨테이너가 기준 사례.
- **그리드**: `TrackSize` 는 `Copy` 라(`css/mod.rs` ~130행) `Box` 를 넣지 않는다 — `MinMax(TrackBreadth, TrackBreadth)`(Box 없는 min/max 전용 enum)은 **2.1a 에서 추가됨**, 남은 건 `FitContent(f32)`. `repeat(n)` 은 파싱 때 펼침, `repeat(auto-fill|auto-fit, …)` 은 `Value::TrackList` 모양을 바꿔(`TrackList{ before, auto: Option<(AutoKind, Vec<TrackSize>)>, after }`) 레이아웃 때 개수 결정 — 개수는 **gap 포함**, max 가 확정이면 max·아니면 min 으로 센다, `auto-fit` 은 빈 트랙을 접는다.
- **그리드 해석은 가능하면 taffy 로**: taffy 에는 `minmax`·`repeat(auto-fill)`·`fit-content` 와 스펙의 fr 알고리즘("find the size of an fr")이 이미 있다. 레거시 `grid.rs::resolve_grid_columns` 에 처음 안의 근사("min 을 고정 예산에 넣고 남는 공간을 fr 로 분배")를 넣으면 스펙과 다르다 — `minmax(300px,1fr) 1fr` 를 400px 에 두면 스펙 300/100, 근사 350/50. 또 `1fr` 단독은 `minmax(auto,1fr)` 라 긴 내용(pre·표)이 줄지 않는다. **결정**: 새 트랙 함수가 든 그리드는 taffy 경로로 보내고(`taffy_bridge.rs` grid 트랙 변환에 매핑), 레거시 `grid.rs` 확장은 `minmax(0, Nfr)`(위키 모양 — 근사로도 정확) 정도로 최소화. **2.1a 에서 바뀜**: 위키 그리드가 boundary 안(레거시)이라, 레거시에 `minmax`(상한 트랙을 fr 보다 먼저 키움)·스펙의 fr 몫 계산(위 300/100 사례 정확)·간격·영역 폭 배치를 넣었다. 영역 그리드를 taffy 로 옮기는 일은 여전히 열려 있다.
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
- 그리드: `grid_repeat_n`, `grid_repeat_auto_fill_count_includes_gap`, `grid_auto_fit_collapses_empty`. (`grid_minmax_min_not_added_to_fr_share` 와 위키 모양 `grid_areas_item_is_laid_out_at_its_track_width` 는 2.1a 에 있음.)
- 브릿지: `calc_width_on_flex_container_resolves`(스파이크 결과에 맞춰), `explicit_width_check_accepts_calc`(`matches!(Length)` 오판 회귀).
- `line_height_unitless_calc_inherits_as_factor`, `flex_basis_percent_uses_main_axis`.
- 퍼저 CSS 풀에 `calc()` 추가 — 0 나누기·거대값(`minmax()`·`grid-template` 은 2.1a 에서 추가됨).

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

버린 선언·규칙 목표(≤1,500 · ≤380 등)는 [공통 규칙](#범위와-순서)대로 `ours` 기준이다 — 0b 기준선(2차 5곳, 새 캐시)으로 다시 환산하면 **버린 선언 ours 7,389 → 1단계 −80% = ≤1,478, 5단계 −95% = ≤369**(코퍼스 측정 20곳 합계로는 14,112 → ≤2,822 / ≤706 — K4 수정으로 daum·hani·bbc 가 들어와 0b 의 17곳 13,251 에서 늘었다). browser-too(5곳 15, 17곳 236)는 목표 밖.

단계를 마칠 때마다 이 표와 [compat-survey.md](compat-survey.md) 의 사이트별 수치를 갱신한다. 수치는 `scripts/compat-corpus.sh run` 의 `corpus-report.tsv` 합계(코퍼스 전체)와, 비교용으로 2차 5곳 합계를 따로.

| 단계 | 상태 | 커밋 | 테스트 수 | 코퍼스 fail/panic/timeout | 버린 선언 (5곳) | 버린 규칙 (5곳) | sr-only 슬리버 | 에이전트 컨트롤 누락 사이트 | 메모 |
|---|---|---|---|---|---|---|---|---|---|
| 2차 기준 | ✅ | `78bedff`·`4588aba` | 804 | (5곳) 0/0/0 | 7,593 (ours/browser-too 분리는 0단계에서) | 1,931 (같음) | 19 (GitHub 17·위키 2; naver "검색" 은 2글자라 probe 기준 밖) | naver(모듈) | 출발점 |
| 0 견고성 | ✅ 0a·0b·K4·0c 완료 (K5 → 4차) | `d6801bb`(0a) · `15775d6`(0b) · `0e0ccc9`(K4) · 0c 미커밋 | 806 → 829 (0a) → 842 (0b) → 843 (K4) → 862 (0c) → 869 (R3) → 872 (R2) → 875 (R4) → 876 (R1) | **기준선(25곳, K4 뒤)**: ok 19 + ok* 1 · nav-fail 1(crates.io K5) · panic 0 · **timeout 0**(0b 때 3 — daum·hani·bbc, K4 로 해결) · blocked 2 · robots skip 2 · `[mb-budget]` 0. 목표 0/0/0 중 nav-fail 1 남음 → K5 | 5곳 **ours 7,389 / browser-too 15** (20곳 14,112 / 256; 0b 17곳 13,251 / 236) | 5곳 **ours 1,745 / browser-too 11** (20곳 10,451 / 58; 0b 17곳 10,114 / 53) | 5곳 17 (20곳 297; 0b 17곳 249) → 0c 5곳 **10** (20곳 275) | 8/20: naver·daum·hani·korea-kr·velog·d2·google·enwiki | **0a**: Robustness Rules(AGENTS.md) + `[mb-budget]` 워치독 · 검토 표 [부록 A](#부록-a--04-표적-코드-검토-표-0a)(grep 140곳: 안전 117·수정 10·보류 2·거짓 양성 11 + grep 밖 발견 수정 5 — 패닉 4: 멀티바이트 `#색`·`bgcolor`·`@font-face`, colspan 오버플로 / 할당 폭주 1: 거대 grid 줄) · 리소스 수집 `Result` 제거 · 스크립트 에러 위치(콘솔) · `compat3` 골든 + `compat3-menu` eval(오늘은 실패가 정상). **0b**: 코퍼스 도구(`compat-corpus.sh`·`compat-sites.tsv`·probe `summary.tsv`)·조사 캐시 GET 전용·`css/properties.rs`·버림 사유 코드·[분류표](#0b-분류표-동결--바꾸려면-이-표를-먼저-고친다) 동결 · 녹화 2026-11-26(캐시 동결) · 발견 수정 K1 settle·K2 빈 오류 페이지·K3 `queueMicrotask` · **K4**(`0e0ccc9`): `queueMicrotask` 가 `then` 의 species 를 통해 페이지 폴리필 Promise 를 만들던 것 — daum·hani·bbc timeout → ok(~250ms), 나머지 22곳 수치·요소 목록 변화 없음(`diff`) ([부록 B](#부록-b--0b-코퍼스에서-찾은-것-알려진-문제--담당)) · 사이트별 표 [survey](compat-survey.md#3차-코퍼스-기준선-0b). **0c**: 구조적 퍼저 + 축소기 + CSS 토큰·이미지 바이트 퍼징 + 깊이 10,000 케이스([0.3 구현 메모](#03-퍼징)) · 긴 실행(release) 레이아웃·CSS·이미지 각 15만 무결 · 발견 수정 11건([부록 C](#부록-c--0c-퍼저에서-찾은-것)): 비유한 좌표(길이 포화 ±33,554,431 + 레이아웃 출력 포화), 증폭 4(음수 shrink-to-fit 합, 인라인 상자가 자기 폭을 % 기준으로 넘김, 음수 flex 인수, flex 탐색이 1e6 대리 폭을 max-content 로 보고 — 실사이트 flex 내비 균등 분배의 원인), 깊이 abort(파서 평탄화 256), SVG 4(래스터 4096 상한, tiny-skia 이중 패닉 abort, 필터 층 3GB, 중첩 `<svg>` 스택 넘침), 래스터 거대 글리프 · D1·D2·D3 결정([부록 A](#부록-a--04-표적-코드-검토-표-0a)) · 코퍼스 재측정(`0c`, [survey](compat-survey.md#3차-코퍼스-기준선-0b)): 결과·버림 수·요소 목록 25곳 동일, 슬리버 297 → 275, 문서 높이 3곳 변화, timeout·panic·`[mb-budget]` 0. 남음: K5(4차), R5–R7(부록 C; R1–R4 는 후속으로 수정) |
| 1 값 파서 | ✅ P·1r·1a·1p·1q·1b 완료 · 1c 완료(미커밋) — **목표 달성** | P `e470230` · 1r `9de59c9` · 1a `80766d8` · 1p `1ca47b2` · 1q `994d8c8` · 1b `35f1218` · 1c 미커밋 | 915 → 919 (P) → 932 (1r) → 943 (1a) → 955 (1p) → 961 (1q) → 980 (1b) → 996 (1c) | 1a: ok 17 + ok* 3(github·yna 새 이미지 요청 미스) · nav-fail 1 · panic 0 · timeout 0 · `[mb-budget]` 0 | 목표 ≤1,478 (동결, ours 기준) — **전(`step1-before`)**: 5곳 ours 7,371 · 합계(ours + IACVT 2,143 + `Keyword` 폴백 113 + var 폴백 0) **9,627** (20곳 14,078 · 18,204) → **1r(`html-root`)**: ours 그대로 · IACVT 128 · 합계 **7,612** (20곳 IACVT 318 · 합계 15,075) → **1a(`important`)**: ours **4,387** · 합계 **4,645**(20곳 9,643 · 10,680), 사유 `important` 5,097 → **0** → **1b(`value-parsers`)**: ours **3,941** · 합계 **4,213**(20곳 7,123 · 8,086), `multi-token` 4,319 → 1,098 → **1c(`var`)**: ours **335** · 합계 **738** ✅ ≤1,478(20곳 1,806 · 3,037) | — | 5곳 15 · 20곳 325 그대로 | 8/20 → 1a 7/20(enwiki 3/3), 1c 까지 7/20 | [1단계 구현 메모](#1단계-구현-메모-2026-11-27). P = 계산값 시점 열 `iacvt`·`var_keyword_fallback`·`decl_drop_total`(`style_tree_with_report`), 기존 열은 `grid` 와 25곳 동일. **1r** = `<html>` 을 단일 루트로(`:root`·`html{}`·rem·`documentElement`), 루트 여백 비겹침, 캔버스 배경(색), `--*` 공유 스코프(첫 실행 `[mb-budget]` 3 → 0). 요소 diff: github +1(로고 링크, 토큰으로 크기 생김) · enwiki −3(`.client-nojs` 숨김 — `document.cookie` 없어 시작 스크립트가 죽음, 후속)  **1a** = 파싱·두 패스 캐스케이드·인라인 `!important`·분류표 `important` 행 수정. 요소 diff: yna −4·microsoft −2(`display:none!important` 유틸리티, 정상) · docsrs −1(`<noscript>` 시트를 우리가 적용 — 기존 버그, 후속) · enwiki +3(헤더 레이아웃 이상 — 레거시 % 높이 = 부모 폭 기준, 3단계 항목, 진단만; 문서 높이 18,008 → 1,280)  **1p** = % 높이를 확정 포함 블록 높이 기준으로(아니면 auto), 루트 = 뷰포트, absolute % 는 패딩 상자, absolute 자식 블록화, 인라인 블록 높이 근사. 요소 diff: docsrs +15(`<a><code>` 문서 링크, 정상) 뿐. enwiki 헤더 1,292 → 124px, +3 링크 유지. `page_height` 가 `html,body{height:100%}` 4곳에서 800 — (c) `scrollHeight` 권고. 슬리버 319 → 340(블록화된 sr-only, 3단계 몫)  **1q** = `page_height`·스크롤 상한을 `DocumentView::document_height`(넘친 내용 포함) 하나로, 단위 없는 0 = `0px`. 요소 diff: korea-kr −1(썸네일 `height:0` + 버려지는 `calc()` 패딩 — 5단계). d2·enwiki `page_height` 800 → 5,458·12,732. 슬리버 340 → 413(0×0 sr-only, 3단계)  **1b** = border 계열·`font-family`·`box-shadow` 목록·`transition/animation` 파싱만·`flex` 완성(`flex:1` = basis 0%)·`background-size` 페인트·`clip`/`clip-path`·`light-dark()`·`inset`·전역 키워드·`all`(전엔 `inherit` 등을 아예 안 풀었다). 요소 diff: korea-kr +1(1q 썸네일 링크 복귀) · bbc −1(`border:0` 이 UA 테두리를 지우자 0×0 SVG 만 남은 버튼 — 인라인 `<svg>` 고유 크기 갭, 후속). `ok*` 5곳 늘어남(살아난 배경 단축의 이미지).  **1c** = `Unparsed`(커스텀 속성·`var()`), 정의 노드에서 치환 후 상속, 재파싱 메모, IACVT = `unset`, 64KiB 상한; 코퍼스 첫 실행에서 정의된 이름 + 폴백이 전부 실패하던 버그(`parse_nested_block` 은 parse_entirely) 고침. 요소 diff 없음. 리뷰 후 `var()`·함수 중첩 상한 `css::MAX_VAR_NESTING` 32(스택 넘침 abort 방지). [완료 판정](#1단계-완료-판정-2026-11-28-라벨-var) |
| 2 @media/@layer/@supports | 🔶 2.1 완료(미커밋) · 2.2·2.3 ⏳ | 2.1 미커밋(probe·엔진 두 커밋) | 876 → 878 (2.1 probe) → 902 (2.1) | 25곳 그대로: ok 19 + ok* 1 · nav-fail 1(K5) · panic 0 · timeout 0 · `[mb-budget]` 0 | 5곳 ours 7,389 / browser-too 15 (변화 없음 — 2.1 은 버림을 안 바꿈) | 5곳 ours 1,745 / browser-too 11 (변화 없음) | 5곳 10 → **218**(kowiki 0→203), 20곳 275 → **984**(enwiki 66→552) — 아래 메모 | 8/20 그대로(naver·daum·hani·korea-kr·velog·d2·google·enwiki) | **2.1**: 코퍼스 `media-before`(probe 만 바꾼 옛 엔진) → `media-after`. 20곳 `@media` 블록 3,786 → 3,789(`<link media>` 3개가 이제 블록) · 블록 안 규칙 15,417 → 16,220 · 적용 15,417 → **5,754** · "wrong at 1280x800" 범주 규칙 적용 9,223 → **1**(GitHub `print,screen and (width<=1349px) and (width>=768px)` — probe 휴리스틱 오분류, 1280 에서 참이 맞음) · 무효 쿼리 0. 결과·버림·오프라인 미스·기대 컨트롤은 25곳 동일. **요소 목록 diff 10곳** — 모바일 전용이 사라짐(정상): yna −186(메뉴 열기/닫기 ×29 + 모바일 서랍 메뉴 링크·APP설치·광고삭제·뉴스레터), apple −11(모바일 푸터 아코디언 버튼), d2 −2(모바일 검색·메뉴 링크), docsrs −1(모바일 "Crate tokio"), github −1("View all files"). 데스크톱 전용이 나타남(정상): bbc +69(섹션 내비·월드 서비스 언어), yna +20(캐러셀 버튼), github +18(파일 목록·Go to file·Branches/Tags), apple +17(글로벌 내비·검색·장바구니), microsoft +10(상단 내비), hani +6(섹션 링크), d2 +4(검색창·D2 News·Hello world), kowiki +14(전거 통제·바닥글 링크). **버그 → 5단계 몫**: enwiki −494 +240 — Vector 2022 데스크톱 격자(`@media screen and (min-width:1120px)` 의 `.mw-body{display:grid; grid-template: … / minmax(0,59.25rem) min-content; grid-template-areas: …}`)가 이제 적용되는데 `grid-template` 단축·`minmax()` 를 못 읽어 본문 열이 0폭 → navbox 링크 494개가 0폭(관찰에서 빠짐), 슬리버 enwiki 66→552·kowiki 0→203(같은 원인, kowiki 는 링크가 남음). 나타난 240 은 참고문헌 역링크·"145 languages" 버튼(Chrome 데스크톱에도 있음). 기대 컨트롤 enwiki 2/3 그대로. 미리보기 `compat-cache/previews/2.1/`(후만 — 전은 옛 빌드가 필요해 생략): HN 데스크톱 표(문서 높이 1,377 → 1,020, 모바일 규칙 빠짐), GitHub 위쪽 어두운 띠 = 로그아웃 헤더 `div[data-color-mode=dark]`(Chrome 에서도 어두움)가 레이아웃 갭으로 늘어난 것 — `@media` 와 무관, "GitHub 라이트" 판정은 2.2 때. **비트셋 비용**: GitHub `@media` 517블록(쿼리 534) **3.6µs/프레임**, 코퍼스 최대 yna 1,571블록 17µs(release) — 매 프레임 키 확인에 그대로 씀. 퍼저 CSS 긴 실행 10만 무결. 남음: 2.2 `@layer`(GitHub 의 레이어 안 다크 블록 49개는 2.1 평가가 있어 펼쳐도 꺼짐)·2.3 `@supports` — 1단계 뒤 |
| 2.1a 위키 그리드 | 🔶 완료(미커밋) | 미커밋 | 902 → **915** | 25곳 그대로(ok 19 + ok* 1 · nav-fail 1 · panic 0 · timeout 0 · `[mb-budget]` 0) | 5곳 ours 7,389 → **7,371** / 15 (20곳 14,112 → 14,078 — `grid-template`·`minmax` 가 이제 읽힘: github 17·enwiki 6·mdn 6·docsrs 2·bbc 2·google 1) | 그대로 | 5곳 218 → **15**, 20곳 984 → **325**(enwiki 552→71 · kowiki 203→0 · **mdn 0→25**) | 8/20 그대로 | [2.1a 메모](#21a-위키-그리드-회귀-수정-2026-11-27). 요소 diff(`media-after` → `grid`) 2곳뿐: enwiki **+484**·kowiki **+230** = navbox 링크(요소 678→1,162 · 767→997). 2.1 전(`media-before`) 대비로는 enwiki +241 −11(+240 은 2.1 의 참고문헌 역링크·언어 버튼), kowiki +244(2.1 의 +14 + navbox 230 — kowiki navbox 는 **2.1 전에도** colspan 표 버그로 빠져 있었고 이번에 처음 보임). 2.1 전 대비 enwiki −11 은 navbox 그룹 셀(`width:1%`) — 표 min-content 바닥 없음, 후속. mdn 슬리버는 사용자 속성 트랙·calc 곱셈 미디어 쿼리(1·5단계 몫), 요소 변화 없음. 문서 높이: enwiki 35,445→16,790 · kowiki 16,345→11,983 · hani 11,898→10,426 · mdn 5,791→6,755 · bbc 25,226→26,042 · docsrs 5,947→6,029 · apple 26,569→26,617(간격·영역 폭 배치·내용 크기 `fr` 행; 요소 목록 동일) |
| 3 클리핑 | ⏳ | | | 0/0/0 | — | — | 목표 0 | | |
| 4 셀렉터 | ⏳ | | | 0/0/0 | — | 목표: 대상 범주 0 | — | | |
| 5 calc/트랙 | ⏳ | | | 0/0/0 | 목표 ours −95% (2차 환산 ≤380) | — | — | | 위키 레이아웃, taffy 스파이크 결과 |

"에이전트 컨트롤 누락 사이트" = [compat-survey 에이전트 관점 표](compat-survey.md#에이전트-관점-요약) 처럼 사이트마다 사람이 기대하는 주요 컨트롤(검색창·검색 버튼·주 내비·본문 링크)을 관찰이 찾는지. 코퍼스가 커지면 사이트별 기대 컨트롤 목록을 `compat-cache/sites.tsv` 옆 열로 적고 probe 가 이름 부분 일치로 자동 판정한다(목록은 사람이 처음 한 번 작성).

미리보기 전후 PNG 는 `compat-cache/previews/<단계>/<사이트>.png` 에 남긴다(git-ignored). 문서에는 관찰한 변화를 글로만 적는다.

---

## 부록 A — 0.4 표적 코드 검토 표 (0a)

2026-11-26, `802a836` 기준. [0.4](#04-표적-코드-검토) 의 grep 을 그대로 돌린 **140곳**(테스트 모듈 제외) + 같은 경로에서 grep 이 못 잡는 모양(문자열 바이트 슬라이스, 페이지가 주는 개수로 할당)을 따로 찾은 것. 분류: **안전**(불변식이 코드로 보장 — 필요한 곳엔 주석 추가) · **수정**(테스트 먼저, 고침) · **보류**(이유) · **거짓 양성**(타입 표기·주석·슬라이스 패턴).

| 분류 | 곳 |
|---|---:|
| 안전 | 117 |
| 수정 | 10 (+ grep 밖 5) |
| 보류 | 2 (+ grep 밖 3 — D1~D3, 0c 에서 결정: D1·D3 수정, D2 퍼징 후 SVG 3건 수정) |
| 거짓 양성 | 11 |

### grep 결과

| 파일 | 줄(`802a836`) | 분류 | 근거 / 조치 |
|---|---|---|---|
| `layout/grid.rs` | 119–205 (`boxes[box_idx]`, `natural_*[area.*]`, 슬라이스) 11곳 | 안전 | `box_idx` 는 push 와 함께 기록. `col_start < n_cols == columns.len()`: `place_grid_item` 이 시작을 `n_cols - span` 으로 자르고 템플릿 영역은 `effective_column_tracks` 가 트랙을 넓힘(2차 C2). `row_start < row_end <= n_rows`, `resolve_grid_rows` 는 `n_rows` 개를 돌려줌 → 불변식 주석 추가 |
| `layout/grid.rs` | 343·356 (`cells[r][c]`) | 안전 | `r < len && c < n_cols` 검사 / `ensure_rows` 후 `c < n_cols` |
| `layout/grid.rs` | 462–495 (`sizes[i]`) 6곳 | 안전 | `i` 는 `natural_row_heights` enumerate·`take(n_rows)` |
| `layout/grid.rs` | 515 (`vec![content_width]`) | 거짓 양성 | 배열 리터럴 |
| `layout/table.rs` | 153–154 (`col_widths[col_idx]`) | 안전 | `span == 1` 이면 `col_idx < column_count`(span = min(colspan, count − idx)) → 주석 추가 |
| `layout/table.rs` | 193–234 (`column_x_starts[…]`, `col_widths[j]`) 6곳 | 안전 | 길이 `column_count + 1`, `col_idx + span <= column_count` |
| `layout/inline.rs` | 81·90·311·322 | 안전 | `lines`/`line_widths` 같이 push, `child_idx` 는 enumerate, `i < len` 검사 |
| `layout/flex.rs` | 94–207 13곳 (`boxes[i]`) | 안전 | `i` 는 `in_flow` 에 push 직전 `boxes.len()` |
| `layout/block.rs` | 105·111 | 안전 | `group_block_children` 의 인덱스·범위는 `node.children` 안 |
| `layout/taffy_bridge.rs` | 475·486·504 (`?` on Option) | 안전 | taffy 경로 실패 → `None` → 레거시 레이아웃 폴백 |
| `layout/taffy_bridge.rs` | 597·605·773 (`node.children[…]`) | 안전 | `native_child_slots(node)` 가 같은 노드에서 만든 인덱스 |
| `layout/taffy_bridge.rs` | 735·800·805 (`expect`) | 안전 | 같은 트리에서 방금 만든 id; boundary 판정은 build/walk 가 같은 `is_native_block` |
| `render/display_list.rs` | 430·452·472·482·518·605·636·688 (`?`) | 안전 | 그 상자의 배경·이미지·값을 안 그릴 뿐 |
| `render/display_list.rs` | 549–578 (그라디언트 `positions[…]`, `unwrap`) 6곳 | 안전 | `n == 0` 은 앞에서 반환, `positions[0]` 를 먼저 채우고 `last_known` 은 Some 인 곳으로만 이동 → 주석 추가 |
| `style/mod.rs` | 51·52·53·67·134 (`&[usize]`) | 거짓 양성 | 타입 표기 |
| `style/mod.rs` | 137·138 (`?`) | 안전 | 상호작용 경로가 문서에 없으면 hover/focus 없음 |
| `style/mod.rs` | 271 (`expect`) | 안전 | arena 불변식: children 목록엔 살아 있는 id 만(`remove_child`·`replace_with_text`·innerHTML·`replace_child` 모두 떼고 나서 묘비) → 주석 추가 |
| `style/defaults.rs` | 430 (`[hidden]`) | 거짓 양성 | 주석 |
| `html.rs` | 66 (`parse_fragment(…)?`) | 안전 | 문서 자체 파싱 — 규칙 1 이 허용한 실패(html5ever 는 사실상 실패 안 함) |
| `css/mod.rs`·`parse.rs`·`transform.rs`·`shadow.rs`(5)·`shorthand.rs`(310)·`css/grid.rs`·`gradient.rs` | 39곳 (`?`) | 안전 | 파서 안의 `?` = 그 선언 하나 버림. 패닉 아님. 버림 자체는 1단계 범위 |
| `css/shadow.rs` 3·4, `css/shorthand.rs` 42·136 | 4곳 | 거짓 양성 | 주석의 `[blur]`, 슬라이스 패턴 `[v]` |
| `mb-runtime/src/resource.rs` | 16·38·71·197·207·239·269·306 (`?` on `ResourceError`) | **수정** | 실제로는 이미 실패할 일이 없는 `?`(2차 `resolve_attr`) — 함수 시그니처에서 `Result` 를 빼서(`load_stylesheets`/`load_scripts`/`load_images`/`stylesheet_urls`/`image_urls`/`collect_*`) 다시 생기지 않게. 기존 resource 테스트 green |
| `mb-runtime/src/resource.rs` | 79·151·152·166 (`?` on Option) | 안전 | 이미지 하나·파비콘 없음으로 끝남 |
| `mb-runtime/src/navigation.rs` | 187·201 (+ grep 이 못 잡은 194, 줄 끝 `?` 뒤 `.into_iter()`) | **수정** | 위 시그니처 변경으로 `map_err(describe_resource_error)?` 제거 — 하위 리소스가 이동을 실패시킬 경로가 타입에서 사라짐 |
| `mb-runtime/src/navigation.rs` | 183 (`html parse error`) | 안전 | 문서 응답 자체(규칙 1 허용) |
| `mb-runtime/src/navigation.rs` | 162·181 (`InvalidBodyEncoding`) | 보류 | 규칙 1 이 허용한 "문서 응답 인코딩" 실패지만, 지금은 `String::from_utf8` 라 **비 UTF-8 문서(EUC-KR 국내 사이트)·잘못된 바이트 하나**에도 이동 전체가 실패한다. 인코딩 감지·변환(`encoding_rs`, `<meta charset>`)은 기능 작업 — 0b 코퍼스에서 실제로 걸리는 사이트가 나오면 우선순위를 정한다 |

### grep 밖에서 찾은 것

grep 패턴은 `[ident]`·`[ident ± 1]` 만 잡아서 `&s[0..2]` 같은 **문자열 바이트 슬라이스**와 **페이지가 주는 개수로 하는 할당**을 놓친다. 같은 경로에 `grep -aE '\[[a-z_0-9.]* *\.\.|\[\.\.[a-z0-9_ +-]+\]|as_bytes\(\)\['` 와 `parse::<usize>`/`vec![…; n]` 을 추가로 돌렸다.

| # | 곳 | 분류 | 증상 → 조치 (테스트) |
|---|---|---|---|
| E1 | `css/parse.rs` `parse_hex_color_str` | **수정** (패닉) | `#` 토큰 값은 식별자라 멀티바이트 가능. `color:#é1`(3바이트, 2글자) → `chars[2]` 인덱스 초과, `color:#한글`(6바이트) → 글자 중간 슬라이스. ASCII 아니면 길이 0 취급 → 선언 버림 (`non_ascii_hash_color_drops_the_declaration_not_a_panic`) |
| E2 | `style/presentational.rs` `parse_hex_color_body` | **수정** (패닉) | `bgcolor="한글"` 같은 속성이 스타일 패스 전체를 패닉. `is_ascii()` 먼저 (`non_ascii_color_attribute_is_ignored_not_a_panic`) |
| E3 | `mb-runtime/src/resource.rs` `extract_font_urls`/`extract_url_value` | **수정** (패닉, 이동 중) | 닫히지 않은 `@font-face{` 나 `url("a")` 뒤에 `)` 가 없을 때 ASCII 구분자 **한 바이트 뒤**에서 재개 → 다음 글자가 멀티바이트면 슬라이스 패닉. 닫히지 않으면 중단, 따옴표 뒤에서 재개 (`font_face_scan_survives_multibyte_text_after_a_brace_or_quote`) |
| E4 | `layout/table.rs` `cell_colspan` | **수정** (패닉) | 상한 없음 → `colspan="18446744073709551615"` 에서 열 수 합 오버플로(debug 패닉)·`vec![0.0; usize::MAX]`. HTML 규칙대로 1000 으로 자름 (`absurd_colspan_is_clamped_like_html_says`) |
| E5 | `layout/grid.rs` `axis_hint_from` | **수정** (할당 폭주) | `grid-row: 2000000000` / `span 1000000000` → 점유 맵·행 표가 줄 수만큼 할당(수 GB → abort, `catch_unwind` 로도 못 잡음). Blink 처럼 줄 1000 상한 (`huge_grid_lines_and_spans_are_clamped`, `grid_item_on_an_absurd_line_number_lays_out_bounded`) |
| D1 | 스타일·레이아웃·리소스 수집·접근성 이름의 **재귀** 전반 | **0c 결정: 수정**(파서 평탄화, 상한 **256**) | 깊이 10,000 은 `html::parse` 부터 넘쳤다(abort). 실측 한계(중첩 `div`, 파이프라인 전체; agent `observe` 를 더해도 같음 — 레이아웃이 가장 깊게 먹음): debug 2MB ~132 · debug 8MB ~537 · release 2MB ~976 · release 8MB ~3,910 (파싱만: debug 2MB ~2,982 / release 2MB ~6,264, 스타일까지: ~537 / ~2,084). Blink/WebKit 의 512 를 그대로 쓰면 debug 8MB(브라우저 메인 스레드)에서 여유가 5% 뿐이라 **256** — release 2MB 3.8배, debug 8MB 2.1배. `html.rs` 의 `build_into_arena` 를 반복문으로 바꾸고 상한 깊이 요소의 자식은 그 요소의 부모에 이어 붙인다(Blink 처럼 형제로, 문서 순서 유지, 잃는 노드 없음; `parse_fragment`(innerHTML)도 같은 함수). rcdom 은 원래 반복으로 drop 해서 10만 단계도 안전. 테스트 `deeply_nested_source_is_flattened_at_the_depth_cap`·`fragment_parsing_has_the_same_depth_cap`·퍼저 `ten_thousand_levels_of_nesting_do_not_overflow_the_stack`(자식 프로세스, 8MB). **안 다룬 것**: ~~스크립트가 만든 깊이~~ → 후속 [R3](#부록-c--0c-퍼저에서-찾은-것) 에서 트리 변경 경계로 강제, debug 2MB 스레드(테스트 스레드)에서는 ~132 단계부터 넘침(테스트용 깊은 페이지는 8MB 스레드로) · 시간은 [부록 C](#부록-c--0c-퍼저에서-찾은-것) R1 |
| D2 | `render/raster/`, `mb-dom/src/resource.rs` `decode_image` | **0c 결정: 퍼징함**(구조적·저비용) | `image_bytes_fuzz_*` ([0.3 구현 메모](#03-퍼징)) — 래스터 디코더(`image` 0.25, 할당 한도 512MB, 상류 퍼징)는 그대로 두고 **크기를 주장하는 헤더 → 할당·시간** 글루를 본다. 찾은 것 4건 모두 SVG 경로([부록 C](#부록-c--0c-퍼저에서-찾은-것) F6–F8, F10): 픽스맵 크기 무제한(abort), tiny-skia 이중 패닉(abort), 필터 층 3GB, 중첩 `<svg>` 스택 넘침(abort). 래스터 형식은 0건. 남은 것: 래스터 이미지는 `image` 한도 안에서 디코드한 뒤 `Vec<u32>` 로 한 벌 더 복사(최대 ~1GB 순간 사용) — R4 |
| D3 | 레이아웃의 f32 나눗셈 | **0c: 퍼저 NaN 판정이 담당 → 수정** | 나눗셈보다 **넘침**이 원인이었다: `1e39px`(f32 → inf), `3e38px` 두 개의 합, `font-size: 1000%` 40단계, `width: 1000%` 중첩. Blink `LayoutUnit` 범위로 포화 — 캐스케이드가 길이·%·수·트랙·글자 크기를 ±33,554,431 로(`style::MAX_CSS_LENGTH`), `layout_tree` 가 결과 좌표를 한 번 더(NaN → 0) ([부록 C](#부록-c--0c-퍼저에서-찾은-것) F1). 페인트 전용 값(그림자·변환)은 포화하지 않음 — 래스터가 견디는 것을 확인(R5) |

---

## 부록 B — 0b 코퍼스에서 찾은 것 (알려진 문제 → 담당)

2026-11-26 녹화 · 2026-11-27 기준선(`compat-cache/corpus/baseline-0b/`; K4 수정 뒤 `baseline-k4/`). 사이트별 수치는 [compat-survey.md 3차 코퍼스 기준선](compat-survey.md#3차-코퍼스-기준선-0b).

**0b 에서 고친 것** (각각 손으로 쓴 재현 → 실패 테스트 → 수정):

| # | 증상 (사이트) | 원인 | 수정 · 테스트 |
|---|---|---|---|
| K1 | 로드가 ~1.5초보다 느린 페이지가 **로드 전 빈 화면**으로 관찰됨 — `url: (none)`, 요소 0, `settled: false` (녹화 중 daum·yna) | `AgentBrowser::settle` 이 이동 대기 프레임도 `MAX_SETTLE_FRAMES`(600 × ~2.5ms)에 셌다. 실제 MCP 세션의 느린 사이트도 같음 | 이동 대기 프레임은 프레임 상한에 안 셈(벽시계 `settle_timeout` 만 자름) · `settle_waits_for_a_slow_document_load_within_the_wall_clock_budget`(2.5초 지연 라우트) |
| K2 | 문서 로드가 실패하면 에이전트가 **이유를 못 봄** — 오류 페이지가 비어 `url: (none)`·요소 0·글자 0 (crates.io 404, stackoverflow·w3c 403) | `error_document`/`text_document` 가 `<div>…</div>` 앞뒤 공백을 남겨 조각 파싱의 마지막 루트(=그려지는 유일한 것)가 공백 텍스트 (AGENTS.md "테스트 HTML 과 루트" 함정 그대로) | HTML 을 `trim` · `failed_load_tells_the_agent_why`(404 → 관찰에 `load failed`·`http status 404`), `canned_documents_parse_to_a_single_root`. **`text/plain` 문서도 같은 이유로 빈 화면이었다** |
| K3 | core-js 의 Promise 폴리필 안에서 스택 소진 (hani·bbc·daum, 임시 인터럽트로 위치 확인) | `queueMicrotask` 심이 호출 때마다 전역 `Promise` 를 찾음 → 페이지가 `Promise` 를 폴리필로 바꾸고 그 반응을 `queueMicrotask` 로 예약하면 서로를 끝없이 부름 | 엔진 `Promise` 를 부트스트랩 때 한 번 묶음 · `queue_microtask_survives_a_page_replacing_promise` |
| K4 | **hani·bbc·daum `timeout`**(120초) — K3 뒤에도 프레임마다 마이크로태스크가 끝없이(드레인 한 번에 30만 개+) | 임시 계측(되돌림 — 드레인 5만 번째마다 인터럽트를 걸고 잡힌 예외의 스택을 찍음)으로 세 곳 모두 같은 고리: core-js 의 flush/notify → **우리 `queueMicrotask`**(부트스트랩 `then.call(resolved, cb)`) → 네이티브 `then` → **폴리필의 `Promise` 생성자**. `then` 은 결과 프로미스를 SpeciesConstructor(`resolved.constructor`)로 만드는데, core-js 는 Promise 를 바꿀 때 네이티브 프로토타입의 `constructor` 를 지우고 프로토타입을 폴리필 쪽으로 다시 잇는다(`setSpecies` 도 함). 그래서 호출마다 폴리필 프로미스가 생기고, 잡이 돌며 그걸 resolve → 폴리필 notify → 다시 `queueMicrotask` → … 한 잡이 다음 잡 하나를 낳는 무한 사슬. Chrome 의 `queueMicrotask` 는 프로미스를 만들지 않아(`constructor` 를 안 읽음) 이 고리가 없다 | 비공개 `resolved` 에 자기 `constructor: undefined` 를 둬서 SpeciesConstructor 가 렐름의 내장 %Promise% 를 쓰게(페이지가 못 바꿈) · `queue_microtask_never_builds_a_page_promise_through_species`(core-js 의 그 세 동작만 흉내 낸 손 재현 — 수정 전 폴리필 생성 1001회(테스트의 상한), 후 0). 세 곳 timeout → ok(daum 277·hani 220·bbc 266ms). **안전망(드레인당 마이크로태스크 상한)은 넣지 않음** — 원인 수정만으로 세 곳 다 끝나고, Chrome 도 상한이 없어 상한은 정상 페이지의 의미를 바꿀 수 있다. 같은 종류가 또 나오면 그때 `[mb-budget]` 식 보고와 함께 검토 |

**남은 것**:

| # | 증상 | 확인한 것 | 담당 |
|---|---|---|---|
| K5 | crates.io `nav-fail`: 문서 요청에 **404·빈 본문·Content-Type 없음** | 엔진의 문서 요청에 `Accept` 헤더가 없다. crates.io 는 HTML 을 받는다고 밝힌 요청에만 앱 껍데기를 주는 것으로 보인다(**추정** — 확인하려면 재방문이 필요해 안 함) | 4차(네트워크) — 고치면 캐시 정체성에 헤더가 없으므로 보충 녹화로도 안 바뀐다. 그 사이트만 캐시 항목을 지우고 다시 녹화해야 함(재방문 = 사용자 결정) |
| K6 | stackoverflow·w3c `blocked`(403 챌린지), robots.txt 도 418/403 | 우회하지 않음(규칙) | 없음 — 표에 사유만 |
| K7 | JS `fetch('/path')`·XHR 상대 URL 이 실패(`invalid URL`) | `js/fetch.rs` 가 `String(url)` 을 기준 URL 없이 `Url::parse` | 4차(JS 소소) |
| K8 | 11st·old.reddit 는 robots.txt 가 `*` 에 `Disallow: /` | 녹화 안 함 | 없음 — 녹화 23곳, 그중 측정된(ok/ok*) 20곳(K4 전 17곳) |
| K9 | daum 이 `ok` 인데 **요소 0·텍스트 0** (K4 뒤 처음 보임) | 서버 HTML 의 `<body>` 는 정적 마크업이 317바이트(링크 3개)뿐인 껍데기 — 화면 전체를 자기 `index.js` 가 그리는데 그 앱이 빠진 DOM API(`createTreeWalker`, WebSocket `removeEventListener`, `customElements`)에서 죽는다. naver 의 ES 모듈과 같은 "JS 가 그리는 본문" 부류 | 4차(JS API 표면) — CSS 단계와 무관. daum 의 CSS 수치(선언 471/20 등)는 시트 파싱이라 유효 |

---

## 부록 C — 0c 퍼저에서 찾은 것

2026-11-27, `0e0ccc9` 기준. 각각 퍼저(또는 퍼저 결과를 따라간 손 탐색) → 손으로 줄인 재현 → **실패하는 단위 테스트**(증상 이름, 해당 모듈) → 수정. 퍼저 시드는 테스트로 쓰지 않았다.

| # | 증상 | 원인 | 수정 · 테스트 |
|---|---|---|---|
| F1 | 레이아웃 좌표 `inf`/`NaN` — `padding: 1e39px`, `margin: 1e30% …`, `height: 3e38px` + 여백, `font-size: 1e39px` 인 글자 상자 | cssparser 는 f32 라 `1e39` 가 `inf`, 유한한 큰 값도 합치면 넘침, %·글자 크기는 단계마다 곱해짐. 이후 `inf - inf` = NaN. `document_height`(버퍼 크기)·적중 판정·에이전트 rect 로 퍼짐 | 캐스케이드: 계산된 `Length`·`Number`·트랙·글자 크기를 ±`MAX_CSS_LENGTH`(33,554,431 = Blink `LayoutUnit` 최대)로 포화(`style/mod.rs` `saturate_value`). 레이아웃: `layout_tree` 끝에서 좌표 전체를 한 번 포화(`saturate_layout`, 반복문, NaN → 0) — 정상 CSS 도 % 중첩으로 넘칠 수 있어 값 하나씩 자르는 것만으론 안 됨. `overflowing_length_literal_resolves_to_a_finite_px`, `compounding_font_size_stays_finite`, `nested_percent_widths_saturate_instead_of_overflowing_to_inf` |
| F2 | 유계 입력에서 폭 −1e7px(→ 포화 3.3e7): `<span>` 안 `<table style="margin: auto -711px">` 열 단계 | 인라인 상자·inline-block 의 내용 폭 = 자식 바깥 폭의 **합**인데 음수 여백이면 음수. 그런 자식이 여럿인 감싸개는 단계마다 배로 | 내용 폭·shrink-to-fit 폭을 0 이상으로(`inline.rs` `inline_content_size`·`inline_block_shrink_to_fit_width`). `shrink_to_fit_width_is_never_negative_around_negative_margins` |
| F3 | 폭 1.8e7px: 인라인 상자 안 블록·표에 `padding-left: 67%` | `layout_inline_node` 가 **자기** 내용 폭(자식 합으로 커진 값)을 자식의 % 기준·줄 폭으로 넘김 → 단계마다 커진 기준에 다시 %. CSS 2.1 §10.1: 인라인 상자는 포함 블록이 아니다 | `layout_inline_sequence_no_wrap` 에 `containing_width`(가장 가까운 블록 컨테이너 폭)를 따로 — 정렬 여유만 자기 폭으로. `percent_padding_inside_an_inline_box_resolves_against_the_block_container`. **실사이트 배치가 바뀔 수 있는 유일한 수정**(인라인 안 % 여백·패딩) — 코퍼스 재측정 참고 |
| F4 | 폭 1.7e7px: `flex-shrink: -1` | 음수 flex 인수를 받아들여 넘치는 항목이 오히려 커짐. Flexbox 명세: 음수는 무효 | 파서가 `flex-grow`/`flex-shrink` 음수와 `flex` 단축형 음수를 선언째 버림(Chrome 처럼 — 앞선 유효 선언이 남음). `negative_flex_factors_are_invalid_and_dropped`. probe 분류는 `invalid-value`(ours) 로 잡힌다 — Chrome 도 버리니 엄밀히는 browser-too 지만 0b 분류표가 동결이라 그대로(빈도 미미) |
| F5 | 깊이 10,000 중첩 → 스택 넘침 abort | [D1](#부록-a--04-표적-코드-검토-표-0a) | 파서 평탄화 256 (D1 행) |
| F6 | `<svg width="1e9" height="1e9"/>` 몇 바이트로 ~4e18 바이트 할당 → abort | `decode_svg` 가 SVG 의 width/height 로 픽스맵을 만듦. tiny-skia 는 행 폭만 제한, `image` 의 512MB 한도는 이 경로에 없음 | 긴 변 `MAX_SVG_RASTER_SIDE` 4096 으로 균등 축소 렌더(작은 SVG 는 전과 같은 크기 — `ceil`). 레이아웃은 이미지 자체 크기를 안 읽고(기본 200×150·속성) 페인트는 상자에 맞춰 늘리므로 화면 변화 없음. `huge_svg_is_rasterized_within_the_size_cap` |
| F7 | SVG `scale(1e30)`·`r="1e30"` → tiny-skia 패닉(범위 밖 슬라이스·`None` unwrap, release 도). 일부는 **이중 패닉 abort** | tiny-skia 0.12 의 경로 채우기가 f32 범위만 검사하고 고정소수점 스캔 변환은 ~1e9 부터 깨짐(실측: 1e8 정상, 1e9–1e10 실패). 첫 패닉이 풀리는 중에 `SuperBlitter` 의 `Drop` 이 flush 하다 또 패닉 → abort. **상류 버그** | 렌더 전에 루트 그룹의 stroke/layer 상자가 ±1e6px(축소 후) 안인지 확인 → 밖이면 디코드 실패. 그래도 풀리는 패닉은 `catch_unwind` 로 디코드 실패(두 번째 방어선). 참고: 페이지 이미지는 `mb-runtime` `parallel_fetch` 의 스코프 스레드에서 디코드되고 `join().ok()` 가 패닉 난 스레드를 버리므로 **풀리는** 패닉은 원래도 이미지 하나 실패로 끝났다 — 실제 구멍은 abort(이중 패닉) 쪽이었다(파비콘은 같은 스레드가 아님). `svg_with_absurd_geometry_fails_to_decode_instead_of_panicking`(debug 는 `debug_assert` 가 먼저 터져 깔끔히 풀리므로 "렌더 전에 거절됐다" 를 단언) |
| F8 | `viewBox="0 0 1 1"` 4096² SVG 의 흐림 필터 → 3GB·13초 | resvg 는 격리 그룹(필터·불투명도·마스크·클립) 층을 층 상자로 만들되 캔버스 5배×5배(`max_filter_bbox`)까지만 자름 — 4096² 면 층 하나 ~1.7GB, 중첩되면 동시에 | 격리 층 넓이 합의 상한(층마다 min(층 상자, 25×래스터))이 `MAX_SVG_LAYER_PIXELS`(래스터 2장 ≈ 33.5M px, ~130MB, 실측 ~0.3초) 넘으면 디코드 실패. 평범한 큰 도형(격리 아님)은 그대로 렌더. 같은 테스트 |
| F9 | 래스터: `font-size: 3e7px; white-space: nowrap` → 폰트가 깔리면 swash 가 3e7px 글리프 비트맵을 그림(멈춤·수 GB). 폰트 없는 비트맵 대체 경로는 `column * scale` 넘침, `text-shadow: 1e30px …` 위치에서 `i32` 덧셈 넘침(debug 패닉) | 래스터는 0.3 파이프라인 밖 — F1 의 포화 상한(3.3e7)과 크기만 바꾸는 변환이 글자 크기에 구워지는 것을 따라가 손으로 찾음 | 장치 px 글자 크기 2048 초과(또는 비유한)면 그 텍스트를 그리지 않음(`MAX_RASTER_FONT_SIZE` — 어떤 화면보다 큼), 비트맵 대체 경로는 f32 로 계산. `absurd_font_size_text_is_skipped_by_the_rasterizer`. 이후 CSS 토큰 퍼징에 래스터를 붙임 |

| F10 | SVG 안 `<svg>` 1,000 단계 중첩 → 2MB 스택(이미지 로더 스레드) 넘침 abort, ~630 단계부터(`<g>` 는 usvg 자체 상한 1,024 까지 견딤) | usvg 변환기·resvg 렌더가 요소마다 재귀, 중첩 뷰포트 한 단계가 스택을 많이 먹음. D2 탐색 중 손으로 찾음(이미지 퍼저 템플릿엔 깊이가 없었음) | XML 을 먼저 roxmltree(usvg 가 re-export — 새 의존성 아님, 반복 파서)로 파싱해 요소 깊이를 반복문으로 재고 256(DOM 상한과 같음) 넘으면 디코드 실패, 아니면 그 문서로 `Tree::from_xmltree`(두 번 파싱 안 함). `deeply_nested_svg_is_refused_not_a_stack_overflow`(2MB 스레드에서) |
| F11 | 유계 입력에서 폭 1.1e7px(실제 폰트 경로 5만 케이스에서): `div { display: flex; flex-shrink: 0 }` 를 `<div><i></i>…</div>` 네 단계 | 인라인 자식만 가진 요소는 legacy 경계 잎이고, flex 항목 크기 탐색(MaxContent)에서 1e6px 대리 폭으로 배치된 뒤 **그 폭을 그대로** max-content 로 보고했다(InlineRun 만 줄 폭으로 고쳐져 있었음). 그래서 실사이트에서도: `display:block` 링크를 담은 flex 내비의 항목들이 글자 폭이 아니라 **균등 분배**(784px 를 3개가 261씩 — Chrome 은 글자 폭) | 탐색 중엔 폭이 auto 이고 자식이 모두 인라인인 경계도 배치된 줄의 너비로(`taffy_bridge.rs` `probe_sizes_to_lines`) — 줄의 **폭(왼쪽 끝~오른쪽 끝)**이지 오른쪽 끝이 아니다: 첫 수정은 오른쪽 끝을 재서 `text-align: center` 줄이 대리 폭 한가운데(~5e5)에 놓이면 여전히 1.25e7px 였다(실제 폰트 5만 재실행에서 같은 시드). 기존 InlineRun 분기도 같이 폭으로. `flex_items_with_inline_content_size_to_their_text_not_the_probe_width`. (뒤에 드러난 회귀 — 경계 항목에 `flex-grow`·grid 배치가 안 넘어가던 것 — 는 R2 에서 고침.) **실사이트 배치가 바뀌는 수정**(flex 안 인라인 내용 항목이 Chrome 처럼 내용 폭) — 코퍼스 재측정 참고. 표·float 등 다른 경계 모양은 그대로(R6) |

**생성기 보정(버그 아님)**: 음수 % 여백 중첩(`margin: -52%` → 단계마다 ~2배, Chrome 도 같음)과 큰 패딩 × 긴 `nowrap` 줄은 정상 CSS 가 실제로 1e7px 를 넘는 경우라 유계 풀에서 뺐다([0.3 구현 메모](#03-퍼징)).

**남은 것 (0c 에서 안 고침)**:

| # | 무엇 | 왜 남김 · 담당 |
|---|---|---|
| R1 | ~~인라인 크기 계산이 O(n × 인라인 깊이)~~ → **수정**(후속, 미커밋). 인라인 상자마다 서브트리 전체를 다시 합산하고 자손 상자가 또 합산(`inline_total_size` ↔ `inline_content_size`). DOM 깊이 상한(256) 뒤에도 중첩 `<span>` 1만 개(깊이 256 + 평탄화된 형제)가 release **7.79초** | `inline_total_size` 를 **레이아웃 한 번 동안 메모**(`inline::with_inline_size_memo`, `layout_tree` 가 감쌈): 키 = (DOM `NodeId`, 가용 폭 비트) — `StyledNode` 주소가 아님(레이아웃 상자·경계 잎이 스타일 서브트리 **사본**을 들고, 경계 사본은 패스 중간에 해제돼 주소가 재사용될 수 있음; 사본은 같은 id·같은 값, 레이아웃은 스타일을 안 고침). 패스 밖에선 메모 없음, 중첩 패스는 자기 메모(바깥 것은 끝날 때 — 패닉에도 — 되돌림), 조회만이라 HashMap 순서가 안 샘. **측정(release)**: 중첩 span 1,000 / 2,500 / 5,000 / 10,000 개 = 687ms / 1.73s / 3.92s / **7.79s → 96ms / 296ms / 648ms / 1.59s**. 테스트 `nested_inline_boxes_are_sized_once_per_layout_not_once_per_ancestor`(깊이 200: 실제 계산 횟수 ≤ 4×노드 — 수정 전 401 노드에 **80,400**회; 시간 대신 횟수라 플래키하지 않음). **기하 바이트 동일**: 임시 지문(퍼저 2만 케이스의 모든 상자 치수 f32 비트 + 페인트 목록 해시 — 장난감 측정 `4763ff1a4d31c52b`, 실제 폰트 5천 `c28d82c1856b0ec5`)이 전후 같음, 골든 변화 없음, 코퍼스 `r1` 이 `r4` 와 `observation.txt`·`layout.txt`·`elements.txt` 까지 바이트 동일. **남은 것(R7)**: 1만 개에 아직 1.59초 — 표본의 대부분이 `StyledNode` 의 `clone`/`drop`: 레이아웃 상자마다 `BoxType::BlockNode(node.clone())` 로 **스타일 서브트리 전체를 깊은 복사**(상자 n 개 × 서브트리 크기 = 역시 O(n × 깊이)), `shift_layout_subtree` 도 단계마다 서브트리를 옮김. 상자가 `Rc`/id 로 스타일을 가리키게 하는 구조 변경이라 별도 작업 |
| R2 | ~~인라인 자식만 있는 `<body>` 의 내용 폭이 784 가 아니라 768~~ → **수정**(후속, 미커밋). **원인**: 인라인 내용만 가진 요소는 legacy 경계 잎인데, taffy 는 잎의 measure 에 **테두리 상자**용 공간(자기 여백을 이미 뺀 값 — taffy `compute_leaf_layout` 문서)을 주고, `measure_boundary` 가 그걸 legacy 블록 배치의 부모 폭으로 넘겨 legacy 가 여백을 **한 번 더** 뺐다. `<body>` 만이 아니라 좌우 여백이 있는 모든 경계 잎(인라인 글만 든 `<blockquote>`(40px)·여백 있는 flex 항목 등)이 여백 두 배만큼 좁았다. **고치다 드러난 것**: 경계 잎의 taffy 스타일에 `flex-grow`/`flex-shrink`/`flex-basis`/`align-self`/`grid-column`/`grid-row` 가 없어서, 인라인 내용 flex 항목은 **자라지 않았고** grid 항목은 배치를 무시하고 순서대로 놓였다 — 0c F11 전엔 max-content 가 1e6 대리 폭이라 줄어들며 가려졌다가 F11 뒤 글자 폭에 머물렀다(**F11 이 낸 회귀**). | `measure_boundary` 가 확정 폭이면 legacy 에 `폭 + 자기 좌우 여백`(%는 taffy 의 부모 크기 기준, `auto` 는 0)을 준다. `boundary_taffy_style` 이 항목 속성 여섯 개를 넘긴다(블록 흐름 부모는 무시). `boundary_box` 는 캐시된 legacy 배치(마지막 measure 호출의 것)가 taffy 의 최종 테두리 폭과 다르면 그 폭으로 다시 배치(방어 — 최종 크기 없이 탐색 결과가 놓이는 일 방지). 테스트 `boundary_leaf_margins_are_subtracted_once`(body 784, blockquote, 여백 flex 항목), `flex_grow_item_with_inline_content_fills_the_row`, `grid_placement_of_an_item_with_inline_content_is_honoured`. 골든 변화 없음. **코퍼스 재측정**(`r2`, 오프라인, `r3` 대비): 결과·버림·요소 수·요소 목록 25곳 동일(`diff` 빈 결과), 관찰이 바뀐 곳은 github 의 `page_height` 8,229 → 8,277 하나(슬리버 위치가 48px 아래로) — 실사이트 `<body>` 는 대개 블록 자식이 있어 폭이 안 바뀜 |
| R3 | ~~스크립트가 만든 깊이(`appendChild` 반복)는 상한 없음~~ → **수정**(0c 뒤 후속, 미커밋). 자동 보안 리뷰 지적: 페이지가 `appendChild` 를 반복하거나, 떼어 낸 채로 깊은 사슬을 만든 뒤 한 번에 붙이거나, 이미 깊은 곳에 `innerHTML` 을 넣으면 스타일·레이아웃·페인트 재귀가 스택을 넘겨 **abort** — 에이전트/MCP 세션 전체가 죽는다. 같은 길로 **고리**(조상을 자기 후손에 붙이기)도 막혀 있지 않았다(무한 재귀) | `MAX_DOM_DEPTH` 를 `dom.rs` 로 옮겨(`html::MAX_DOM_DEPTH` 는 re-export) **트리 변경 경계에서** 강제: `Document::append_child`·`insert_before`·`replace_child`·`replace_with_text` 가 `Result<(), TreeError>` (`Cycle`·`TooDeep`·`NotAChild`) — 검사는 `parent` 에서 뿌리까지 올라가며(≤ 상한 걸음, 붙은 트리든 떼어 낸 트리든) 고리를 보고, 옮길 노드의 **서브트리 높이**를 남은 자리만큼만 훑는다(넘는 순간 멈춤, 새 노드·잎은 O(1)). 파서와 `clone_node` 만 내부 `attach_unchecked`(새 노드 + 자체 평탄화 / 사본은 원본보다 안 큼). **동작은 둘로 나눔**: 파서 경로(`innerHTML` — 요소 깊이 + 1 에서 시작하는 `parse_fragment_at`)는 문서 파싱과 같이 **평탄화**, 스크립트 변경 API(`appendChild`·`insertBefore`·`replaceChild`·조각(fragment) 삽입·`textContent=`)는 **거절**하고 `TypeError: … HierarchyRequestError: the tree would be nested too deep` 를 던진다(검사는 떼어 내기 **전에** — 거절되면 아무것도 안 바뀜; `innerHTML` 도 기존 자식을 지우기 전에 파싱). 거절을 고른 이유: 스크립트가 놓은 노드를 조용히 다른 부모에 붙이면 `parentNode` 가 스크립트가 정한 것과 달라져 프레임워크의 트리 모델(React 재조정 등)이 깨진다 — 던지면 스크립트가 알 수 있다. **Chrome**: DOM 명세엔 깊이 제한이 없고 Blink 도 파서만 자른다(512) — 스크립트 깊이는 Chrome 에서 통과하는 입력을 우리가 거절하는 것이지만, 256 단계 넘는 실사이트는 코퍼스 25곳에 없다(재측정 `r3` = `0c` 와 모든 열·요소 목록 동일, 스크립트/콘솔 에러 수 포함). 고리는 명세대로 `HierarchyRequestError`. 지금 JS 에 없는 API(`outerHTML=`·`insertAdjacentHTML`·`append`/`prepend`/`before`/`after`/`replaceWith`)는 폴리필이 쓰는 같은 갈고리를 지난다. 덤으로 `x.insertBefore(x, x)`·`replaceChild(x, x)` 가 떼어 낸 뒤 기준 노드를 다시 찾으며 `expect` 로 **패닉**하던 것도 이동 없음으로. 테스트: `dom` `script_mutations_past_the_depth_cap_are_refused`·`inserting_an_ancestor_into_its_descendant_is_refused`·`insert_before_itself_and_replace_with_itself_are_no_ops_not_panics`, `html` `fragment_parsed_below_a_deep_element_flattens_within_the_cap`, `js` `append_child_refuses_an_ancestor_with_hierarchy_request_error`·`inner_html_and_text_content_at_the_depth_cap`, 통합 `mb-runtime/tests/dom_depth.rs`(자식 프로세스·8MB 스레드: `appendChild`/`insertBefore` 1만 단계, 떼어 낸 256 단계 사슬 붙이기, 상한 근처 `innerHTML`, 고리 → 최대 깊이 256, 이후 스타일·레이아웃·페인트 — 수정 전 깊이 10,257). 남은 것: debug 2MB 스레드(테스트 스레드)는 ~132 단계부터 넘침(테스트용 깊은 페이지는 8MB 스레드로) |
| R4 | ~~래스터 이미지: `image` 한도(512MB) 안에서 디코드 후 `Vec<u32>` 로 한 벌 더~~ → **수정**(후속, 미커밋). 실제로는 두 벌 더였다: `load_from_memory` 의 버퍼 → `to_rgba8()` 복사 → 압축한 `Vec<u32>` 를 모으는 동안 RGBA8 이 살아 있어 정점에서 **픽셀당 ~8바이트**(결과는 4바이트) | `decode_raster`: 같은 `ImageReader` 로 크기를 먼저 읽고 **결과 버퍼(4B/px)를 `image` 기본 한도에서 미리 예약**(`load_from_memory` 가 하던 것과 같은 512MB 예산 — 이제 실제로 잡는 4B/px 로 셈) → 디코더가 8비트 원본(L8·La8·Rgb8·Rgba8)을 최종 `Vec<u32>` 의 앞쪽에 바로 쓰고 → **마지막 픽셀부터 거꾸로** 제자리에서 0xRRGGBB 로 펼침(픽셀 하나의 원본 ≤4바이트가 자기 4바이트 칸 뒤에 놓이는 일이 없어 안 읽은 바이트를 덮지 않음). 16비트·float 는 예전 경로(`into_rgba8`). 새 의존성 없음 — 대신 `u32` 버퍼의 바이트 보기에 **이 저장소 첫 `unsafe`**(`as_bytes_mut`, 3줄, 안전 근거 주석; 대안은 이미 트리에 있는 `bytemuck` 을 직접 의존성으로 — 결정은 리뷰에서). **측정**: 셈하는 전역 할당자 테스트(`mb-dom/tests/decode_peak.rs`, 합성 1200×900 PNG) 정점 RGB8·RGBA8·L8 모두 **8.00 → 4.38 B/px**(0.38 = PNG 디코더 자체 버퍼). 최대 RSS(release, `/usr/bin/time -l`, 합성 PNG — 파일 자체 포함): 8000² RGB **747 → 318 MB**, 8000² RGBA **590 → 334 MB**, 4000² RGBA 150 → 86 MB, 디코드 시간도 595 → 450ms(RGB). 테스트 `decoding_needs_about_one_output_buffer_not_three`(수정 전 8.00 B/px 로 실패), `raster_decode_matches_the_reference_conversion_for_every_format`(PNG L8/La8/Rgb8/Rgba8/Rgb16·JPEG·GIF·BMP 가 예전 변환과 픽셀 단위로 같음), `raster_header_claiming_too_many_pixels_hits_the_512_mb_budget`. 이미지 퍼저 2만 무결, 코퍼스 `r4` = `r2`(관찰 파일까지 바이트 동일). 남은 차이: 예약을 3B/px 가 아니라 4B/px 로 세므로 1.28억~1.7억 픽셀 RGB 는 이제 한도 초과(전엔 통과) |
| R7 | 레이아웃 상자가 스타일 서브트리를 깊은 복사(`BoxType::*Node(node.clone())`) + `shift_layout_subtree` 가 단계마다 서브트리 이동 → 깊은 트리에서 O(n × 깊이)(중첩 span 1만 개 1.59초 중 대부분, R1 뒤 표본) | `LayoutBox` 가 `Rc<StyledNode>` 나 `NodeId` 로 스타일을 가리키게 — 레이아웃·페인트·적중 판정·mb-agent 가 `box_type` 의 노드를 읽는 곳 전부를 건드리는 구조 변경. 별도 작업 |
| R6 | flex/grid 크기 탐색에서 표·float·절대 위치·% 패딩 등 **다른** legacy 경계 모양은 여전히 1e6 대리 폭을 max-content 로 보고할 수 있음(F11 은 인라인 내용 경계만 고침). 이번 긴 실행에선 재현 안 됨 | 각 모양의 max-content 를 legacy 쪽에서 정의해야 함 — 3단계(배치) 이후 |
| R5 | 페인트 전용 값(그림자·변환·그라디언트)은 포화하지 않음 | 래스터가 견딤을 확인(극단값 손 케이스 16개 + CSS 토큰 퍼징 래스터 10만). tiny-skia 상류 버그(F7)가 우리 래스터 경로에서 재현된 적은 없음 |
