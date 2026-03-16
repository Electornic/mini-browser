# Data Model

## Overview

이 문서는 브라우저 파이프라인의 핵심 자료구조를 정리한다. 실제 필드 구성은 구현 과정에서 조정될 수 있지만, 각 구조체의 역할과 경계는 유지하는 것을 목표로 한다.

## DOM

### `Node`

문서 트리의 기본 단위다.

예상 역할:
- element node 표현
- text node 표현
- child node 보관

예시:

```rust
pub struct Node {
    pub children: Vec<Node>,
    pub node_type: NodeType,
}

pub enum NodeType {
    Element(ElementData),
    Text(String),
}
```

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

최소 버전의 박스 종류다.

예시:

```rust
pub enum BoxType {
    BlockNode(StyledNode),
    InlineNode(StyledNode),
    AnonymousBlock,
}
```

비고:
- 첫 버전에서 inline layout을 생략한다면 `InlineNode`는 나중에 추가해도 된다.

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

## App State

### `BrowserState`

앱 단위 상태를 관리한다.

예상 역할:
- 현재 URL
- 현재 DOM
- 현재 stylesheet 목록
- 현재 layout tree 또는 display list
- last error 또는 loading state

## Data Flow Summary

```text
HTML text -> Node
CSS text -> Stylesheet
Node + Stylesheet -> StyledNode
StyledNode + viewport -> LayoutBox
LayoutBox -> DisplayCommand[]
URL -> HttpResponse -> Html/Css/Image resource
```

## Related Documents

- [Project Spec](spec.md)
- [Architecture](architecture.md)
- [Roadmap](roadmap.md)
