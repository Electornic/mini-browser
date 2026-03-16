# AGENTS.md

이 문서는 `/Users/leejun/Desktop/Projects/mini-browser` 이하 전체에 적용된다.

## Project Goal

이 프로젝트는 Rust로 구현하는 학습용 미니 브라우저다.

현재 목표 범위:
- Window / Event Loop
- HTML Parser
- DOM Tree
- CSS Parser
- Style Engine
- Block Layout Engine
- Basic Renderer (Rect + Text)
- Simple Network Loader
- Basic Resource Loader

상세 범위와 설계는 아래 문서를 기준으로 한다.
- `README.md`
- `docs/spec.md`
- `docs/architecture.md`
- `docs/data-model.md`
- `docs/roadmap.md`

작업 전에 관련 문서를 먼저 읽고, 구현 방향이 문서와 어긋나면 문서도 함께 갱신한다.

## Priorities

1. 요구사항 충족
2. 최소 변경(minimal diff)
3. 안전성(빌드/테스트/타입)
4. 가독성

## Working Rules

- 불필요한 리팩터링, 대규모 포맷팅, 파일 이동 금지
- 기존 코드 스타일과 패턴을 최대한 유지
- 작은 단위로 구현하고 단계별로 검증
- 적당한 작업 단위가 끝날 때마다 `git add .` 후 `git commit`까지 진행해 변경을 작은 단위로 유지
- 현재 범위를 벗어나는 기능은 임의로 추가하지 않음
- 미지원 기능은 억지로 일반화하지 말고 명시적으로 제한
- parser, style, layout, render, network 모듈 경계를 흐리지 않음

## Implementation Guidance

- 초기 구현은 “정적 입력으로 렌더링 파이프라인 완성”을 우선한다.
- 네트워크는 렌더링 파이프라인이 최소 동작한 뒤 연결한다.
- HTML/CSS는 처음부터 표준 전체를 구현하지 않는다.
- block layout only 원칙을 유지한다.
- renderer는 rect/text primitive 중심으로 시작한다.
- 설계 기준 자료구조는 `docs/data-model.md`를 우선 참조한다.

권장 구현 순서:
1. `dom`
2. `html`
3. `css`
4. `style`
5. `layout`
6. `render`
7. `window`
8. `net`
9. `resource loader`

## Code Organization

모듈 추가 시 아래 책임 분리를 지향한다.

- `dom`: 문서 트리 자료구조
- `html`: HTML 파싱
- `css`: CSS 파싱
- `style`: selector matching, inheritance, computed/specified values
- `layout`: block layout 계산
- `render` 또는 `paint`: display command 생성 및 화면 출력 연결
- `net`: URL parsing, HTTP GET, resource fetch
- `app`: 전체 파이프라인 조립

한 모듈이 다른 단계의 내부 구현 세부사항을 직접 알지 않도록 유지한다.

## Verification

코드 변경 시 가능한 범위에서 아래를 실행한다.

1. lint
   - `cargo fmt --check`
   - `cargo clippy --all-targets --all-features -- -D warnings`
2. typecheck
   - `cargo check`
3. test
   - `cargo test`
4. build
   - `cargo build`

모든 검증이 항상 가능하지 않다면:
- 실행하지 못한 이유를 명시
- 필요한 정확한 커맨드를 제시
- 예상 실패 지점을 짧게 설명

문서만 변경한 경우에는 build/test를 생략할 수 있다.

## Documentation Rules

아래 항목에 영향이 있으면 관련 문서를 함께 업데이트한다.
- 사용법
- 구성
- 환경변수
- 스크립트
- 아키텍처
- 주요 데이터 구조
- 구현 범위
- 주요 플로우

문서 업데이트 후보:
- `README.md`
- `docs/spec.md`
- `docs/architecture.md`
- `docs/data-model.md`
- `docs/roadmap.md`

문서 변경이 필요 없으면, 왜 생략했는지 한 줄로 남긴다.

## Output Expectations

작업 보고에는 아래 항목을 포함한다.

- 요약(불릿 3~5개)
- 변경 파일 목록
- 핵심 diff 또는 패치
- 실행한 커맨드 및 결과
- 문서 업데이트 여부(업데이트함/생략함 + 사유 1줄)

## Non-Goals

초기 단계에서 아래는 우선 구현 대상이 아니다.

- JavaScript engine
- full HTML5 parser
- full CSS compliance
- flexbox / grid
- 복잡한 inline layout
- cache, cookie, redirect, compression
- 멀티탭, 히스토리 UI

## Working Style

- 최소 변경으로 진행한다.
- 불필요한 리팩터링, 대규모 포맷 변경, 파일 이동은 피한다.

## Git Workflow

- 적당한 작업 단위가 끝날 때마다 `git add .` 후 `git commit`까지 진행해 변경을 작은 단위로 남긴다.
- GitHub 관련 작업은 가능하면 `gh` CLI를 우선 사용한다.
- PR 조회, 리뷰 확인, 코멘트 확인, PR 생성 같은 작업은 `gh` 기준으로 수행한다.
- PR review 사항을 반영한 뒤에는 해당 review thread에 답글을 달고, resolve 처리까지 진행한다.
