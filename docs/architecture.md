# Architecture

## Overview

브라우저는 파이프라인형 아키텍처로 구성한다. 각 단계는 앞 단계의 결과를 다음 단계의 입력으로 넘기며, 자료구조 경계를 명확히 유지한다.

```text
Window/Event Loop
  -> Browser App State
  -> Network Loader (HTTP/HTTPS, redirect, keep-alive 풀)
  -> Resource Fetcher (stylesheet/이미지/스크립트 병렬 fetch)
  -> HTML/CSS Parser
  -> DOM (NodeId arena) / Stylesheet
  -> JS Runtime (Boa, document/window globals, 이벤트 디스패치)
  -> Style Engine (cascade + inheritance + pseudo-state)
  -> Layout Engine (block / inline / inline-block / position / float / flex / grid)
  -> Display List
  -> Renderer (rect / rounded-rect / text / image / gradient / shadow / transform)
```

매 프레임마다 입력 → JS job drain (microtask + due timer) → rAF 콜백 drain → layout 빌드 → display list → 라스터 순서로 흐른다. JS 가 핸들러 안에서 한 DOM mutation 은 같은 프레임의 layout 에 즉시 반영되도록 `Rc<RefCell<Document>>` 한 arena 를 모든 단계가 공유한다.

## Module Boundaries

### `window`

역할:
- OS 창 생성
- 입력 이벤트 수신
- redraw 타이밍 제어

입력:
- display list 또는 render tree

출력:
- 없음

비고:
- 첫 버전에서는 스크롤, 클릭 처리 없이 정적 렌더만 지원해도 된다.
- 현재는 `DisplayCommand`를 소프트웨어 래스터라이즈해 단일 버퍼를 창에 표시한다.

### `net`

역할:
- URL parsing
- HTTP/HTTPS GET 요청 (redirect 추적 포함)
- 응답 body 및 content type 처리
- relative URL resolution

입력:
- URL 문자열

출력:
- document bytes 또는 text
- resource bytes 또는 text

비고:
- HTTP/HTTPS 모두 지원. HTTPS 는 `native-tls` connector 를 통해 같은 요청/응답 파싱 경로로 연결.
- **HTTP keep-alive 풀** 보유 — 같은 host 에 대해 TCP 연결을 재사용해 stylesheet/이미지/스크립트 병렬 fetch 시 핸드셰이크 비용을 줄인다.
- chunked transfer encoding 디코딩.
- redirect status는 `Location` 헤더를 따라가며 최종 URL을 app state에 반영.

### `resource`

역할:
- DOM 트리에서 외부 resource 참조 (`<link rel="stylesheet">`, `<img src>`, `<script src>`, `@font-face`) 수집
- `thread::scope` 로 병렬 fetch
- 디코딩된 image, font 데이터, script body 를 BrowserState 에 전달

입력:
- 파싱된 DOM
- base URL

출력:
- decoded image map, font byte 배열, stylesheet 문자열, external script 본문 map

비고:
- `BrowserState::install_document` 가 새 페이지 set up 시 한 번 호출하고, 결과를 history snapshot 에 stash 하므로 back/forward 시 재페치 없이 즉시 복원.

### `html`

역할:
- HTML text를 token/element/text 구조로 해석
- DOM tree 생성 지원

입력:
- HTML 문자열

출력:
- `dom::Node`

비고:
- tokenizer와 parser를 분리할 수 있지만, 초기에는 단일 모듈로 시작해도 된다.

### `dom`

역할:
- 문서 구조 표현
- element / text node 저장

입력:
- parser 결과

출력:
- style engine이 참조할 트리 구조

### `css`

역할:
- CSS text를 selector와 declaration 목록으로 파싱

입력:
- CSS 문자열

출력:
- `Stylesheet`

### `style`

역할:
- DOM node와 CSS rule 매칭
- inheritance와 우선순위를 적용해 styled tree 생성

입력:
- DOM tree
- Stylesheet 목록

출력:
- `StyledNode`

비고:
- 최소 구현에서는 UA stylesheet 없이 기본값만 제공한다.

### `layout`

역할:
- styled tree를 layout tree/box tree로 변환
- block flow 기준 위치와 크기 계산

입력:
- styled tree
- viewport size

출력:
- `LayoutBox`

### `paint` 또는 `render`

역할:
- layout tree를 그릴 수 있는 명령 목록으로 변환
- rect/text 중심 primitive 생성

입력:
- layout tree

출력:
- `DisplayCommand` 목록

### `js`

역할:
- Boa engine wrapper (`JsRuntime`) — 페이지 단위 `Context` 보유
- DOM 바인딩 (`document` / `window` / `self` globals, Element / Text wrapper, getter/setter, mutation, 이벤트 listener registry)
- 이벤트 디스패치 (`dispatch_event(target, "click")` — text → Element retarget, bubble dispatch)
- 비동기 잡 큐 — 커스텀 `FrameJobExecutor` 로 microtask + 만료된 timer 만 비블로킹 drain, `requestAnimationFrame` 스냅샷 큐

입력:
- 공유 `Rc<RefCell<Document>>`
- 매 프레임 main loop 의 `drain_pending_jobs` / `run_animation_frame_callbacks` / `dispatch_event` 호출

출력:
- 같은 Document arena 에 대한 in-place mutation (다음 layout pass 가 자동으로 본다)

비고:
- 외부 모듈은 Boa 타입 (`Context`, `JsObject`, `JsValue`) 을 직접 import 하지 않는다 — `JsRuntime` 의 좁은 메서드 (`execute`, `dispatch_event`, `drain_pending_jobs`, `run_animation_frame_callbacks`) 만 호출한다.
- `Rc<RefCell<Document>>` 는 `JsRuntime` 과 BrowserState 가 같은 arena 를 공유하므로 JS mutation → 다음 frame 의 layout 에 즉시 반영된다.

### `app` (`main.rs::BrowserState`)

역할:
- 전체 파이프라인 조립
- document load -> parse -> style -> layout -> render 호출
- 에러 처리와 상태 전이 관리
- history (back/forward), chrome 입력 (주소창 / 버튼 / 키 단축키)
- 매 프레임 `display_list` 호출 — 입력 → JS job drain → rAF drain → layout 빌드 → display command 발행

입력:
- 시작 URL 또는 HTML 문자열

출력:
- 최종 display list (window 가 라스터화)

## Recommended Dependency Direction

```text
app
  -> window
  -> net
  -> resource
  -> html
  -> css
  -> style
  -> layout
  -> render
  -> js

html -> dom
style -> dom, css
layout -> style
render -> layout
js -> dom, css (selector parser 재사용)
resource -> net, html, dom
```

원칙:
- `dom`은 상위 단계에 의존하지 않는다.
- `layout`은 네트워크나 파서 구현을 알지 못한다.
- `render`는 CSS selector나 HTML token을 알지 못한다.
- `js`는 layout/render 단계를 모른다 — DOM 만 mutate 하고 다음 frame 의 layout 가 자동으로 새 트리를 본다.

## Main Flow

1. 사용자가 URL 또는 정적 문서를 제공한다.
2. `net`이 HTML 문서를 읽는다.
3. `html`이 DOM tree를 생성한다.
4. `resource` 가 DOM 에서 stylesheet / 이미지 / 스크립트 / 폰트 참조를 수집해 `thread::scope` 로 병렬 fetch.
5. `css`가 stylesheet 문자열을 파싱한다.
6. `js::JsRuntime` 이 새로 만들어지고 `<script>` 본문을 순서대로 실행 (inline + external src 둘 다). microtask 는 `execute` 끝에 자동 drain.
7. 매 프레임 `BrowserState::display_list`:
   1. 입력 처리 + frame index 증가
   2. `js.drain_pending_jobs()` → 만료된 setTimeout/setInterval + microtask 발사
   3. `js.run_animation_frame_callbacks()` → 큐 스냅샷 후 호출
   4. `style`이 DOM 에 최종 스타일을 계산 (interaction state — hover/focus/active — 도 함께)
   5. `layout`이 박스 크기와 위치를 계산
   6. 클릭 hit-test → 페이지 element 면 `js.dispatch_event(node_id, "click")` (link navigate 보다 먼저)
   7. `render`가 display list를 만든다
8. `window`가 이를 화면에 그린다.

## Error Handling Strategy

- parser 에러는 우선 단순 실패 또는 제한된 복구로 처리
- network 에러는 사용자에게 메시지 또는 fallback 화면 렌더링
- unsupported CSS/property는 무시
- unsupported tag는 generic block element로 취급 가능

## Related Documents

- [Project Spec](spec.md)
- [Data Model](data-model.md)
- [Roadmap](roadmap.md)
