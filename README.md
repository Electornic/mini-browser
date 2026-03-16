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

아직 렌더러, window/event loop, network/resource loader는 구현 전이다.

## Documents

- [Project Spec](docs/spec.md)
- [Architecture](docs/architecture.md)
- [Data Model](docs/data-model.md)
- [Roadmap](docs/roadmap.md)

## Run

```bash
cargo run
```

## Notes

- 초기 구현은 최소 기능 우선이다.
- 불필요한 리팩터링보다 단계별 동작 검증을 우선한다.
- 처음부터 완전한 HTML/CSS 브라우저를 목표로 하지 않는다.
