# mini-browser

작은 범위의 브라우저를 Rust로 구현하는 학습용 프로젝트다.

이 프로젝트의 목표는 URL 또는 정적 입력으로부터 HTML/CSS를 읽고, 이를 DOM/스타일/레이아웃 구조로 변환한 뒤, 가장 단순한 형태의 렌더러로 화면에 그리는 것이다.

## Current Scope

- Window / Event Loop
- HTML Parser
- DOM Tree
- CSS Parser
- Style Engine
- Block Layout Engine
- Basic Renderer (Rect + Text)
- Simple Network Loader
- Basic Resource Loader

## Initial Pipeline

```text
URL
  -> Network Loader
  -> HTML / CSS text
  -> HTML Parser / CSS Parser
  -> DOM Tree / Stylesheet
  -> Style Engine
  -> Layout Tree
  -> Display Commands
  -> Window Renderer
```

## Project Status

현재는 아래 기반 모듈이 구현된 상태다.

- DOM 자료구조
- HTML parser
- CSS parser
- Style Engine
- Block Layout Engine
- Basic Renderer (Display List: Rect + Text)
- Window / Event Loop
- Simple Network Loader
- Basic Resource Loader
- Basic Image Loader

현재는 `http://`와 `https://` 기반 HTML 문서 다운로드, 기본 redirect 추적, `<link rel="stylesheet">` CSS 로드, `<img src>` 이미지 다운로드 및 기본 렌더링을 지원한다.
기본 박스 렌더링에는 `background-color`, `border-*`, `border-color`가 반영된다.
또한 `body`, `p`, `h1`, `a`에는 최소 기본 스타일이 적용된다.
`a`, `span`, text node는 기본적인 inline 흐름으로 한 줄에 배치된다.

## Documents

- [Project Spec](docs/spec.md)
- [Architecture](docs/architecture.md)
- [Data Model](docs/data-model.md)
- [Roadmap](docs/roadmap.md)

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
로드 실패나 unsupported content type은 간단한 에러 페이지로 렌더된다.

앱 안에서는 상단 주소창을 클릭하거나 `Cmd+L`/`Ctrl+L`로 포커스한 뒤 URL을 입력하고 `Enter`로 이동할 수 있다. `Up/Down`, `PageUp/PageDown`, 마우스 휠로 스크롤할 수 있고, 문서 안의 `<a href>` 링크는 밑줄로 표시되며 hover 시 강조되고 클릭으로 이동할 수 있다. `Esc`를 누르면 종료된다.

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

- 초기 구현은 최소 기능 우선이다.
- 불필요한 리팩터링보다 단계별 동작 검증을 우선한다.
- 처음부터 완전한 HTML/CSS 브라우저를 목표로 하지 않는다.
