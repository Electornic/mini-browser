# Project Spec

## Goal

이 프로젝트는 학습과 실험을 목적으로 한 미니 브라우저 구현이다. 목표는 다음 파이프라인을 실제 코드로 연결하는 것이다.

```text
URL
  -> document download
  -> HTML parse
  -> DOM tree build
  -> CSS parse
  -> style resolve
  -> block layout
  -> rect/text render
```

## In Scope

### Runtime

- 단일 프로세스 브라우저 실행
- 단일 창(Window) 기반 렌더링
- 간단한 redraw 중심 event loop

### Document

- 잘 형식이 맞는 HTML 입력
- 기본적인 element node / text node 처리
- 최소 속성 파싱 (`id`, `class`, `href`, `src`, `rel`)

### CSS

- 단순 selector 지원
- `tag`, `.class`, `#id`
- 기본 declaration parsing
- 일부 기본 태그 스타일 제공 (`body`, `p`, `h1`, `a`)
- 최소 property set:
  - `display`
  - `width`
  - `height`
  - `margin-*`
  - `padding-*`
  - `border-*`
  - `border-color`
  - `background-color`
  - `color`
  - `font-size`

### Layout

- block formatting flow only
- 부모 너비를 기준으로 자식 block 박스를 위에서 아래로 배치
- margin/padding 반영

### Rendering

- 사각형 배경 그리기
- 텍스트 그리기
- 기본 흰색 배경 또는 단일 캔버스 clear

### Networking

- URL parsing
- HTTP GET
- HTML 다운로드
- CSS 다운로드
- 이미지 다운로드 선택 지원
- 상대 URL 해석

## Out of Scope

- JavaScript engine
- DOM mutation API
- flexbox / grid / inline formatting context
- full CSS cascade compliance
- full HTML5 parsing algorithm
- HTTPS/TLS 직접 구현
- redirect, cache, cookie, compression
- accessibility tree
- browser tabs, history, navigation UI

## Assumptions

- 초기 입력은 비교적 단순하고 예측 가능한 HTML/CSS다.
- malformed document recovery는 초기에 다루지 않는다.
- 네트워크는 우선 단순 성공 케이스 위주로 구현한다.
- 렌더러는 pixel-perfect보다 파이프라인 연결이 우선이다.

## Non-Goals For First Version

- 웹 표준 완전 준수
- 성능 최적화
- 멀티스레드 로딩
- 복잡한 텍스트 shaping
- 고급 폰트 관리

## Success Criteria

- 로컬 문자열 HTML/CSS를 파싱해 창에 텍스트와 배경을 렌더할 수 있다.
- 원격 URL에서 HTML을 다운로드하고 stylesheet를 추가 로드할 수 있다.
- DOM, style, layout, render 단계가 명확히 분리되어 있다.
- 각 단계는 독립 테스트 또는 디버그 출력으로 검증 가능하다.

## Related Documents

- [Architecture](architecture.md)
- [Data Model](data-model.md)
- [Roadmap](roadmap.md)
