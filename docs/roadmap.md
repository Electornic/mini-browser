# Roadmap

## Principles

- 작게 구현하고 바로 검증한다.
- 각 단계는 독립적으로 디버그 가능해야 한다.
- 먼저 정적 입력으로 렌더링 파이프라인을 완성한 뒤 네트워크를 붙인다.

## Milestone 1: DOM Foundations

목표:
- DOM 자료구조 정의
- 기본 HTML parser 구현

완료 조건:
- 간단한 HTML 문자열을 element/text tree로 변환할 수 있다.
- 중첩 구조와 속성(`id`, `class`)을 읽을 수 있다.
- 디버그 출력 또는 테스트로 tree 구조를 확인할 수 있다.

검증 예시:
- `"<div id='a'><p>Hello</p></div>"` 파싱 테스트

## Milestone 2: CSS Foundations

목표:
- CSS parser 구현
- 단순 selector와 declaration 해석

완료 조건:
- tag/class/id selector를 읽을 수 있다.
- declaration 목록을 property/value 형태로 저장할 수 있다.
- 잘 형식이 맞는 CSS 입력을 테스트로 검증할 수 있다.

검증 예시:
- `"div { color: red; } .note { margin-top: 8px; }"`

## Milestone 3: Style Engine

목표:
- DOM과 CSS를 연결해 styled tree 생성

완료 조건:
- selector matching이 동작한다.
- 기본 우선순위(tag < class < id)를 반영한다.
- 일부 상속 속성(`color`, `font-size`)이 동작한다.

검증 예시:
- 동일 노드에 여러 규칙이 적용될 때 최종 스타일 비교

## Milestone 4: Block Layout

목표:
- block formatting context 기반 레이아웃 구현

완료 조건:
- viewport 폭 기준으로 block 박스를 위에서 아래로 배치한다.
- width, margin, padding, background 영역을 계산할 수 있다.
- layout tree 또는 dimensions를 테스트로 검증할 수 있다.

검증 예시:
- 부모 폭 800일 때 자식 박스 좌표/크기 확인

## Milestone 5: Basic Painting

목표:
- layout tree를 display command로 변환
- rect + text 렌더링 구현

완료 조건:
- 배경 사각형과 텍스트를 화면에 그릴 수 있다.
- 최소 하나의 샘플 문서를 창에 렌더할 수 있다.

검증 예시:
- hard-coded DOM/CSS를 입력으로 사용한 렌더링 확인

## Milestone 6: Window Integration

목표:
- event loop와 renderer 연결

완료 조건:
- 창 생성 후 초기 렌더가 가능하다.
- resize 또는 redraw 이벤트에 반응할 수 있다.

검증 예시:
- 앱 실행 시 샘플 페이지 표시

## Milestone 7: Network Loader

목표:
- URL parsing
- HTTP GET
- HTML 다운로드

완료 조건:
- URL 문자열에서 scheme/host/path를 파싱할 수 있다.
- 원격 HTML을 GET으로 가져와 parser로 넘길 수 있다.
- 오류 응답 또는 연결 실패를 처리할 수 있다.

검증 예시:
- 단일 테스트 서버 또는 고정 URL fetch

## Milestone 8: Resource Loader

목표:
- 외부 stylesheet 로드
- 상대 URL 해석
- 선택적으로 이미지 로드

완료 조건:
- `<link rel="stylesheet">`를 읽고 CSS를 추가 다운로드할 수 있다.
- base URL 기준으로 상대 경로를 절대 경로로 해석할 수 있다.
- 여러 stylesheet를 style engine에 전달할 수 있다.

검증 예시:
- HTML + external CSS 조합 렌더링

## Milestone 9: Stabilization

목표:
- 모듈 정리
- 테스트 보강
- 디버그 도구 추가

완료 조건:
- parser/style/layout 핵심 테스트가 존재한다.
- 주요 데이터 구조 출력이나 trace가 가능하다.
- 최소 데모 페이지가 안정적으로 렌더된다.

## Suggested Work Order

1. `dom`
2. `html`
3. `css`
4. `style`
5. `layout`
6. `render`
7. `window`
8. `net`
9. `resource loader`

## Verification Strategy

- parser 계층은 unit test 중심
- style/layout 계층은 snapshot 또는 구조 비교 중심
- renderer/window 계층은 수동 실행 검증
- network 계층은 integration test 또는 샘플 서버 기반 검증

## Next Concrete Step

다음 구현은 `HTTPS 지원` 또는 `렌더 품질 보강`이다. 현재 HTML, external CSS, 이미지 리소스까지 로드할 수 있으므로, 이제 네트워크 현실화나 레이아웃/텍스트 렌더 품질 개선 단계로 넘어갈 수 있다.

## Related Documents

- [Project Spec](spec.md)
- [Architecture](architecture.md)
- [Data Model](data-model.md)
