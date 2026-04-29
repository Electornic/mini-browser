# mini-browser

브라우저 렌더링/스타일/JS 엔진을 Rust로 직접 구현해보는 학습용 프로젝트.

URL 또는 정적 입력에서 HTML/CSS를 읽어 DOM/스타일/레이아웃 구조로 변환한 뒤, 자체 렌더러로 화면에 그린다. 단계(Phase) 단위로 범위를 늘려간다 — Phase 0은 정적 렌더링 + Chrome 스타일 UI까지, Phase 1은 CSS 표현력 확장(flexbox/grid 포함), Phase 2는 JS 엔진 임베드와 DOM 바인딩.

## Phase Status

| Phase | Status | Theme |
|---|---|---|
| 0 | **Done** | Block layout + Chrome 스타일 UI + NTP 시작 페이지 |
| 1 | Backlog | CSS Expansion (units, selectors, position, flexbox, grid) |
| 2 | Backlog | JS Engine Integration (Boa 임베드 + DOM 바인딩) |

자세한 task 분해는 [docs/roadmap.md](docs/roadmap.md) 참조.

## Pipeline

```text
URL or sample input
  -> Network Loader
  -> HTML / CSS text
  -> HTML Parser / CSS Parser
  -> DOM Tree / Stylesheet
  -> Style Engine
  -> Block Layout (margin: auto, text-align: center, border-radius)
  -> Display Commands (SolidRect / RoundedRect / Text / Image)
  -> Rasterizer
  -> Window
```

## Project Status (Phase 0 Done)

### 렌더링 파이프라인
- DOM 자료구조, HTML/CSS parser, Style Engine
- Block Layout Engine (`margin: auto`, `text-align: center` 지원)
- Display List: `SolidRect` / `RoundedRect` / `Text` / `Image`
- 기본 inline 흐름 (`<a>` / `<span>` / `<img>`)

### 네트워크 / 리소스
- HTTP/HTTPS GET + redirect 추적
- `<link rel="stylesheet">` 자동 로드
- `<img src>` 자동 로드 + 렌더링
- web font (`@font-face`) 로드 + macOS 시스템 폰트 fallback (한글 표시)
- 에러 페이지, `text/plain` 페이지 처리

### Chrome UI
- 상단 chrome (88px = 32 tab strip + 56 toolbar)
- 단일 탭 (위쪽 corner만 둥근 RoundedRect)
- pill 모양 주소창 (focus 시 blue ring)
- chevron back/forward 아이콘 + 3-dot 메뉴 (rect-composed icons)
- 히스토리 navigation (Alt+←/→ 또는 `Cmd/Ctrl+[ ]`)

### CSS support (Phase 0 시점)
- selector: tag/class/id (단일)
- 단위: `px`만
- properties: `width`/`height`/`margin-*`/`padding-*`/`border-*`/`background-color`/`color`/`font-size`/`text-align`/`border-radius`
- inheritance: `color`, `font-size`, `text-align`
- 색상: hex (`#RGB` / `#RRGGBB`)

### 시작 페이지 (NTP)
URL 인자 없이 실행하면 Chrome NTP 스타일 페이지가 보임 — 가운데 큰 로고, pill 검색창, 단축 타일 4개.

## Documents

- [Project Spec](docs/spec.md)
- [Architecture](docs/architecture.md)
- [Data Model](docs/data-model.md)
- [Roadmap](docs/roadmap.md)
- [Understanding Guide](docs/understanding-guide.md)

## Run

```bash
cargo run
```

원격 HTML을 불러오려면:

```bash
cargo run -- http://example.com
```

또는:

```bash
cargo run -- https://example.com
```

원격 문서의 외부 stylesheet는 `<link rel="stylesheet" href="...">`에 한해 자동 로드된다.
원격 문서의 `<img src="...">` 이미지는 자동 로드되어 기본 박스로 렌더된다.
`text/plain` 문서는 읽기용 페이지로 렌더된다.
로드 실패나 unsupported content type은 간단한 에러 페이지로 렌더된다.

앱 안에서는 상단 주소창을 클릭하거나 `Cmd+L`/`Ctrl+L`로 포커스한 뒤 URL을 입력하고 `Enter`로 이동할 수 있다. `Alt+Left/Right` 또는 `Cmd/Ctrl+[ ]`, 그리고 상단 뒤로/앞으로 버튼 클릭으로 이동할 수 있다. `Up/Down`, `PageUp/PageDown`, 마우스 휠로 스크롤할 수 있고, 문서 안의 `<a href>` 링크는 밑줄로 표시되며 hover 시 강조되고 클릭으로 이동할 수 있다. `Esc`를 누르면 종료된다.

## Package macOS

macOS용 `.app` 및 `.dmg`를 만들려면:

```bash
./scripts/package-macos.sh
```

산출물:

- `dist/mini-browser.app`
- `dist/mini-browser.dmg`

현재 스크립트는 unsigned app을 생성한다. 외부 배포용이면 이후 `codesign`과 notarization을 추가해야 한다.

## Notes

- 단계(Phase)별 task 단위로 commit을 작게 끊는다.
- 각 Phase 종료 시 [docs/roadmap.md](docs/roadmap.md)와 README/AGENTS의 status를 갱신한다.
- "정확하게 동작하는 작은 브라우저"가 1차 목표이고, 표준 100% 호환은 목표가 아니다.
