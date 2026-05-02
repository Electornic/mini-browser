# Data Model

## Overview

이 문서는 브라우저 파이프라인의 핵심 자료구조를 정리한다. 실제 필드 구성은 구현 과정에서 조정될 수 있지만, 각 구조체의 역할과 경계는 유지하는 것을 목표로 한다.

## DOM

### `Document` (arena)

DOM 트리는 `NodeId` 기반 arena 로 보관된다. parent/child link 가 owned `Vec<Node>` 가 아니라 인덱스라서, JS 측 long-lived wrapper 가 트리 재배치 후에도 stable 한 핸들로 같은 노드를 가리킬 수 있다. 삭제는 tombstone — 슬롯은 비워두고 ID 는 invalidate.

예시:

```rust
pub struct Document {
    nodes: Vec<Option<Node>>,
    roots: Vec<NodeId>,
}

pub struct Node {
    pub node_type: NodeType,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

pub enum NodeType {
    Element(ElementData),
    Text(String),
}
```

JS 측 `JsRuntime` 과 BrowserState 가 `Rc<RefCell<Document>>` 를 공유하므로, JS 핸들러의 `appendChild` / `removeChild` 가 **즉시** layout 단에서 보인다 (다음 프레임 layout 빌드 시 새 트리를 그대로 read).

### `ElementData`

HTML element의 이름과 속성을 저장한다.

예시:

```rust
pub struct ElementData {
    pub tag_name: String,
    pub attributes: AttrMap,
}
```

## CSS

### `Stylesheet`

파싱된 CSS 규칙들의 컨테이너다.

예시:

```rust
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}
```

### `Rule`

selector와 declaration 묶음이다.

예시:

```rust
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
}
```

### `Selector`

최소 구현에서는 단순 selector만 지원한다.

예시:

```rust
pub enum Selector {
    Tag(String),
    Class(String),
    Id(String),
}
```

### `Declaration`

속성 이름과 값 쌍이다.

예시:

```rust
pub struct Declaration {
    pub name: String,
    pub value: Value,
}
```

### `Value`

스타일 값 표현이다.

예시:

```rust
pub enum Value {
    Keyword(String),
    Length(f32, Unit),
    Color(Color),
}
```

## Style

### `StyledNode`

DOM 노드에 대해 계산된 스타일 결과를 연결한 트리다.

예시:

```rust
pub struct StyledNode {
    pub node: Node,
    pub specified_values: PropertyMap,
    pub children: Vec<StyledNode>,
}
```

역할:
- selector matching 결과 보관
- inheritance 반영
- layout 단계의 직접 입력

## Layout

### `LayoutBox`

화면에 배치될 박스 단위다.

예시:

```rust
pub struct LayoutBox {
    pub box_type: BoxType,
    pub dimensions: Dimensions,
    pub children: Vec<LayoutBox>,
}
```

### `Dimensions`

박스의 위치와 크기, spacing 정보를 저장한다.

예시:

```rust
pub struct Dimensions {
    pub content: Rect,
    pub padding: EdgeSizes,
    pub border: EdgeSizes,
    pub margin: EdgeSizes,
}
```

### `BoxType`

박스 종류는 layout 모드별로 분기된다. flex / grid 는 자체 레이아웃 알고리즘을 가진다.

예시:

```rust
pub enum BoxType {
    BlockNode(StyledNode),
    InlineNode(StyledNode),
    InlineBlockNode(StyledNode),
    FlexNode(StyledNode),
    GridNode(StyledNode),
    AnonymousBlock,
}
```

비고:
- 각 variant 의 `StyledNode` 가 `node_id` 를 들고 있어서 layout box → DOM 노드 역추적이 가능 (click hit-test → dispatch_event 에서 사용).

## Painting / Rendering

### `DisplayCommand`

렌더러가 실제로 실행할 그리기 명령이다.

예시:

```rust
pub enum DisplayCommand {
    SolidRect(Color, Rect),
    Text(TextCommand),
}
```

### `TextCommand`

텍스트와 위치, 스타일 정보를 묶는다.

예시:

```rust
pub struct TextCommand {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub color: Color,
    pub font_size: f32,
}
```

## Network

### `Url`

문서와 리소스 주소를 구조적으로 표현한다.

예시:

```rust
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    pub path: String,
}
```

### `HttpRequest`

최소 GET 요청 표현이다.

예시:

```rust
pub struct HttpRequest {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
}
```

### `HttpResponse`

응답 메타데이터와 body를 저장한다.

예시:

```rust
pub struct HttpResponse {
    pub status_code: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
```

### `Resource`

다운로드한 외부 리소스 표현이다.

예시:

```rust
pub enum Resource {
    Html(String),
    Css(String),
    Image(Vec<u8>),
}
```

## JavaScript

### `JsRuntime`

페이지 단위 Boa engine wrapper. 같은 Document arena 를 공유해 JS mutation 이 layout 에 즉시 반영되게 한다.

예상 역할:
- Boa `Context` 보유 (페이지 단위 globals 보존)
- `Rc<RefCell<Document>>` 공유로 DOM read/mutate
- 이벤트 listener registry — `(NodeId, event_type) -> Vec<JsObject>`
- 비동기 잡 큐 — 커스텀 `FrameJobExecutor` (microtask + due timer 만 비블로킹 drain)
- requestAnimationFrame 콜백 스냅샷 큐

좁은 외부 API:
- `JsRuntime::new(dom)` — 페이지 navigate 시 새로 만든다 (이전 globals/listener 초기화)
- `execute(source) -> Result<String, String>` — `<script>` 본문 실행 후 microtask 자동 drain
- `dispatch_event(target, event_type)` — bubble dispatch + microtask drain
- `drain_pending_jobs()` — 매 프레임 시작에 호출, due 인 timer/microtask drain
- `run_animation_frame_callbacks()` — 매 프레임 시작에 호출, snapshot-then-fire

### `Job` (Boa)

Boa 의 `Job` enum 을 그대로 사용 (PromiseJob / GenericJob / TimeoutJob / AsyncJob). 우리 `FrameJobExecutor` 는 PromiseJob / GenericJob 은 빈 큐 될 때까지 drain, TimeoutJob 은 deadline `<= now` 인 것만 발사, AsyncJob 은 dropped (Phase 3 에서 활성화 예정).

## App State

### `BrowserState`

앱 단위 상태를 관리한다.

예상 역할:
- 현재 URL, address bar 입력 / focus 상태
- 현재 DOM (`Rc<RefCell<Document>>`) — `JsRuntime` 과 공유
- 현재 stylesheet 목록 (parsed)
- 캐시된 image / font / external script 본문
- back/forward history snapshot stack
- hover/focus DOM path
- last error 또는 loading state

매 프레임 `display_list(viewport, input, fonts) -> Vec<DisplayCommand>` 호출이 진입점.

## Data Flow Summary

```text
HTML text -> Document (NodeId arena)
CSS text -> Stylesheet
Document + Stylesheet (+ interaction state) -> StyledNode
StyledNode + viewport -> LayoutBox
LayoutBox -> DisplayCommand[]
URL -> HttpResponse -> Html / Css / Image / Script body / Font bytes
JS source -> Boa Context (mutates Document)
User input + LayoutBox hit-test -> JsRuntime.dispatch_event
```

## Related Documents

- [Project Spec](spec.md)
- [Architecture](architecture.md)
- [Roadmap](roadmap.md)
