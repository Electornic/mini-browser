# Understanding Guide

이 문서는 현재 `mini-browser` 코드베이스를 처음 읽는 사람을 위한 안내서다.

목표는 다음 3가지를 빠르게 파악하는 것이다.

1. 이 브라우저가 전체적으로 어떤 흐름으로 동작하는지
2. `src/`를 어떤 순서로 읽으면 이해가 쉬운지
3. 각 파일에서 무엇을 중점적으로 봐야 하는지

## Big Picture

이 프로젝트는 아래 파이프라인으로 동작한다.

```text
URL 또는 샘플 문서
  -> net / resource (병렬 fetch)
  -> html / css parse
  -> dom (NodeId arena) / stylesheet
  -> js (Boa, document/window globals, 이벤트 디스패치)
  -> style
  -> layout (block / inline / flex / grid / position …)
  -> render (rect / text / image / gradient / shadow / transform)
  -> window
```

즉, "문자열을 구조화하고", "구조에 스타일을 입히고", "JS 가 트리를 mutate 하고", "화면 좌표를 계산한 뒤", "픽셀로 그린다" 는 흐름이다. JS mutation 은 같은 `Rc<RefCell<Document>>` 에 가해지므로 다음 frame 의 layout 이 자동으로 새 트리를 본다.

## Read Order

처음 읽을 때는 아래 순서를 추천한다.

1. `src/main.rs`
2. `src/dom.rs`
3. `src/html.rs`
4. `src/css.rs`
5. `src/style.rs`
6. `src/layout.rs`
7. `src/render.rs`
8. `src/window.rs`
9. `src/net.rs`
10. `src/resource.rs`
11. `src/js.rs` (JS 에 익숙하지 않으면 마지막에 봐도 됨 — 다른 단계 안 바뀌고 Document arena 만 mutate 한다)

이 순서가 좋은 이유는, 먼저 앱 전체 조립 방식을 본 뒤, 파이프라인을 앞에서 뒤로 따라갈 수 있기 때문이다.

## File Guide

### `src/main.rs`

가장 먼저 봐야 하는 파일이다.

이 파일의 역할:
- 브라우저 상태를 보관
- URL 입력, history, scroll, link click 같은 상호작용 처리
- 문서 로드
- 최종적으로 렌더링에 넘길 display list 생성

중점적으로 볼 함수:
- `BrowserState`
  - 앱 전체 상태가 여기에 있다
- `display_list(...)`
  - 매 프레임마다 무엇을 그릴지 결정한다
- `apply_input(...)`
  - 키보드/마우스 입력이 상태를 어떻게 바꾸는지 보여준다
- `navigate(...)`
  - 주소창 이동이 어떤 로딩 흐름을 타는지 보여준다
- `build_document_view(...)`
  - 실제 브라우저 파이프라인의 핵심 연결 지점이다

이 파일을 이해하면 “이 브라우저가 어떻게 움직이는지”를 전체적으로 잡을 수 있다.

### `src/dom.rs`

HTML parser가 만들어내는 기본 트리 구조다.

중점적으로 볼 타입:
- `Node`
- `NodeType`
- `ElementData`

핵심 포인트:
- 이후 style/layout/render는 모두 이 DOM tree를 읽는다
- text node와 element node를 구분한다
- attribute는 `AttrMap`에 저장된다

### `src/html.rs`

HTML 문자열을 DOM tree로 바꾸는 파서다.

중점적으로 볼 함수:
- `parse(...)`
- `parse_nodes(...)`
- `parse_element(...)`
- `parse_text(...)`

핵심 포인트:
- 이 파서는 잘 형식이 맞는 HTML만 다룬다
- 브라우저처럼 복잡한 복구(recovery)는 하지 않는다
- `<tag ...>children</tag>`와 text를 재귀적으로 트리로 만든다

읽을 때 질문:
- “문자열이 어디서 element가 되고 어디서 text가 되는가?”
- “닫는 태그 불일치는 어떻게 처리하는가?”

### `src/css.rs`

CSS 문자열을 규칙 목록으로 바꾸는 파서다.

중점적으로 볼 타입:
- `Stylesheet`
- `Rule`
- `Selector`
- `Declaration`
- `Value`

중점적으로 볼 함수:
- `parse(...)`
- `parse_rule(...)`
- `parse_selectors(...)`
- `parse_value(...)`

핵심 포인트:
- selector는 `tag`, `.class`, `#id`만 지원
- value는 `keyword`, `length(px)`, `color`
- CSS를 완전 구현하는 게 아니라, 브라우저 뒤 단계를 움직일 만큼만 구현했다

### `src/style.rs`

DOM과 CSS를 연결하는 단계다.

중점적으로 볼 타입:
- `StyledNode`

중점적으로 볼 함수:
- `style_tree(...)`
- `specified_values(...)`
- `matches_selector(...)`
- `default_values(...)`

핵심 포인트:
- DOM node마다 최종 스타일 맵을 만든다
- 기본 우선순위는 `tag < class < id`
- 일부 속성(`color`, `font-size`)은 상속한다
- 브라우저 기본 스타일처럼 최소 UA-like defaults도 여기서 준다

읽을 때 질문:
- “CSS rule이 어떤 node에 적용되는가?”
- “적용된 뒤 결과는 어디에 저장되는가?”

### `src/layout.rs`

스타일이 적용된 트리를 화면 좌표가 있는 박스로 바꾸는 단계다.

중점적으로 볼 타입:
- `Rect`
- `Dimensions`
- `LayoutBox`

중점적으로 볼 함수:
- `layout_tree(...)`
- `layout_node(...)`
- `layout_inline_children(...)`
- `inline_content_width(...)`

핵심 포인트:
- block layout과 basic inline flow를 둘 다 담당
- margin/padding/border를 고려해 content rect를 계산
- 텍스트 폭은 실제 폰트 측정이 아니라 단순 근사치로 계산
- image는 기본 intrinsic size를 가진다

이 파일을 이해하면 “왜 어떤 텍스트가 저 위치에 그려지는지”를 이해할 수 있다.

### `src/render.rs`

레이아웃 박스를 실제 그리기 명령으로 바꾸고, 다시 픽셀 버퍼로 래스터라이즈하는 단계다.

중점적으로 볼 타입:
- `DisplayCommand`
- `TextCommand`
- `ImageCommand`

중점적으로 볼 함수:
- `build_display_list(...)`
- `paint_layout_box(...)`
- `rasterize(...)`
- `draw_text(...)`
- `draw_image(...)`

핵심 포인트:
- 먼저 `LayoutBox -> DisplayCommand`
- 그 다음 `DisplayCommand -> pixel buffer`
- 즉, “무엇을 그릴지”와 “어떻게 픽셀에 찍을지”가 한 파일 안에서 이어진다
- 텍스트는 간단한 비트맵 글리프 방식이다

### `src/window.rs`

OS 창과 입력 이벤트를 다루는 매우 얇은 계층이다.

중점적으로 볼 타입:
- `WindowInput`

중점적으로 볼 함수:
- `run(...)`

핵심 포인트:
- `minifb` 창을 열고
- 입력을 수집하고
- 매 프레임 `build_scene(...)`을 호출해 그릴 내용을 받아온다

이 파일은 “렌더러를 실제 창에 연결하는 어댑터”라고 보면 된다.

### `src/net.rs`

문서와 리소스를 네트워크로 가져오는 단계다.

중점적으로 볼 타입:
- `Url`
- `HttpResponse`
- `FetchResult`
- `NetworkError`

중점적으로 볼 함수:
- `Url::parse(...)`
- `Url::resolve(...)`
- `fetch(...)`
- `http_get(...)`
- `load_html_document(...)`

핵심 포인트:
- `http`와 `https` 둘 다 처리
- redirect 추적
- `Location`을 base URL 기준으로 해석
- CSS/image loader가 공통으로 쓰는 기반 계층

### `src/resource.rs`

HTML에서 외부 리소스를 찾아 실제로 읽어오는 단계다.

중점적으로 볼 함수:
- `load_stylesheets(...)`
- `load_images(...)`
- `load_external_scripts(...)`
- `collect_stylesheet_urls(...)`
- `collect_image_urls(...)`

핵심 포인트:
- DOM tree를 순회하며 `<link rel="stylesheet">`, `<img src>`, `<script src>` 를 찾는다
- `thread::scope` 로 병렬 fetch — `net` 의 keep-alive 풀이 같은 host 연결을 재사용
- `net` 모듈은 "한 URL을 가져오는 역할"
- `resource` 모듈은 "DOM에서 필요한 URL들을 모으고 병렬로 가져오는 역할"

### `src/js.rs`

페이지 스크립트를 실행하는 JS 엔진 wrapper 다.

중점적으로 볼 타입:
- `JsRuntime` — 페이지 단위 Boa `Context` + listener registry + rAF 큐
- `FrameJobExecutor` — 커스텀 Boa `JobExecutor`. `run_jobs` 가 비블로킹: due 인 timer 만 발사, future timer 는 큐에 둠
- `ListenerMap` — `(NodeId, event_type) -> Vec<JsObject>`

중점적으로 볼 함수:
- `JsRuntime::new(dom)` — 페이지 navigate 시 새로 만든다
- `execute(source)` — `<script>` 본문 실행 + microtask 자동 drain
- `dispatch_event(target, "click")` — text → Element retarget 후 bubble dispatch
- `drain_pending_jobs()` — 매 프레임 시작에 호출, due timer + microtask drain
- `run_animation_frame_callbacks()` — 매 프레임 시작에 호출, snapshot-then-fire
- `register_document` / `register_timers` / `register_console` / `register_window_aliases` — host API binding

핵심 포인트:
- 외부 모듈은 Boa 타입 (`Context`, `JsObject`) 을 직접 import 하지 않는다 — `JsRuntime` 의 좁은 메서드만 사용
- `Rc<RefCell<Document>>` 가 BrowserState 와 공유되므로 JS mutation → 다음 프레임 layout 에 즉시 반영
- 비동기 큐는 한 곳 (`FrameJobExecutor`) 에서 관리. setInterval 은 closure 안에서 자기 자신을 재 enqueue 하는 패턴
- `JsRuntime::new_with_fixed_clock` 는 test-only — 실제 wall clock 안 건드리고 deterministic timer 검증

읽을 때 질문:
- "JS 가 만든 새 element 가 어떻게 화면에 나타나는가?" (답: 같은 Document arena 에 append → 다음 프레임 layout 이 본다)
- "setTimeout 콜백이 정확히 언제 실행되는가?" (답: `display_list` 가 frame 시작에 `drain_pending_jobs` 호출 → due 인 것만 발사)
- "click 이벤트가 어디서 페이지 element 로 전달되는가?" (답: `display_list` 의 hit-test → `dispatch_event(node_id, "click")`)

## Recommended Learning Path

코드를 읽으면서 아래 순서로 직접 추적해보면 이해가 빠르다.

1. `main`에서 샘플 문서 또는 URL이 어디서 들어오는지 확인
2. `build_document_view(...)`에서 HTML/CSS -> style -> layout -> render 흐름 따라가기
3. `window::run(...)`이 매 프레임 어떻게 `WindowInput`을 넘기는지 보기
4. 클릭/스크롤/히스토리처럼 한 가지 상호작용을 골라 상태가 어떻게 바뀌는지 보기

추천 예시:
- 주소 입력 후 `Enter`
- 링크 클릭
- 뒤로/앞으로
- 스크롤

## What To Ignore At First

처음 읽을 때 아래는 잠시 무시해도 된다.

- 테스트 코드 전체
- 이미지 디코딩 세부사항
- TLS 세부 구현
- hover/underline 같은 UI polish

먼저 “문서가 어떻게 화면이 되는지”만 잡고, 그 다음 상호작용과 네트워크를 보면 된다.

## Mental Model

이 프로젝트를 한 문장으로 기억하면 이렇다.

`문자열(HTML/CSS)을 트리로 바꾸고 -> JS 가 트리를 mutate 하고 -> 트리에 스타일을 입히고 -> 박스 좌표를 계산하고 -> 화면에 그리는 작은 브라우저`

이 문장만 머리에 두고 각 파일을 보면, 현재 코드 구조를 훨씬 덜 헷갈리게 읽을 수 있다.

매 프레임의 흐름은 이렇다:

```text
입력 수집 (마우스/키)
  -> JS job drain (microtask + due timer)
  -> requestAnimationFrame 콜백 drain
  -> style 계산 (interaction state 반영)
  -> layout 빌드
  -> 클릭 hit-test → JsRuntime.dispatch_event
  -> display list 발행
  -> 화면 라스터
```

JS mutation 은 어디서 일어나든 다음 프레임 layout 이 자동으로 새 트리를 본다 — 명시적 invalidate 가 없는 이유는 layout 이 매 프레임 새로 빌드되기 때문이다 (정확성 우선, 성능은 나중에).
