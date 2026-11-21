# ClaudeTranslatorRust

Rust로 만든 Claude Cli를 이용한 GWS 번역기

Windows 트레이에 상주하면서 Chrome의 Google Workspace(Gmail, Chat, Docs, Slides, Sheets)에서 단축키로 번역과 한글 교정을 하는 프로그램입니다. 번역은 이 PC에 설치된 Claude Code CLI를 호출해서 합니다.

## 단축키

| 단축키 | 동작 |
|---|---|
| Alt+Alt | 선택한 텍스트(또는 입력 중인 칸)를 번역해 바꿔 넣기 |
| Ctrl+Ctrl | 번역 결과를 창으로 보기 |
| Ctrl을 누른 채 Alt+Alt | 한글 교정 후 바꿔 넣기 |
| Alt를 누른 채 Ctrl+Ctrl | 한글 교정 결과를 창으로 보기 |

- Target 언어는 여러 개 고를 수 있고, 정한 순서대로 모두 번역합니다.
- Google Docs에서는 블록을 선택해야 합니다.
- Google Sheets에서는 셀 범위를 선택하면 범위 전체를 한 번에 처리합니다.

## 실행 파일

`dist\ClaudeTranslator.exe` 하나로 실행됩니다(정적 CRT, 별도 런타임 설치 불필요).

전제 조건:
- Claude Code CLI 설치와 로그인(`claude login`)
- 이전 Python 버전(ClaudeLangTrans v5)과 동시에 실행하지 않기(단축키가 두 번 동작함)

설정과 개인 사전, 로그는 `%APPDATA%\ClaudeTranslator`에 저장됩니다.

## 소스에서 빌드하기

1. Rust 설치: https://rustup.rs
2. Visual Studio Build Tools 설치: "C++를 사용한 데스크톱 개발" 항목 선택(Windows SDK 포함)
3. 소스 받기:

   ```powershell
   git clone https://github.com/ixuskiss/ClaudeTranslatorRust.git
   cd ClaudeTranslatorRust
   ```

4. 실행(개발 중):

   ```powershell
   cargo run -p ct-app --release
   ```

5. 배포용 단일 exe 만들기:

   ```powershell
   $env:RUSTFLAGS = "-C target-feature=+crt-static --remap-path-prefix=$env:USERPROFILE=~ --remap-path-prefix=$PWD=."
   cargo build --release -p ct-app
   Copy-Item target\release\ct-app.exe dist\ClaudeTranslator.exe
   ```

   `--remap-path-prefix`는 exe 안에 들어가는 소스 경로에서 PC 사용자 폴더 이름을 지웁니다.

처음 빌드할 때는 라이브러리를 받아 전체를 컴파일하므로 몇 분 걸립니다. `target` 폴더는 빌드 결과물이라 git에 올리지 않습니다.

## 검사

```powershell
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

## 구조

| 크레이트 | 역할 |
|---|---|
| `ct-core` | 설정, 개인 사전, 언어 판별, 번역·교정 서비스, 배치 처리 |
| `ct-llm` | Claude CLI 클라이언트, 프롬프트, 모델 목록 |
| `ct-sheets` | Google Sheets 셀 모델, 클립보드 HTML 파서, 교체·검증 |
| `ct-win` | 키보드 훅, UI Automation, 클립보드, Sheets 백엔드 |
| `ct-app` | 트레이, Slint 창, 단축키 흐름, 조립 |

설계와 계획 문서는 `docs/superpowers/`에, 수동 테스트 절차는 `tests/manual/`에 있습니다.
