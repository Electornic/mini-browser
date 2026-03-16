# Architecture

## Overview

브라우저는 파이프라인형 아키텍처로 구성한다. 각 단계는 앞 단계의 결과를 다음 단계의 입력으로 넘기며, 자료구조 경계를 명확히 유지한다.

```text
Window/Event Loop
  -> Browser App State
  -> Network Loader
  -> HTML/CSS Parser
  -> DOM / Stylesheet
  -> Style Engine
  -> Layout Engine
  -> Display List
  -> Renderer
```

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
- HTTP GET 요청
- 응답 body 및 content type 처리
- relative URL resolution

입력:
- URL 문자열

출력:
- document bytes 또는 text
- resource bytes 또는 text

비고:
- 처음에는 `http://` 중심으로 시작하고 `https://`는 라이브러리 위임 여부를 별도로 결정한다.
- 현재는 `TcpStream` 기반 단순 HTTP GET으로 main document HTML만 읽는다.
- stylesheet resource는 DOM에서 `<link rel="stylesheet">`를 추출한 뒤 같은 계층에서 추가 다운로드한다.

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

### `app`

역할:
- 전체 파이프라인 조립
- document load -> parse -> style -> layout -> render 호출
- 에러 처리와 상태 전이 관리

입력:
- 시작 URL 또는 HTML 문자열

출력:
- 최종 display list

## Recommended Dependency Direction

```text
app
  -> window
  -> net
  -> html
  -> css
  -> style
  -> layout
  -> render

html -> dom
style -> dom, css
layout -> style
render -> layout
```

원칙:
- `dom`은 상위 단계에 의존하지 않는다.
- `layout`은 네트워크나 파서 구현을 알지 못한다.
- `render`는 CSS selector나 HTML token을 알지 못한다.

## Main Flow

1. 사용자가 URL 또는 정적 문서를 제공한다.
2. `net`이 HTML 문서를 읽는다.
3. `html`이 DOM tree를 생성한다.
4. DOM 내부의 stylesheet 링크를 수집한다.
5. `net`이 CSS를 읽고 `css`가 stylesheet로 파싱한다.
6. `style`이 DOM에 최종 스타일을 계산한다.
7. `layout`이 박스 크기와 위치를 계산한다.
8. `render`가 display list를 만든다.
9. `window`가 이를 화면에 그린다.

## Error Handling Strategy

- parser 에러는 우선 단순 실패 또는 제한된 복구로 처리
- network 에러는 사용자에게 메시지 또는 fallback 화면 렌더링
- unsupported CSS/property는 무시
- unsupported tag는 generic block element로 취급 가능

## Related Documents

- [Project Spec](spec.md)
- [Data Model](data-model.md)
- [Roadmap](roadmap.md)
