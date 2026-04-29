# AGENTS.md

이 문서는 `/Users/leejun/Desktop/Projects/mini-browser` 이하 전체에 적용된다.

## Project Goal

이 프로젝트는 Rust로 구현하는 학습용 미니 브라우저다. 단계(Phase) 단위로 범위를 늘려간다.

**Phase 0 (Done)** — 블록 layout 기반 정적 렌더링 파이프라인 + Chrome 스타일 chrome:
- Window / Event Loop, HTML Parser, DOM Tree, CSS Parser, Style Engine
- Block Layout Engine (`margin: auto`, `text-align: center` 포함), 기본 inline 흐름
- Renderer: Rect + RoundedRect + Text + Image, `border-radius` hookup
- Simple Network Loader (HTTP/HTTPS + redirect), Resource Loader (CSS/이미지/web font)
- Chrome 스타일 chrome (탭 strip, pill 주소창, chevron 아이콘) + Chrome NTP 모양 시작 페이지

**Phase 1 (Backlog)** — CSS Expansion (units, advanced selectors, position, flexbox, grid 등). 자세한 task는 `docs/roadmap.md` 참조.

**Phase 2 (Backlog)** — JS Engine Integration (Boa 임베드 + DOM 바인딩 + reflow trigger).

상세 범위와 설계는 아래 문서를 기준으로 한다.
- `README.md`
- `docs/spec.md`
- `docs/architecture.md`
- `docs/data-model.md`
- `docs/roadmap.md` (Phase별 task 매트릭스 — source of truth)

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

- HTML/CSS는 표준 전체를 구현하지 않는다. 필요한 만큼만 단계적으로 늘린다.
- block layout이 기본. inline 흐름은 `<a>`/`<span>`/`<img>` 같은 명시적 인라인 태그에 한정.
- Phase 1에서 layout 모드를 추가할 때마다 기존 block 경로를 깨지 않게 한다 (`display: inline-block`, `position` 등).
- renderer는 rect/rounded-rect/text/image primitive 중심. 새 paint 효과(gradient, shadow 등)는 새 프리미티브로 추가하고 기존을 깨지 않는다.
- 모듈 경계 유지: parser, style, layout, render, network 사이의 책임을 흐리지 않는다.
- 설계 기준 자료구조는 `docs/data-model.md`를 우선 참조한다.

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

## Long-term Roadmap

Phase 0(블록 layout + Chrome 스타일 UI + NTP)은 main에 반영됨. 이후 단계는 학습 우선 순서대로 정리되어 있다.

- **Phase 1 — CSS Expansion**: 단위(`%`/`em`/`rem`), descendant/child/`:hover` selectors, `inline-block`, `position`, `transform`, `box-shadow`, gradient, **flexbox**, **grid** 등.
- **Phase 2 — JS Engine Integration**: [Boa](https://crates.io/crates/boa_engine) 임베드, DOM 바인딩(`document`/`querySelector`/`addEventListener`), mutation → reflow trigger, event loop, `setTimeout`/`requestAnimationFrame`, `fetch` 기본 GET.

Phase별 task 분해와 작업량/학습 가치 매트릭스는 [docs/roadmap.md](docs/roadmap.md)에 있다. 진행 시 그 파일이 source of truth.

### 명시적 비포함

다음 항목은 학습 가치 대비 범위 폭증이 커서 로드맵에서 제외한다(필요 시 별도 Phase로 다시 평가):

- WebGL / Canvas 2D context
- WebSocket, IndexedDB, ServiceWorker
- HTTP/2/3, compression(gzip/brotli)
- 멀티탭 UI, 히스토리 페이지
- 폰트 셰이핑(HarfBuzz 수준), BiDi 정교화

## Working Style

- 최소 변경으로 진행한다.
- 불필요한 리팩터링, 대규모 포맷 변경, 파일 이동은 피한다.

## Git Workflow

- 적당한 작업 단위가 끝날 때마다 `git add .` 후 `git commit`까지 진행해 변경을 작은 단위로 남긴다.
- GitHub 관련 작업은 가능하면 `gh` CLI를 우선 사용한다.
- PR 조회, 리뷰 확인, 코멘트 확인, PR 생성 같은 작업은 `gh` 기준으로 수행한다.
- PR review 사항을 반영한 뒤에는 해당 review thread에 답글을 달고, resolve 처리까지 진행한다.
