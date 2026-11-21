# ClaudeTranslator (Rust) 설계

- 작성일: 2026-11-20
- 원본: `C:\python_workspace\ClaudeLangTrans_v5` (Python, PySide6). 원본은 읽기만 하고 수정하지 않는다.
- 대상 위치: `C:\rust-projects\ClaudeTranslator_rust`

## 1. 목적과 범위

### 1.1 목적

v5를 Rust로 다시 만든다. 이유는 세 가지다.

1. 배포 개선: PyInstaller exe 대신 작고 빨리 뜨는 단일 exe.
2. 반응 속도와 안정성: 키보드 훅, UIA, 클립보드 처리의 지연과 불안정을 줄인다.
3. 새 기능 추가: 다중 Target 번역, Alt+Alt "원문 포함" 옵션, 한글 다듬기 강화.

### 1.2 범위

v5 기능 전체를 옮긴다. 아래 "v5와 다른 점"만 예외다.

- 트레이 상주. 메뉴: Target Language, Personal Dictionary, Settings, Pause Translation, Exit.
- Alt+Alt (번역 후 교체), Ctrl+Ctrl (번역 보기 창).
- 한글 교정: Ctrl+Alt+Alt (교정 후 교체), Alt+Ctrl+Ctrl (교정 보기).
- 개인 사전(KEEP/TRANSLATE)과 결정적 마스킹(`⟦N⟧`).
- 언어 판별(한/영 혼합 처리), 자동 양방향 번역.
- Google Sheets: 셀 범위 표 보기, 범위 일괄 교체, 편집 중 셀 교체.
- Claude Code CLI 상주 서브프로세스(stream-json), 모델·effort 목록 조회와 캐시.
- Settings, 자동 시작(Start on Login), 깨진 파일 허용과 시작 경고.

### 1.3 v5와 다른 점

| 항목 | v5 | Rust 버전 |
|---|---|---|
| Target 언어 | 1개 | 1~3개, 순서 지정, 순서대로 모두 번역 |
| Alt+Alt 결과 | 번역문 | 옵션에 따라 원문 + 번역문(들) |
| 사전 | company / team / user 3계층 병합 | **개인 사전(`user.yaml`)만** |
| 한글 다듬기 | 적극 수정 | 의미가 같으면 문장 전체 재작성 허용, 과장된 AI식 단어 금지 |
| 앱 이름·데이터 | `ClaudeLangTrans_v5`, `%APPDATA%\ClaudeLangTrans_v5` | `ClaudeTranslator`, `%APPDATA%\ClaudeTranslator` |
| v5 데이터 | — | 가져오지 않는다. 완전히 별개 앱 |
| 이전 버전 감지 | v3/v4 자동 실행 경고 | 넣지 않는다 |

키보드 훅은 앱끼리 분리되지 않는다. v5와 동시에 실행하면 단축키 한 번에 두 앱이 모두 반응한다. 이 점은 사용자 가이드에 적는다.

### 1.4 기준 동작 (앱별)

| 앱 | Alt+Alt (번역 후 교체) | Ctrl+Ctrl (번역 보기) |
|---|---|---|
| Google Chat | 선택 영역 또는 작성 중인 메시지 전체 | 선택 영역 |
| Gmail | 선택 영역 또는 포커스된 입력란 전체(관련 없는 필드는 건드리지 않음) | 선택 영역 |
| Google Slides | 선택 영역 또는 편집 중인 텍스트 상자 전체 | 선택 영역 |
| Google Docs | 블록을 선택했을 때만 | 선택 영역 |
| Google Sheets | 셀 범위 일괄 교체 / 편집 중 셀(전체 또는 선택 글자) | 셀 범위 표 / 선택 글자 |

- 한글 교정 교체(Ctrl+Alt+Alt)의 범위는 Alt+Alt와 같다. Docs에서도 블록 선택이 필요하다.
- 교정 보기(Alt+Ctrl+Ctrl)의 범위는 Ctrl+Ctrl과 같다.
- 선택 영역이 필요한데 없으면 안전하게 실패하고 안내한다.
- 이 표는 v5 README와 Settings 안내 문구 기준이다. Sheets는 사용자가 v5에서 직접 확인했다(2026-11-20 보고).
- Rust 버전은 훅과 UIA 코드를 새로 쓰므로, 다섯 앱 모두 Rust exe로 다시 수동 확인해야 한다.

### 1.5 성공 기준

1. 1.4 표의 동작이 Rust exe에서 수동 테스트 절차대로 통과한다.
2. 다중 Target과 원문 포함이 2장의 규칙대로 다섯 앱 모두에서 동작한다.
3. 다듬기 모드가 3장의 규칙대로 동작한다.
4. Claude CLI가 없거나 로그인되지 않은 PC에서도 앱이 죽지 않고, 원문을 바꾸지 않고 실패를 안내한다.
5. `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`가 통과한다.

## 2. 새 기능: 다중 Target과 원문 포함

### 2.1 설정 모델

```rust
pub enum Lang { Ko, En, Vi, Ja }

pub struct UserSettings {
    pub base_language: Lang,               // 기본 Ko
    pub target_languages: Vec<Lang>,       // 기본 [En]. 순서 = 출력 순서
    pub include_original_on_replace: bool, // 기본 false
    pub alt_alt_interval_seconds: f64,     // 기본 0.2
    pub ctrl_ctrl_interval_seconds: f64,   // 기본 0.2
    pub start_on_login: bool,              // 기본 false
    pub claude_cli_path: String,           // "" = 자동 탐색
    pub ctrl_ctrl_display_mode: DisplayMode, // WholeText(기본) | PerSentence
    pub translation_tone: Tone,            // Neutral(기본) | Casual | Formal | Plain
    pub claude_model: String,              // 기본 "sonnet"
    pub claude_effort: String,             // 기본 "low"
    pub korean_correction_mode: CorrectionMode, // Spelling(기본) | Polish
}
```

불러올 때 `target_languages`를 정규화한다.

1. 모르는 언어 코드는 버린다.
2. base 언어를 뺀다.
3. 중복을 빼되, 처음 나온 순서를 유지한다.
4. 결과가 비면 base를 뺀 언어 중 첫 번째(`ko, en, vi, ja` 순)를 하나 넣는다.

나머지 항목은 v5처럼 처리한다. 종류가 틀린 값은 그 항목만 기본값으로 읽고, 파일 전체를 읽을 수 없으면 기본 설정으로 시작한다.

### 2.2 번역 방향

```rust
pub struct TranslationPlan { pub source: Lang, pub destinations: Vec<Lang> }

pub fn resolve_plan(detected: Option<Lang>, base: Lang, targets: &[Lang]) -> Option<TranslationPlan>;
```

| 감지된 언어 | 결과 |
|---|---|
| base | `destinations = targets` (순서 그대로) |
| targets 중 하나 | `destinations = [base]` |
| 판별 실패, 또는 그 밖의 언어 | `None`. 안전하게 실패하고 원문은 그대로 |

Target이 하나일 때 결과는 v5의 `resolve_translation_direction`과 같아야 한다.

### 2.3 LLM 요청과 응답 (한 번 호출로 모든 언어)

- 요청: `{ text(마스킹된), source, destinations, tone }`
- 응답 JSON: `{"translations": {"en": "...", "ja": "..."}}`
- 검증: 키 집합이 `destinations`와 정확히 같아야 한다. 아래는 모두 `LlmError`다.
  - 빠진 언어
  - 추가 언어
  - 문자열이 아닌 값
  - 빈 문자열(앞뒤 공백 제거 후)
- Target이 하나여도 같은 형식(키 1개)을 쓴다.
- 프롬프트는 v5 `_build_translation_prompt`에서 다음을 그대로 가져온다.
  - 형식 보존 지시
  - `⟦N⟧` 보존 지시
  - 어투 지시
  - haiku일 때 완화되는 형식 지시
- 여기에 "각 언어 번역은 서로 독립적으로, 원문에서 직접 번역" 지시를 더한다.
- JSON 파싱은 v5 `parse_json_response`와 같다. 먼저 코드펜스를 벗기고 파싱하고, 실패하면 각 `{` 위치에서 JSON 객체를 찾는다. 그래도 없으면 실패한다.

### 2.4 사전 처리

1. 원문을 한 번 마스킹한다.
2. 마스킹된 텍스트로 언어를 판별하고 `resolve_plan`을 실행한다.
3. LLM을 호출한다.
4. 언어마다 토큰 무결성을 검증하고, 그 언어 기준으로 복원한다. KEEP은 원형, TRANSLATE는 그 언어의 사전 번역을 쓴다. 번역 표기로 매칭된 경우에 한해 `term`으로 폴백한다(v5 규칙).
5. 한 언어라도 4에서 실패하면 전체 실패로 처리하고 원문을 바꾸지 않는다.

### 2.5 출력 조립

```rust
pub fn compose_replacement(original: Option<&str>, translations: &[String]) -> String;
```

- 블록 순서: [원문(`include_original_on_replace`가 켜져 있을 때)] → Target 순서대로 번역문
- 각 블록의 끝 줄바꿈(`\r`, `\n`)을 잘라낸 뒤 `"\n\n"`(빈 줄 하나)으로 잇는다. 블록 안의 줄바꿈은 그대로 둔다.
- 원문은 캡처한 그대로 쓴다.
- 언어 라벨은 붙이지 않는다.
- 옵션이 꺼져 있고 번역이 하나면 결과는 번역문 그대로다(v5와 같다).

예 (Base=ko, Target=[en, ja], 원문 포함 켜짐):

```
안녕하세요. 내일 회의해요.

Hello. Let's meet tomorrow.

こんにちは。明日会議しましょう。
```

입력이 Target 언어(예: 영어)면 결과는 `[원문] + 한국어 번역` 하나다.

### 2.6 적용 위치

| 기능 | 다중 Target | 원문 포함 |
|---|---|---|
| Alt+Alt (Chat/Gmail/Slides/Docs) | `compose_replacement` 결과로 교체 | 적용 |
| Ctrl+Ctrl 창 | 문장마다 원문 아래 번역을 Target 순서로 표시 | 해당 없음 |
| Sheets Alt+Alt (범위·편집 셀) | 셀마다 `compose_replacement`. 셀 안 줄바꿈으로 씀 | 적용 |
| Sheets Ctrl+Ctrl 표 | 셀 위치 유지, 셀마다 언어별 번역을 순서대로 | 해당 없음 |
| 한글 교정 (Ctrl+Alt+Alt / Alt+Ctrl+Ctrl) | 영향 없음 | 적용 안 함 |

Ctrl+Ctrl 응답 형식:

```json
{"sentences": [{"original": "...", "translations": {"en": "...", "ja": "..."}}]}
```

- 문장마다 2.3과 같은 키 검증을 한다.
- 사전 토큰 검증은 v5 D14 규칙을 언어별로 적용한다. 전체 문장을 합친 기준으로 원문 칸과 각 언어 칸에 토큰이 정확히 한 번씩 있어야 한다.

Sheets 배치 응답 형식:

```json
{"translations": [{"id": "c1", "texts": {"en": "...", "ja": "..."}}]}
```

- id 누락, 중복, 추가가 있으면 실패한다. 각 항목의 언어 키는 2.3처럼 검증한다.
- 셀마다 감지 언어가 다를 수 있다. 따라서 항목마다 `destinations`를 따로 지정한다(base 셀은 Target 전체, Target 언어 셀은 base만).

### 2.7 한도

- Sheets 묶음 글자 수는 `입력 글자 수 × (해당 항목의 목적 언어 수 + 원문 포함이면 1)`로 계산해 `SHEETS_BATCH_MAX_CHARS`와 비교하고 묶음을 나눈다.
- 조립된 셀 내용이 `SHEETS_MAX_CELL_CHARS`를 넘으면 쓰기 전에 그 작업 전체를 중단하고 안내한다.
- CLI 타임아웃(기본 60초)은 그대로 둔다. 언어가 3개일 때 걸리는 시간은 수동 테스트로 기록한다.

### 2.8 진행 표시

"Translating..." 그대로 둔다.

## 3. 한글 교정 "문장 다듬기" 강화

오탈자 교정(spelling) 모드는 v5와 같다. 오탈자와 띄어쓰기만 고치고, 단어 선택·어순·어투·높임·의미는 바꾸지 않는다.

다듬기(polish) 모드 지시문은 다음 내용을 담는다.

1. 오탈자와 띄어쓰기를 고친다.
2. 같은 내용을 자연스럽고 읽기 쉬운 한국어로 **다시 쓴다**. 의미와 사실이 같으면 문장을 처음부터 다시 써도 된다. 어순, 구조, 단어 선택을 모두 바꿀 수 있고, 문장을 합치거나 나눌 수 있으며, 길이도 달라져도 된다. 고칠 곳이 거의 없어 보여도 더 자연스러운 표현이 있으면 바꾼다.
3. 유지할 것:
   - 의미와 사실(정보 추가·삭제 금지)
   - 높임 수준과 문체(합니다체 / 해요체 / 반말)
   - 문단과 줄바꿈 구조
   - 한국어가 아닌 단어
   - `⟦N⟧` 토큰
4. 원문에 없던 강조어, 과장 수식어, 감정 표현을 새로 넣지 않는다. 업무 메일·채팅에 맞는 담백하고 평이한 단어를 쓴다. 금지 예시(원문에 이미 있으면 그대로 둔다):
   > 혁신적인, 획기적인, 압도적인, 놀라운, 탁월한, 완벽한, 극대화, 최적화된, 시너지, 패러다임, 여정, 도약, 게임 체인저, 핵심적인 역할, 필수적인, 원활한, 한층 더, 매우 중요한, 진정한, 깊이 있는

5. 사람이 쓴 업무 메시지처럼 쓴다(2026-11-21 사용자 요청으로 추가).
   - 이모지·이모티콘, 원문에 없던 느낌표를 넣지 않는다.
   - 과장된 수식어와 지나치게 확신하는 표현을 쓰지 않는다. 원문의 추측·유보 표현은 그대로 둔다.
   - 원문에 없던 요약·결론 문장을 붙이지 않는다.
   - 금지 예시(원문에 이미 있으면 그대로 둔다): 반드시, 확실히, 분명히, 틀림없이, 의심의 여지 없이, 단언컨대, 100%, 결론적으로, 요약하자면

프롬프트 종류별 차이:

- 단일 교정과 Sheets 배치(항목 내부): 문장 합치기·나누기를 적극적으로 한다. 짧게 끊긴 문장은 합치고, 길거나 늘어진 문장은 나누고, 더 자연스러우면 절의 순서를 바꾼다(뜻이 같은 한도 안에서). 배치는 항목끼리 합치거나 나누거나 순서를 바꿀 수 없다(v5와 같다).
- 문장별 보기(Alt+Ctrl+Ctrl): 원문 한 문장당 교정문 한 문장을 낸다. 각 문장 안에서는 전체를 다시 써도 된다.

금지 단어는 프롬프트로만 제어한다. 결과에 섞여 나와도 코드에서 막거나 다시 요청하지 않는다(속도 우선).

교정의 나머지 규칙은 v5와 같다. 실행 조건(base가 ko, 마스킹 후 주 언어가 ko), 변경 없음 안내, 사전 복원(사용자가 쓴 원래 표기), 사용자 메시지 문구, 진행 문구("한국어 교정 중..." / "한국어 수정 중...")가 여기에 해당한다.

## 4. 구조

### 4.1 Cargo 워크스페이스

| 크레이트 | 역할 | Windows 의존 |
|---|---|---|
| `ct-core` | 설정 모델·저장, 개인 사전 로드·저장·마스킹·복원, 언어 판별, `resolve_plan`, `compose_replacement`, `LlmProvider` trait과 요청·응답 타입(서비스가 쓰므로 core에 둔다. 순환 의존 방지), 번역·설명·교정 서비스, 교정 규칙, 파일 문제 수집 | 없음 |
| `ct-llm` | `LlmProvider` 구현: `MockProvider`, `ClaudeCliProvider`(프롬프트, JSON 검증), `ClaudeCliClient`(상주 프로세스, stream-json, 타임아웃, `/clear` 주기, 재시작, `initialize` 모델 조회), 모델 카탈로그·캐시, CLI 경로 탐색 | 없음(프로세스 실행만) |
| `ct-sheets` | 셀 모델, 클립보드 HTML 파서, 지원 판정, 교체 페이로드, 쓰기 후 검증, 결과 분류, 라우팅 | 없음 |
| `ct-win` | 저수준 키보드 훅(F15 주입), 코드 라우터(Alt+Alt / Ctrl+Ctrl / 교차 조합, 키 반복 무시, 변경자 상태 규칙 D7), UIA 캡처·교체, Chrome 앱별 전략, 역할 분류, 클립보드(다중 포맷 보존·복원), Sheets 백엔드, 레지스트리 | 있음 |
| `ct-app` (bin) | 조립, 트레이, Slint 창, 작업 디스패치 | 있음 |

- `ct-core`, `ct-llm`, `ct-sheets`는 Windows 없이 빌드되고 테스트된다.
- `ct-win`의 판단 로직(역할 분류, 전략 선택, 코드 판정)은 순수 함수로 분리해 단위 테스트한다. 실제 OS 호출부는 얇게 둔다.
- 앱 코드는 Claude CLI wire format을 직접 알지 않는다. `ClaudeCliProvider`는 `ClaudeCliClient`를 trait으로만 참조해서 가짜 클라이언트로 테스트한다.

### 4.2 주요 라이브러리

- UI: `slint`, 트레이: `tray-icon`(+ `muda` 메뉴)
- Win32/COM: `windows` 크레이트 (UIA는 `Win32_UI_Accessibility`)
- 채널: `crossbeam-channel`, 직렬화: `serde`, `serde_json`, `serde_yaml`
- 오류: `thiserror`(라이브러리), `anyhow`(앱 조립부만)
- 로그: `tracing`, `tracing-subscriber`, `tracing-appender`(일자별 회전)
- `.env`: `dotenvy`, 리소스: `winres`

### 4.3 스레드 모델

tokio를 쓰지 않는다. `std::thread`와 채널로 구성한다.

- **메인(UI) 스레드**: Slint 이벤트 루프와 트레이를 같은 Win32 메시지 루프에서 돌린다. 다른 스레드는 `slint::invoke_from_event_loop`로만 UI를 바꾼다.
- **훅 스레드**: `WH_KEYBOARD_LL`과 전용 `GetMessage` 루프를 둔다. 콜백은 판정만 하고 즉시 반환하며, 판정된 제스처를 작업 채널로 보낸다. Pause 상태에서는 제스처를 보내지 않는다.
- **작업 스레드 1개**: 캡처, LLM, 교체를 순서대로 처리한다. 시작할 때 COM을 초기화한다. 작업 중에 들어온 제스처는 버린다(중복 실행 방지).
- **CLI**: `ClaudeCliClient`는 Mutex로 한 번에 한 turn만 처리한다. stdout reader 스레드가 이벤트를 채널로 넘기고, `recv_timeout`으로 대기 시간을 제한한다. 모델 목록 조회는 시작 직후 백그라운드 스레드에서 실행하고, 결과는 카탈로그 리스너로 열려 있는 Settings에 반영한다.

### 4.4 데이터와 설정

- 폴더: `%APPDATA%\ClaudeTranslator\`
  - `settings.yaml`, `user.yaml`(개인 사전), `user.yaml.bak`, `models_cache.json`, `logs\`
- 개인 사전 형식은 v5 `user.yaml`과 같다.

  ```yaml
  entries:
    - term: DA
      action: KEEP
    - term: logical model
      action: TRANSLATE
      translations: { ko: 논리 모델, en: logical model, vi: mô hình logic, ja: 論理モデル }
  ```

- 같은 표기가 두 항목에 있으면 파일에서 앞에 있는 항목이 이긴다.
- 매칭 규칙(v5와 같음):
  - KEEP은 `term`, TRANSLATE는 `term`과 모든 번역 값을 매칭한다.
  - 라틴 문자 표기는 단어 경계에서만 매칭하고, 한글 가장자리는 제한하지 않는다.
  - 대소문자를 무시하고 매칭한다. 원문 표기는 `original_text`로 보존한다.
  - 텍스트를 왼쪽부터 훑으면서 각 위치에서 가장 긴 표기를 먼저 시도한다. 길이가 같으면 파일에서 앞에 있는 항목이 이긴다(v5 `_candidate_forms`와 같다).
- 깨진 파일 처리:
  - 읽을 수 없는 `user.yaml`은 빈 사전으로 시작한다.
  - 잘못된 항목은 건너뛴다.
  - 문제가 있었으면 다음 저장 전에 `.bak`으로 복사한다.
  - 시작 직후 파일 이름과 건너뛴 개수만 경고 창으로 한 번 알린다.
- 개인 사전 창에서 저장하면 다음 번역부터 바로 반영된다.
- `.env`와 환경변수 키는 v5 이름을 유지한다.
  - `LLM_PROVIDER`(claude_cli|mock)
  - `CLAUDE_CLI_PATH`, `CLAUDE_CLI_MODEL`, `CLAUDE_CLI_EFFORT`
  - `CLAUDE_CLI_TIMEOUT_SECONDS`(60), `CLAUDE_CLI_CLEAR_EVERY_N_TURNS`(20)
  - `LOG_LEVEL`
  - `SHEETS_RANGE_REPLACE_ENABLED`, `SHEETS_EDIT_FULL_CELL_REPLACE_ENABLED`(기본 켜짐)
  - `SHEETS_BATCH_MAX_CELLS`, `SHEETS_BATCH_MAX_CHARS`, `SHEETS_MAX_CELL_CHARS`, `SHEETS_MAX_RANGE_CELLS`, `SHEETS_JOB_TIMEOUT_SECONDS`
- Sheets 묶음 한도 기본값은 v5 `SheetsConfig` 값을 그대로 쓴다.
- `.env` 탐색 위치: exe 폴더, 그다음 데이터 폴더.

### 4.5 Claude CLI

v5와 같다.

- 실행 인자: `claude -p --input-format stream-json --output-format stream-json --verbose --tools ""` (+ `--model`, `--effort`)
- 상주 프로세스를 재사용하고, N회마다 `/clear`를 보낸다. `/clear`의 끝은 "다음 result 이벤트 하나"로 판단한다.
- 작업 폴더는 데이터 폴더로 고정하고, UTF-8로 통신한다.
- 응답 대기 시간을 제한하고, 프로세스가 죽으면 그 호출은 실패 처리한 뒤 다음 호출에서 재시작한다.
- effort를 지원하지 않는 모델에는 `--effort`를 넘기지 않는다.
- 모델 목록 우선순위: 방금 조회한 결과 > 캐시 > 하드코딩 폴백(opus/sonnet/haiku).
- "이전 대화를 무시하라" 같은 지시문은 쓰지 않는다. 조직 정책이 이를 프롬프트 인젝션으로 보고 거부한다(v5 실측).

## 5. UI

화면 구성과 문구는 v5를 그대로 옮긴다(앱 이름만 `ClaudeTranslator`). 바뀌는 곳은 아래와 같다.

### 5.1 트레이

- 툴팁: `ClaudeTranslator`
- Target Language 하위 메뉴는 체크박스다. base를 뺀 언어를 보여주고, 체크된 언어에는 순서 번호를 붙인다(예: `en (1)`, `ja (2)`).
- 새로 체크한 언어는 순서 맨 끝에 붙고, 체크를 풀면 목록에서 빠진다.
- 마지막 남은 하나는 체크를 풀 수 없다(무시).
- 변경은 즉시 저장된다.

### 5.2 Settings

- **Target Languages**: base를 뺀 언어마다 체크박스와 ▲/▼ 버튼을 둔다. 체크된 항목이 위에 순서대로 놓이고, 그 순서가 출력 순서다. 하나도 체크되지 않으면 Save를 비활성화하고 "Target 언어를 하나 이상 선택하세요."를 보여준다.
- Base Language를 바꾸면 그 언어는 Target 목록에서 빠진다. 그 결과 Target이 비면 2.1 규칙으로 하나를 체크한다.
- **새 옵션**: "Alt+Alt 교체 시 원문 포함" 체크박스. 설명: "켜면 입력한 원문 아래에 빈 줄을 두고 번역문을 넣습니다."
- 나머지는 v5와 같다.
  - 간격, Start on Login, CLI 경로
  - Ctrl+Ctrl 표시 방식, 번역 어투
  - Model/Effort: 목록이 도착하면 열린 창에도 반영, 폴백 안내, haiku 경고, effort 미지원이면 비활성화
  - 한글 교정 옵션
  - 단축키 안내 카드(Docs 블록 선택 필요 문구 포함)
  - Save를 누르면 저장하고 닫기
- 단축키 안내 카드에 "Target 언어가 여러 개면 정한 순서대로 모두 번역합니다." 한 줄을 추가한다.
- 조직 사전 관련 문구는 없다.

### 5.3 Ctrl+Ctrl 창

- v5처럼 모든 창보다 앞에 뜨고, 내용에 맞춰 크기를 조절하며(화면 크기 이내), 스크롤과 Esc 닫기를 지원한다. 라벨 텍스트 없이 굵기·색으로 구분한다.
- whole_text: 원문 블록(굵게) 아래에 Target 순서대로 번역 블록을 둔다.
- per_sentence: 문장 카드마다 원문(굵게) 아래에 언어별 번역을 순서대로 둔다.
- 번역이 2개 이상이면 각 번역 앞에 작은 회색 언어 표시(`EN`, `JA` 등)를 붙인다. 1개면 붙이지 않는다. 이 표시는 창 안에서만 보이고 교체 텍스트에는 들어가지 않는다.
- Sheets 표: 셀 위치를 유지하고, 셀마다 같은 규칙으로 보여준다.
- 교정 보기는 v5 그대로 원문과 교정문을 보여준다.

### 5.4 그 밖의 창

- 개인 사전 창: v5와 같다(추가/수정/삭제, KEEP/TRANSLATE, ko/en/vi/ja 번역).
- 커서 배지: 테두리 없는 작은 창. `WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW`로 포커스를 가져가지 않는다.
- 메시지 창: v5와 같은 문구로 맨 앞에 띄운다.
- 창 스타일은 HWND(`raw-window-handle`)를 얻어 `SetWindowPos`와 `SetWindowLongPtr`로 조정한다.

## 6. 오류 처리와 안전 규칙

- 모든 실패는 `Result`로 올린다. 최종 교체 전 어느 단계에서 실패하든 원문을 수정하지 않는다.
- 테스트 밖에서는 `unwrap`/`expect`를 쓰지 않는다. 작업 스레드는 각 작업을 `catch_unwind`로 감싸고, panic이 나면 실패 안내와 로그를 남긴 뒤 계속 동작한다. 훅 콜백에서는 할당이 크거나 panic 가능성이 있는 코드를 실행하지 않는다.
- 클립보드:
  - 기존 데이터(여러 포맷)를 보존한다.
  - 대기 시간을 제한한다.
  - 안전할 때만 복원한다.
  - 그 사이 사용자가 새로 복사했으면 복원하지 않고 보존한다.
- Enter, Ctrl+Enter, 전송, 폼 제출, 페이지 이동을 합성하지 않는다.
- 대상 편집 영역이 명확하지 않으면 광범위한 Ctrl+A 교체를 하지 않는다.
- Sheets:
  - 한 번 일괄로 쓴 뒤 다시 읽어 검증한다.
  - 재시도하지 않고, 자동 Undo도 하지 않는다.
  - 쓰기 전에 대상이 같은지 다시 확인한다.
  - 불확실하면(숨김·필터·주소 불명) 안내만 하고 중단한다.
- 로그에는 원문, 번역문, 교정문, 메일·채팅·문서 내용을 남기지 않는다. 시각, 작업 종류, 소요 시간, 글자 수, 오류 종류만 남긴다.
- 사용자 메시지 문구는 v5 표(CLAUDE.md "한글 교정" 절)를 따른다.

## 7. 테스트

### 7.1 단위 테스트 (`cargo test --workspace`)

v5 `tests/unit`의 케이스를 같은 입력과 기대값으로 옮긴다. 대상은 다음과 같다.

- 설정 정규화, 사전 로드(깨진 항목), 마스킹·복원·손상 감지
  - 중복, 부분 중첩, 대소문자, 다단어, 문장부호, Unicode, 4개 언어, 반복
- 언어 판별(한/영 혼합 예: "DA에서 logical model 확인하고 CLT infrastructure에 적용해주세요.")
- JSON 파싱, CLI wire format, 가짜 프로세스로 타임아웃·재시작·`/clear` 주기, 모델 카탈로그
- Sheets HTML 파서, 지원 판정, 페이로드, 검증, 코드 라우터(D7), 역할 분류, 전략 선택

새로 추가하는 테스트:

- `resolve_plan`: base 입력, Target 입력, 기타 언어, 판별 실패, Target 1개일 때 v5와 같은지
- `compose_replacement`: 원문 포함 켜짐/꺼짐, 번역 1~3개, 끝 줄바꿈과 `\r\n`, 블록 안 줄바꿈 유지
- 다중 언어 응답 검증: 누락, 추가, 빈 값, 문자열 아님
- 언어별 사전 복원, 한 언어의 토큰 손상 시 전체 실패
- Ctrl+Ctrl 다중 언어 문장 검증, Sheets 배치 항목별 destinations와 한도 계산, 셀 최대 글자 초과 시 중단
- 다듬기 프롬프트: 재작성 허용, 높임 유지, 금지 단어 목록 포함, 프롬프트 종류별 합치기/나누기 문구
- 트레이 Target 토글 규칙(끝에 추가, 마지막 하나 유지)과 Settings 순서 변경 로직(UI와 분리한 순수 함수)

### 7.2 통합 테스트 (opt-in, `RUN_CLAUDE_CLI_LIVE_TESTS=1`)

실제 CLI로 다음을 확인한다.

- 단일 번역, 다중 언어 번역(ko → en, ja)과 `⟦N⟧` 보존
- 설명
- 교정(spelling, polish): polish는 해요체 유지, 원문과 충분히 다른 결과, 원문에 없던 금지 단어 없음
- 모델 목록 조회

LLM 결과는 매번 달라지므로 통과는 "그 실행에서 관찰됨"으로만 보고한다.

### 7.3 수동 테스트 (`tests/manual/*.md`)

v5 절차를 옮기고 다음을 추가한다.

- 앱별(Chat, Gmail, Slides, Docs 블록, Sheets 범위·편집 셀)로 다중 Target(1개/3개), 순서 변경 반영, 원문 포함 켜짐/꺼짐, Target 언어 입력 시 base로만 번역되는지
- Ctrl+Ctrl 창의 다중 언어 표시(whole_text / per_sentence / Sheets 표)
- 트레이 체크 메뉴와 Settings 순서 변경
- 다듬기 모드 결과 확인
- 언어 3개일 때 응답 시간 기록

실제로 해보지 않은 항목은 검증되었다고 적지 않는다.

### 7.4 단계 완료 조건

`cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`가 통과해야 한다.

## 8. 패키징

- `cargo build --release` → `ClaudeTranslator.exe` 하나.
  - `windows_subsystem = "windows"`
  - 아이콘과 버전 정보 리소스 포함
  - 정적 CRT(`+crt-static`)
- 설치 프로그램은 만들지 않는다. exe 하나로 배포한다.
- 사용자 가이드(`docs/USER_GUIDE.md`) 맨 앞에 전제 조건을 적는다: Claude Code CLI 설치, `claude login`. v5와 동시에 실행하지 말라는 안내도 넣는다.
- 자동 시작: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\ClaudeTranslator`(경로는 따옴표로 감쌈). release exe일 때만 쓰고, 실패해도 로그만 남긴다.

## 9. 구현 단계

각 단계가 끝나면 빌드와 7.4 조건이 통과하는 상태를 유지한다.

1. **기반 스파이크**: 워크스페이스 구성. Slint 창과 트레이를 함께 실행한다. 맨 앞에 띄우기와 포커스를 뺏지 않는 배지를 확인한다. 저수준 훅 스레드와 UIA COM 호출이 함께 동작하는지 확인한다. 결과는 `tests/manual/spike.md`에 기록한다.
2. **ct-core**: 설정, 개인 사전, 마스킹, 언어 판별, `resolve_plan`, `compose_replacement`, 서비스 (TDD)
3. **ct-llm**: CLI 클라이언트, 프로바이더, 다중 언어 프롬프트·검증, 다듬기 강화, 모델 카탈로그·캐시, Mock
4. **ct-win (텍스트)**: 훅, 코드 라우터, F15 처리, 클립보드, UIA, Chrome 전략(Chat/Gmail/Slides/Docs)
5. **ct-app 1차**: 트레이, Settings, 개인 사전 창, Alt+Alt / Ctrl+Ctrl 연결, 배지, 메시지 창. 첫 수동 테스트(다중 Target, 원문 포함)
6. **한글 교정**: Ctrl+Alt+Alt / Alt+Ctrl+Ctrl 연결, 교정 보기 창
7. **Google Sheets**: `ct-sheets`, Windows Sheets 백엔드, 다중 Target과 원문 포함 적용
8. **마무리**: 자동 시작, 시작 경고, release 빌드, 사용자 가이드, 수동 테스트 문서 정리
