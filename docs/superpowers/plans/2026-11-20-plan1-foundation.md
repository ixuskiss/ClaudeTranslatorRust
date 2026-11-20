# ClaudeTranslator 계획 1: 기반 (스파이크 + ct-core + ct-llm)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Windows UI 없이 테스트가 모두 도는 번역 코어(설정, 개인 사전, 마스킹, 언어 판별, 다중 Target 계획, 출력 조립, 번역·설명·교정 서비스)와 Claude CLI LLM 백엔드를 Rust로 만든다. 그리고 Slint/트레이/훅/UIA를 함께 쓸 수 있는지 스파이크로 확인한다.

**Architecture:** Cargo 워크스페이스.
- `ct-core`: 순수 로직, 그리고 `LlmProvider` trait 같은 포트.
- `ct-llm`: Claude CLI 서브프로세스와 프롬프트로 그 trait을 구현한다.
- `ct-app`: 이번 계획에서는 스파이크 예제만 있다.

v5(Python) 동작을 그대로 옮기고, 다중 Target, 원문 포함 조립, 다듬기 강화만 새로 넣는다.

**Tech Stack:** Rust 1.100 (edition 2024), serde / serde_json / serde_yaml, thiserror, tracing, dotenvy, crossbeam-channel. 스파이크 전용: slint, tray-icon, windows.

**Spec:** `docs/superpowers/specs/2026-11-20-claude-translator-rust-design.md`

**v5 원본(읽기 전용):** `C:\python_workspace\ClaudeLangTrans_v5`. "v5 `X` 포팅"이라고 적힌 작업은 해당 파일의 동작과 테스트 케이스를 같은 입력·기대값으로 옮긴다는 뜻이다. v5 파일은 절대 수정하지 않는다.

**후속 계획(이 문서 범위 밖):**
- 계획 2: `ct-win`(훅, 코드 라우터, 클립보드, UIA, Chrome 전략)과 `ct-app` 1차(트레이, Settings, 개인 사전 창, Alt+Alt / Ctrl+Ctrl, 배지)
- 계획 3: 한글 교정 단축키 연결
- 계획 4: Google Sheets(배치 LLM 메서드 포함)와 마무리

## Global Constraints

- 앱 이름은 `ClaudeTranslator`, 데이터 폴더는 `%APPDATA%\ClaudeTranslator`(APPDATA가 없으면 `~/.claudetranslator`).
- 언어 코드는 `ko`, `en`, `vi`, `ja` 네 개만 쓴다. 프롬프트와 JSON 키에도 이 소문자 코드를 쓴다.
- 사전 파일은 `user.yaml` 하나뿐이다. 최상위는 `entries:` 목록이다. company/team 사전 코드는 만들지 않는다.
- 테스트 밖 코드에서는 `unwrap`/`expect`를 쓰지 않는다. 각 크레이트 `lib.rs`에 `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]`를 둔다.
- 로그(`tracing`)에는 원문, 번역문, 교정문, 프롬프트 내용을 남기지 않는다. 작업 종류, 상태, 글자 수, 소요 시간, 오류 종류만 남긴다.
- 사용자 메시지와 파일 문제 메시지는 이 계획에 적힌 한국어 문구를 글자 그대로 쓴다.
- `.env` 키는 v5 이름 그대로다: `LLM_PROVIDER`, `CLAUDE_CLI_PATH`, `CLAUDE_CLI_MODEL`, `CLAUDE_CLI_EFFORT`, `CLAUDE_CLI_TIMEOUT_SECONDS`(60), `CLAUDE_CLI_CLEAR_EVERY_N_TURNS`(20), `LOG_LEVEL`, `SHEETS_*`.
- 단위 테스트는 인터넷, 실제 Claude CLI, Windows UI 없이 돌아야 한다. 실제 CLI 테스트는 `RUN_CLAUDE_CLI_LIVE_TESTS=1`일 때만 실행한다.
- 각 작업 완료 조건: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check` 통과.
- 커밋 메시지는 v5처럼 `feat(core): ...` 형식의 한국어로 쓰고, 끝에 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`를 붙인다.

## Review Focus

1. **소문자 변환 시 길이가 바뀌는 문자**(예: `İ` → `i̇`, 두 글자)가 들어간 텍스트를 마스킹할 때 위치가 어긋나 엉뚱한 부분이 치환되면 안 된다. 테스트: Task 6의 `mask_handles_chars_whose_lowercase_changes_length`.
2. **Windows에서 캡처된 `\r\n` 줄바꿈**은 조립 결과에서 블록 끝만 정리되고 블록 안 줄바꿈은 그대로여야 한다. 테스트: Task 8의 `compose_trims_crlf_only_at_block_end`.
3. **공백이나 빈 텍스트**를 번역하라고 하면 LLM을 부르지 않고 방향 판별 실패로 끝나야 한다. 테스트: Task 10의 `translate_blank_text_fails_without_calling_llm`.
4. **손으로 고친 settings.yaml**에서 `target_languages: en`처럼 목록 대신 문자열 하나를 쓰면 `[en]`으로 읽혀야 한다(앱이 기본값으로 돌아가 버리면 안 됨). 테스트: Task 3의 `load_accepts_single_string_target`.
5. **Claude CLI가 설치되지 않아 프로세스를 띄울 수 없으면** 패닉 없이 `CliError::Spawn`을 내야 하고, 다음 호출에서 다시 띄워 봐야 한다. 테스트: Task 14의 `spawn_failure_is_error_and_next_call_retries`.

---

## 파일 구조

```
Cargo.toml                         워크스페이스 (resolver 3, edition 2024)
.gitignore                         (이미 있음: /target/, .env, *.log)
crates/ct-core/src/
  lib.rs                           모듈 선언, lint 설정
  lang.rs                          Lang
  settings.rs                      UserSettings, 열거형, 정규화, Target 토글·이동, SettingsStore
  config.rs                        AppConfig, ClaudeCliConfig, SheetsConfig, env 로드
  paths.rs                         user_data_dir
  file_problems.rs                 시작 경고 문구
  dictionary/mod.rs                재수출
  dictionary/model.rs              Action, DictionaryEntry
  dictionary/loader.rs             load_dictionary_file, save_dictionary_file
  dictionary/store.rs              PersonalDictionaryStore
  dictionary/protector.rs          mask, restore, 무결성 검증
  detector.rs                      LanguageDetector, RuleBasedDetector
  plan.rs                          TranslationPlan, resolve_plan
  compose.rs                       compose_replacement
  llm.rs                           LlmProvider trait, 요청·응답 타입, LlmError, parse_json_response
  translate.rs                     translate_text
  explain.rs                       explain_text, restore_sentence_columns
  correction.rs                    correct_text, correct_sentences, CorrectionStatus
  test_support.rs                  #[cfg(test)] FakeLlm
crates/ct-llm/src/
  lib.rs
  prompts.rs                       모든 프롬프트 빌더, POLISH_BANNED_WORDS
  provider.rs                      CliTransport trait, ClaudeCliProvider
  spawner.rs                       CliSpawner trait, RealSpawner
  client.rs                        ClaudeCliClient, CliError, build_command
  models.rs                        ModelInfo, parse_model_info
  catalog.rs                       ModelCatalog, fallback_models
  locator.rs                       resolve_claude_cli_path
  mock.rs                          MockProvider
  factory.rs                       build_llm_provider
crates/ct-llm/tests/live_cli.rs    opt-in 실제 CLI 테스트
crates/ct-app/src/main.rs          빈 진입점(계획 2에서 채움)
crates/ct-app/examples/spike.rs    기반 스파이크
tests/manual/spike.md              스파이크 수동 확인 절차와 결과표
```

---

### Task 1: 워크스페이스 뼈대

**Files:**
- Create: `Cargo.toml`, `crates/ct-core/Cargo.toml`, `crates/ct-core/src/lib.rs`, `crates/ct-llm/Cargo.toml`, `crates/ct-llm/src/lib.rs`, `crates/ct-app/Cargo.toml`, `crates/ct-app/src/main.rs`

**Interfaces:**
- Produces: 크레이트 이름 `ct-core`, `ct-llm`, `ct-app`. `ct-llm`은 `ct-core`에 의존한다.

- [ ] **Step 1: 워크스페이스를 만든다.** 루트 `Cargo.toml`에 `[workspace] members = ["crates/*"]`, `resolver = "3"`, `[workspace.package] edition = "2024"`, `rust-version = "1.100"`을 둔다. `[workspace.dependencies]`에는 `serde`(derive), `serde_json`, `serde_yaml`, `thiserror`, `tracing`, `dotenvy`, `crossbeam-channel`을 `cargo add`로 받은 최신 버전으로 넣는다. `ct-app`의 `main.rs`는 `fn main() {}`만 둔다.
- [ ] **Step 2: 빌드와 검사를 확인한다.** `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`를 실행해 모두 성공하는지 본다.
- [ ] **Step 3: 커밋한다.** `chore: Cargo 워크스페이스 뼈대`

---

### Task 2: 기반 스파이크 (Slint + 트레이 + 배지 + 훅 + UIA)

스펙 9장 1단계. 계획 2의 설계가 기대는 다섯 가지 가정을 실제 Windows에서 먼저 확인한다. 코드는 `examples/`에 두는 확인용이며, 계획 2에서 정식 코드로 다시 작성한다.

**Files:**
- Create: `crates/ct-app/examples/spike.rs`, `crates/ct-app/ui/spike.slint`, `tests/manual/spike.md`
- Modify: `crates/ct-app/Cargo.toml`. `[dev-dependencies]`에 `slint`(feature `raw-window-handle-06`), `tray-icon`, `windows`를 넣는다. `windows` feature는 `Win32_Foundation`, `Win32_UI_WindowsAndMessaging`, `Win32_UI_Input_KeyboardAndMouse`, `Win32_System_LibraryLoader`, `Win32_System_Com`, `Win32_UI_Accessibility`, `Win32_System_Threading`이다.

**Interfaces:**
- Produces: `tests/manual/spike.md`의 결과표 S1~S5. 계획 2를 쓰기 전에 사용자가 채운다.

- [ ] **Step 1: `spike.rs`를 작성한다.** 아래 다섯 가지를 한 프로그램에서 확인한다.
  - **S1**: Slint 메인 창과 `tray-icon` 트레이(메뉴: "Show badge", "Show front window", "Exit")가 같은 메인 스레드 이벤트 루프에서 동작한다. 트레이 이벤트는 `TrayIconEvent::receiver()`/`MenuEvent::receiver()`를 `slint::Timer`(50ms)로 폴링해서 처리한다.
  - **S2**: "Show badge"를 누르면 3초 뒤에 테두리 없는 작은 Slint 창("Translating...")을 커서 근처에 띄운다. HWND는 `slint::Window::window_handle()`에서 `raw-window-handle`로 얻고, `SetWindowLongPtrW(GWL_EXSTYLE)`에 `WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW`를 더한 뒤 `SetWindowPos(HWND_TOPMOST, SWP_NOACTIVATE …)`로 띄운다.
  - **S3**: "Show front window"를 누르면 3초 뒤(그 사이 사용자는 다른 앱을 클릭)에 일반 Slint 창을 띄워 맨 앞으로 가져온다. `AllowSetForegroundWindow`, `SetForegroundWindow`를 쓰고, 실패하면 `AttachThreadInput` 방식을 시도한다. 어느 방법이 통했는지 표준출력에 찍는다.
  - **S4**: 별도 스레드에서 `SetWindowsHookExW(WH_KEYBOARD_LL)`와 `GetMessageW` 루프를 돌린다. Alt를 200ms 안에 두 번 눌렀다 떼면 표준출력에 `ALT_ALT`를 찍는다. 키 반복(누른 채 유지)은 무시한다. 콜백은 항상 `CallNextHookEx`를 반환한다.
  - **S5**: 작업 스레드에서 `CoInitializeEx(COINIT_MULTITHREADED)` 후 `CUIAutomation`을 만든다. S4의 `ALT_ALT`를 채널로 받을 때마다 `GetFocusedElement`의 `CurrentName` 길이와 `CurrentControlType`을 찍는다. 이름 내용은 찍지 않는다.
- [ ] **Step 2: `tests/manual/spike.md`를 작성한다.** S1~S5 각각에 대해 실행 명령(`cargo run -p ct-app --example spike`), 조작 절차, 기대 결과, 빈 결과 칸(통과/실패/메모)을 적는다. 기대 결과는 다음과 같다.
  - S2: 메모장에 글자를 치는 중에 배지가 떠도 입력이 메모장에 계속 들어간다.
  - S3: 다른 앱이 앞에 있어도 창이 맨 앞에 뜬다(작업 표시줄 깜빡임만으로는 실패).
  - S4: Chrome에서 Alt+Alt를 눌렀을 때 `ALT_ALT`가 한 번만 찍힌다.
  - S5: Chrome Gmail 본문에 포커스가 있을 때 control type이 찍힌다.
- [ ] **Step 3: 빌드를 확인한다.** `cargo build -p ct-app --examples`가 성공하고 clippy와 fmt도 통과하는지 본다. 실제 동작은 터미널만으로 확인할 수 없으므로 "검증했다"고 보고하지 않는다. 사용자에게 `tests/manual/spike.md` 실행을 요청한다.
- [ ] **Step 4: 커밋한다.** `chore(spike): Slint·트레이·배지·훅·UIA 기반 스파이크`

---

### Task 3: Lang과 사용자 설정

**Files:**
- Create: `crates/ct-core/src/lang.rs`, `crates/ct-core/src/settings.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum Lang { Ko, En, Vi, Ja }            // Copy, Eq, Hash, Debug, Ord
  impl Lang {
      pub const ALL: [Lang; 4];               // [Ko, En, Vi, Ja]
      pub fn code(self) -> &'static str;       // "ko" ...
      pub fn from_code(code: &str) -> Option<Lang>; // 앞뒤 공백 제거, 대소문자 무시
  }
  pub enum DisplayMode { WholeText, PerSentence }      // "whole_text" | "per_sentence"
  pub enum Tone { Neutral, Casual, Formal, Plain }     // "neutral" | "casual" | "formal" | "plain"
  pub enum CorrectionMode { Spelling, Polish }         // "spelling" | "polish"
  // 세 열거형 모두 fn as_str(self) -> &'static str 와 fn parse(&str) -> Option<Self>
  impl CorrectionMode { pub fn loading_message(self) -> &'static str } // "한국어 교정 중..." | "한국어 수정 중..."
  pub struct UserSettings { /* 스펙 2.1 필드 그대로, 기본값도 스펙 2.1 */ }
  pub fn normalize_targets(base: Lang, raw: &[Lang]) -> Vec<Lang>;
  pub fn toggle_target(base: Lang, targets: &[Lang], lang: Lang) -> Vec<Lang>;
  pub fn move_target(targets: &[Lang], lang: Lang, up: bool) -> Vec<Lang>;
  pub fn with_base(settings: &UserSettings, base: Lang) -> UserSettings; // base 변경 + targets 재정규화
  pub struct SettingsStore { /* path, settings */ }
  impl SettingsStore {
      pub fn load(path: &Path, problems: &mut Vec<String>) -> SettingsStore;
      pub fn get(&self) -> &UserSettings;
      pub fn update(&mut self, settings: UserSettings) -> std::io::Result<()>;
  }
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다** (`settings.rs` 안 `#[cfg(test)] mod tests`).
  ```rust
  #[test] fn normalize_drops_base_duplicates_and_keeps_order() {
      assert_eq!(normalize_targets(Lang::Ko, &[Lang::Ja, Lang::Ko, Lang::En, Lang::Ja]), vec![Lang::Ja, Lang::En]);
  }
  #[test] fn normalize_empty_falls_back_to_first_non_base() {
      assert_eq!(normalize_targets(Lang::Ko, &[]), vec![Lang::En]);
      assert_eq!(normalize_targets(Lang::En, &[Lang::En]), vec![Lang::Ko]);
  }
  #[test] fn toggle_appends_new_and_removes_existing() {
      assert_eq!(toggle_target(Lang::Ko, &[Lang::En], Lang::Ja), vec![Lang::En, Lang::Ja]);
      assert_eq!(toggle_target(Lang::Ko, &[Lang::En, Lang::Ja], Lang::En), vec![Lang::Ja]);
  }
  #[test] fn toggle_keeps_last_remaining_target() {
      assert_eq!(toggle_target(Lang::Ko, &[Lang::En], Lang::En), vec![Lang::En]);
  }
  #[test] fn toggle_ignores_base() { assert_eq!(toggle_target(Lang::Ko, &[Lang::En], Lang::Ko), vec![Lang::En]); }
  #[test] fn move_up_and_down_and_edges() {
      let t = [Lang::En, Lang::Ja, Lang::Vi];
      assert_eq!(move_target(&t, Lang::Ja, true), vec![Lang::Ja, Lang::En, Lang::Vi]);
      assert_eq!(move_target(&t, Lang::Ja, false), vec![Lang::En, Lang::Vi, Lang::Ja]);
      assert_eq!(move_target(&t, Lang::En, true), t.to_vec());
      assert_eq!(move_target(&t, Lang::Vi, false), t.to_vec());
  }
  #[test] fn with_base_removes_new_base_from_targets() {
      let s = UserSettings { target_languages: vec![Lang::En], ..Default::default() };
      let s2 = with_base(&s, Lang::En);
      assert_eq!((s2.base_language, s2.target_languages), (Lang::En, vec![Lang::Ko]));
  }
  #[test] fn defaults_match_spec() {
      let s = UserSettings::default();
      assert_eq!((s.base_language, s.target_languages.clone(), s.include_original_on_replace), (Lang::Ko, vec![Lang::En], false));
      assert_eq!((s.claude_model.as_str(), s.claude_effort.as_str()), ("sonnet", "low"));
      assert_eq!((s.alt_alt_interval_seconds, s.ctrl_ctrl_interval_seconds), (0.2, 0.2));
  }
  #[test] fn store_round_trips_all_fields() // 임시 폴더에 update() 후 다시 load()하면 같은 값
  #[test] fn load_missing_file_is_defaults_without_problems()
  #[test] fn load_unreadable_file_is_defaults_with_problem() {
      // 파일 내용 "[unclosed" → problems == ["settings.yaml: 파일 형식 오류로 읽지 못해 기본 설정을 사용합니다."]
  }
  #[test] fn load_wrong_typed_field_uses_that_default_only() {
      // "alt_alt_interval_seconds: fast\nstart_on_login: true\n" → interval 0.2, start_on_login true
  }
  #[test] fn load_int_accepted_for_float() // "alt_alt_interval_seconds: 1" → 1.0
  #[test] fn load_unknown_codes_and_enums_fall_back() {
      // "target_languages: [xx, ja]\nkorean_correction_mode: wild\nctrl_ctrl_display_mode: x" → [Ja], Spelling, WholeText
  }
  #[test] fn load_accepts_single_string_target() // "target_languages: en" → [En]   (Review Focus 4)
  #[test] fn load_normalizes_targets_against_base() // "base_language: en\ntarget_languages: [en]" → [Ko]
  #[test] fn loading_message_per_mode() // Spelling → "한국어 교정 중...", Polish → "한국어 수정 중..."
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-core settings` → 컴파일 오류(아직 타입 없음).
- [ ] **Step 3: 구현한다.**
  - `SettingsStore::load`는 파일을 `serde_yaml::Value`로 읽고, 필드마다 v5 `_typed_or_default` 규칙으로 꺼낸다(bool은 bool만, f64는 정수·실수, 문자열은 문자열만, 열거형은 `parse` 실패 시 기본값).
  - `target_languages`는 문자열 목록 또는 문자열 하나를 받아 `from_code`로 바꾸고, 그다음 `normalize_targets(base, …)`를 적용한다.
  - 파일이 비어 있거나(`null`) 없으면 기본값이고 문제 메시지는 없다. 매핑이 아니거나 YAML 오류면 기본값과 위 문제 메시지를 낸다(파일 이름은 `path.file_name()`).
  - `update`는 스펙 2.1 필드 이름을 키로, 열거형은 `as_str` 값으로 써서 YAML로 저장한다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-core settings` → 모두 PASS.
- [ ] **Step 5: 커밋한다.** `feat(core): 언어·사용자 설정 모델과 다중 Target 정규화`

---

### Task 4: 앱 설정(env), 데이터 경로, 시작 경고 문구

**Files:**
- Create: `crates/ct-core/src/config.rs`, `crates/ct-core/src/paths.rs`, `crates/ct-core/src/file_problems.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum LlmProviderKind { ClaudeCli, Mock }
  pub struct ClaudeCliConfig { pub cli_path: String, pub model: Option<String>, pub effort: Option<String>,
                               pub timeout: Duration, pub clear_every_n_turns: u32 }
  pub struct SheetsConfig { pub batch_max_cells: usize, pub batch_max_chars: usize, pub max_cell_chars: usize,
                            pub max_range_cells: usize, pub job_timeout: Duration,
                            pub range_replace_enabled: bool, pub edit_full_cell_replace_enabled: bool }
  pub struct AppConfig { pub llm_provider: LlmProviderKind, pub claude_cli: ClaudeCliConfig, pub sheets: SheetsConfig }
  pub fn load_app_config(env: &HashMap<String, String>) -> AppConfig;
  pub fn load_app_config_from_environment(exe_dir: &Path, data_dir: &Path) -> AppConfig;
  pub fn user_data_dir(appdata: Option<&str>, home: &Path) -> PathBuf;
  pub fn file_problems_warning(problems: &[String]) -> Option<String>;
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `tests/unit/config/test_loader.py`, `test_sheets_config.py`, `test_paths.py`, `test_file_problems.py`를 포팅하고(BASE/TARGET_LANGUAGE 관련 케이스는 제외), 아래를 추가한다.
  ```rust
  #[test] fn defaults_when_env_empty() {
      let c = load_app_config(&HashMap::new());
      assert_eq!(c.llm_provider, LlmProviderKind::ClaudeCli);
      assert_eq!((c.claude_cli.cli_path.as_str(), c.claude_cli.timeout, c.claude_cli.clear_every_n_turns), ("claude", Duration::from_secs(60), 20));
      assert_eq!((c.sheets.batch_max_cells, c.sheets.batch_max_chars, c.sheets.max_cell_chars, c.sheets.max_range_cells), (30, 5000, 5000, 200));
      assert_eq!(c.sheets.job_timeout, Duration::from_secs(240));
      assert!(c.sheets.range_replace_enabled && c.sheets.edit_full_cell_replace_enabled);
  }
  #[test] fn mock_provider_and_empty_model_is_none() // LLM_PROVIDER=mock, CLAUDE_CLI_MODEL="" → Mock, None
  #[test] fn invalid_numbers_fall_back_to_defaults() // CLAUDE_CLI_TIMEOUT_SECONDS=abc, SHEETS_BATCH_MAX_CELLS=-3 → 기본값
  #[test] fn clear_zero_is_allowed() // CLAUDE_CLI_CLEAR_EVERY_N_TURNS=0 → 0
  #[test] fn sheets_flags_disable_only_with_zero() // "0" → false, 그 밖의 값이나 없음 → true
  #[test] fn data_dir_uses_appdata() // Some("C:\\Users\\a\\AppData\\Roaming") → ...\\ClaudeTranslator ; None → home/.claudetranslator
  #[test] fn warning_text_exact() {
      assert_eq!(file_problems_warning(&[]), None);
      assert_eq!(file_problems_warning(&["a".into()]).as_deref(), Some(
          "설정·사전 파일 일부를 읽지 못해 해당 내용 없이 시작했습니다.\n\n- a\n\n프로그램을 종료(트레이 → Exit)한 뒤 파일을 고치고 다시 시작하세요. 형식은 README의 '사전·설정 파일 샘플과 설정 방법'을 참고하세요."));
  }
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-core config paths file_problems` → FAIL.
- [ ] **Step 3: 구현한다.**
  - 숫자 값은 양수만 받는다(`CLAUDE_CLI_CLEAR_EVERY_N_TURNS`는 0 이상). 잘못된 값은 `tracing::warn!`으로 키 이름만 남기고 기본값을 쓴다.
  - `load_app_config_from_environment`는 `dotenvy::from_path`로 `exe_dir/.env`를 먼저 읽고, 다음으로 `data_dir/.env`를 읽는다(먼저 읽은 값이 이김). 파일이 없으면 무시한다. 그다음 `std::env::vars()`로 맵을 만들어 `load_app_config`에 넘긴다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-core` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(core): env 설정·데이터 경로·시작 경고 문구`

---

### Task 5: 개인 사전 모델, 로드·저장, 저장소

**Files:**
- Create: `crates/ct-core/src/dictionary/{mod.rs, model.rs, loader.rs, store.rs}`

**Interfaces:**
- Produces:
  ```rust
  pub enum Action { Keep, Translate }
  pub struct DictionaryEntry { pub term: String, pub action: Action, pub translations: BTreeMap<String, String> }
  impl DictionaryEntry { pub fn new(term: String, action: Action, translations: BTreeMap<String, String>) -> Result<Self, DictionaryError>; }
  #[derive(thiserror::Error)] pub enum DictionaryError { TranslateWithoutTranslations(String), Duplicate(String), NotFound(String), Io(#[from] std::io::Error) }
  pub fn load_dictionary_file(path: &Path, problems: &mut Vec<String>) -> Vec<DictionaryEntry>;
  pub fn save_dictionary_file(path: &Path, entries: &[DictionaryEntry]) -> std::io::Result<()>;
  pub struct PersonalDictionaryStore;
  impl PersonalDictionaryStore {
      pub fn load(path: &Path, problems: &mut Vec<String>) -> Self;
      pub fn entries(&self) -> &[DictionaryEntry];
      pub fn add(&mut self, entry: DictionaryEntry) -> Result<(), DictionaryError>;
      pub fn update(&mut self, term: &str, entry: DictionaryEntry) -> Result<(), DictionaryError>;
      pub fn delete(&mut self, term: &str) -> Result<(), DictionaryError>;
  }
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `tests/unit/dictionary/test_model.py`, `test_loader.py`, `test_writer.py`, `test_personal_store.py`를 포팅한다(merger 테스트는 제외, 조직 사전이 없음). 문구는 다음과 같다.
  - 읽을 수 없는 파일: `"user.yaml: 파일 형식 오류로 읽지 못했습니다."`
  - 건너뛴 항목: `"user.yaml: 항목 {n}개를 읽지 못해 건너뛰었습니다."`
  - 백업: `"user.yaml: 원본을 user.yaml.bak으로 백업했습니다."`

  v5와 같은 입력 규칙: `action`은 대소문자를 무시하고, 숫자 term·숫자 번역 값은 문자열로 바꾸며, bool·null·빈 번역 값은 버린다. TRANSLATE에 번역이 없으면 그 항목을 건너뛴다. 추가 테스트:
  ```rust
  #[test] fn store_add_duplicate_is_case_insensitive() // "DA" 추가 후 "da" 추가 → Err(Duplicate)
  #[test] fn store_backs_up_before_rewriting_broken_file() // 깨진 항목이 있는 파일 → load 시 .bak 생성, add 후 원본 .bak 내용 보존
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-core dictionary` → FAIL.
- [ ] **Step 3: 구현한다.** 로드는 `serde_yaml::Value`로 읽어 v5 `_parse_entry` 규칙을 그대로 적용한다. 저장 형식은 `entries:` 목록이고, 번역이 비어 있으면 `translations` 키를 생략한다. 모든 변경 메서드는 성공하면 즉시 파일에 저장한다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-core dictionary` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(core): 개인 사전 로드·저장·관리`

---

### Task 6: 사전 마스킹과 복원 (protector)

**Files:**
- Create: `crates/ct-core/src/dictionary/protector.rs`

**Interfaces:**
- Consumes: `DictionaryEntry`, `Action`, `Lang`
- Produces:
  ```rust
  pub struct MaskMatch { pub placeholder: String, pub entry: DictionaryEntry, pub original_text: String }
  pub struct MaskResult { pub masked_text: String, pub mapping: Vec<MaskMatch> }
  #[derive(thiserror::Error)] #[error("{0}")] pub struct PlaceholderIntegrityError(pub String);
  pub fn mask(text: &str, entries: &[DictionaryEntry]) -> MaskResult;
  pub fn validate_integrity(mapping: &[MaskMatch], text: &str) -> bool;
  pub fn restore(masked: &str, mapping: &[MaskMatch], target: Lang) -> Result<String, PlaceholderIntegrityError>;
  pub fn validate_integrity_across(mapping: &[MaskMatch], texts: &[&str]) -> bool;
  pub fn restore_fragment(masked: &str, mapping: &[MaskMatch], target: Option<Lang>) -> Result<String, PlaceholderIntegrityError>;
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `tests/unit/dictionary/test_protector.py`의 모든 케이스(longest match, 반복, 문장부호, 한/영 왕복, 무결성, 손상, 폴백, 단어 경계, 한글 조사 등)를 같은 입력으로 포팅하고, 아래를 추가한다.
  ```rust
  #[test] fn mask_handles_chars_whose_lowercase_changes_length() {   // Review Focus 1
      let e = [keep("DA")];
      let r = mask("İstanbul DA에서", &e);
      assert_eq!(r.masked_text, "İstanbul ⟦0⟧에서");
      assert_eq!(restore(&r.masked_text, &r.mapping, Lang::En).unwrap(), "İstanbul DA에서");
  }
  #[test] fn mask_handles_emoji_and_non_bmp() // "👍 DA ok" → "👍 ⟦0⟧ ok"
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-core protector` → FAIL.
- [ ] **Step 3: 구현한다.** 위치 정렬 규칙은 다음과 같다.
  - 텍스트와 각 표기를 `Vec<char>`로 다룬다. 소문자 변환은 문자마다 `c.to_lowercase()`가 정확히 한 글자를 내면 그 글자를 쓰고, 아니면 원래 문자를 그대로 쓴다. 이렇게 해서 원문과 소문자 버전의 위치가 1:1로 맞게 한다.
  - 후보 정렬과 경계 판정(`is_ascii_alphanumeric`), 폴백 규칙은 v5 `_candidate_forms`, `_boundary_ok`, `_translated_form`과 같다.
  - 토큰 형식은 `⟦{index}⟧`이고, 토큰을 찾을 때는 `⟦` + 숫자들 + `⟧`를 직접 훑는다(regex 의존성 없이).
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-core protector` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(core): 사전 결정적 마스킹·복원`

---

### Task 7: 언어 판별

**Files:**
- Create: `crates/ct-core/src/detector.rs`

**Interfaces:**
- Produces:
  ```rust
  pub trait LanguageDetector: Send + Sync { fn detect(&self, text: &str) -> Option<Lang>; }
  pub struct RuleBasedDetector;
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `tests/unit/language/test_detector.py`의 모든 케이스를 포팅한다(순수 4개 언어, 사전 마스킹된 혼합 문장, 영어 단어가 섞인 한국어, 한국 이름이 들어간 영어, 빈 텍스트·애매한 텍스트 → `None`).
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-core detector` → FAIL.
- [ ] **Step 3: 구현한다.** v5 `RuleBasedLanguageDetector`, `_is_korean_text`와 같은 유니코드 범위와 판정 순서(ko → ja → vi → en)를 쓴다. 단어 분리는 "라틴 또는 한글 문자의 연속"을 문자 순회로 직접 구현한다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-core detector` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(core): 규칙 기반 언어 판별`

---

### Task 8: 다중 Target 번역 계획과 출력 조립

**Files:**
- Create: `crates/ct-core/src/plan.rs`, `crates/ct-core/src/compose.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct TranslationPlan { pub source: Lang, pub destinations: Vec<Lang> }
  pub fn resolve_plan(detected: Option<Lang>, base: Lang, targets: &[Lang]) -> Option<TranslationPlan>;
  pub fn compose_replacement(original: Option<&str>, translations: &[String]) -> String;
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.**
  ```rust
  #[test] fn base_input_goes_to_all_targets_in_order() {
      let p = resolve_plan(Some(Lang::Ko), Lang::Ko, &[Lang::Ja, Lang::En]).unwrap();
      assert_eq!((p.source, p.destinations), (Lang::Ko, vec![Lang::Ja, Lang::En]));
  }
  #[test] fn target_input_goes_to_base_only() {
      let p = resolve_plan(Some(Lang::En), Lang::Ko, &[Lang::En, Lang::Ja]).unwrap();
      assert_eq!((p.source, p.destinations), (Lang::En, vec![Lang::Ko]));
  }
  #[test] fn other_or_unknown_language_is_none() {
      assert!(resolve_plan(Some(Lang::Vi), Lang::Ko, &[Lang::En]).is_none());
      assert!(resolve_plan(None, Lang::Ko, &[Lang::En]).is_none());
  }
  #[test] fn single_target_matches_v5_bidirectional_table() // v5 test_direction.py의 파라미터 표(ko↔en, ko↔vi, ko↔ja) 전부
  #[test] fn compose_without_original_single_is_identity() { assert_eq!(compose_replacement(None, &["Hello".into()]), "Hello"); }
  #[test] fn compose_with_original_and_two_translations() {
      assert_eq!(compose_replacement(Some("안녕\n"), &["Hello\n\n".into(), "こんにちは".into()]), "안녕\n\nHello\n\nこんにちは");
  }
  #[test] fn compose_keeps_inner_line_breaks() { assert_eq!(compose_replacement(None, &["a\nb".into(), "c".into()]), "a\nb\n\nc"); }
  #[test] fn compose_trims_crlf_only_at_block_end() {   // Review Focus 2
      assert_eq!(compose_replacement(Some("가\r\n나\r\n"), &["A\r\nB\r\n".into()]), "가\r\n나\n\nA\r\nB");
  }
  #[test] fn compose_keeps_leading_whitespace() { assert_eq!(compose_replacement(Some("  가"), &["A".into()]), "  가\n\nA"); }
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-core plan compose` → FAIL.
- [ ] **Step 3: 구현한다.** `compose_replacement`는 각 블록에 `trim_end_matches(['\r', '\n'])`를 적용한 뒤 `"\n\n"`으로 잇는다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-core plan compose` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(core): 다중 Target 번역 계획과 원문 포함 출력 조립`

---

### Task 9: LLM 포트(trait, 타입)와 JSON 응답 파싱

**Files:**
- Create: `crates/ct-core/src/llm.rs`, `crates/ct-core/src/test_support.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct TranslationRequest { pub text: String, pub source: Lang, pub destinations: Vec<Lang>, pub tone: Tone }
  pub struct TranslationResult { pub translations: Vec<(Lang, String)> }        // destinations 순서
  pub type ExplanationRequest = TranslationRequest;
  pub struct ExplanationSentence { pub original: String, pub translations: Vec<(Lang, String)> }
  pub struct ExplanationResult { pub sentences: Vec<ExplanationSentence> }
  pub struct CorrectionRequest { pub text: String, pub mode: CorrectionMode }
  pub struct CorrectionSentence { pub original: String, pub corrected: String }
  #[derive(thiserror::Error)] #[error("{0}")] pub struct LlmError(pub String);
  pub trait LlmProvider: Send + Sync {
      fn translate(&self, req: &TranslationRequest) -> Result<TranslationResult, LlmError>;
      fn explain(&self, req: &ExplanationRequest) -> Result<ExplanationResult, LlmError>;
      fn correct(&self, req: &CorrectionRequest) -> Result<String, LlmError>;
      fn correct_sentences(&self, req: &CorrectionRequest) -> Result<Vec<CorrectionSentence>, LlmError>;
  }
  pub fn parse_json_response(raw: &str) -> Result<serde_json::Value, LlmError>;
  pub fn parse_language_map(value: &serde_json::Value, destinations: &[Lang]) -> Result<Vec<(Lang, String)>, LlmError>;
  // test_support (#[cfg(test)]): pub struct FakeLlm { 각 메서드가 돌려줄 Result를 지정, 호출 횟수 기록 }
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `tests/unit/llm/test_json_utils.py`를 포팅하고(코드펜스, 앞뒤 설명 문장, 배열만 있으면 실패, 아무것도 없으면 실패), 아래를 추가한다.
  ```rust
  #[test] fn language_map_returns_in_destination_order() {
      let v = json!({"ja": "こんにちは", "en": "Hello"});
      assert_eq!(parse_language_map(&v, &[Lang::En, Lang::Ja]).unwrap(), vec![(Lang::En, "Hello".into()), (Lang::Ja, "こんにちは".into())]);
  }
  #[test] fn language_map_rejects_missing_extra_empty_and_non_string() {
      for v in [json!({"en": "Hi"}), json!({"en": "Hi", "ja": "x", "vi": "y"}), json!({"en": " ", "ja": "x"}), json!({"en": 1, "ja": "x"}), json!("en")] {
          assert!(parse_language_map(&v, &[Lang::En, Lang::Ja]).is_err());
      }
  }
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-core llm` → FAIL.
- [ ] **Step 3: 구현한다.**
  - `parse_json_response`는 v5 알고리즘을 따른다. 먼저 ```` ```json ```` 펜스를 벗기고 `serde_json::from_str`을 시도하고, 실패하면 각 `{` 위치에서 `serde_json::Deserializer::from_str(&raw[pos..]).into_iter::<Value>().next()`로 객체를 찾는다. 실패 메시지는 `"Claude CLI returned a malformed JSON response"`다.
  - `parse_language_map`의 키 비교는 소문자 코드로 정확히 일치해야 한다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-core llm` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(core): LLM 포트와 JSON·언어별 응답 파싱`

---

### Task 10: 번역 서비스와 설명 서비스 (다중 Target)

**Files:**
- Create: `crates/ct-core/src/translate.rs`, `crates/ct-core/src/explain.rs`

**Interfaces:**
- Consumes: `mask`, `restore`, `restore_fragment`, `validate_integrity_across` (Task 6), `LanguageDetector` (Task 7), `resolve_plan` (Task 8), `LlmProvider` (Task 9)
- Produces:
  ```rust
  pub struct Translated { pub source: Lang, pub translations: Vec<(Lang, String)> }
  #[derive(thiserror::Error)] pub enum TranslateError { #[error("Could not safely determine translation direction")] DirectionUnknown,
      #[error("{0}")] Llm(String), #[error("Dictionary placeholder integrity check failed")] Integrity }
  pub fn translate_text(text: &str, entries: &[DictionaryEntry], detector: &dyn LanguageDetector, llm: &dyn LlmProvider,
                        base: Lang, targets: &[Lang], tone: Tone) -> Result<Translated, TranslateError>;
  impl Translated { pub fn replacement_text(&self, original: &str, include_original: bool) -> String; } // compose_replacement 사용
  pub const DICTIONARY_INTEGRITY_MESSAGE: &str = "사전 용어 처리에 실패했습니다. 다시 시도해주세요.";
  pub fn explain_text(text: &str, entries: &[DictionaryEntry], detector: &dyn LanguageDetector, llm: &dyn LlmProvider,
                      base: Lang, targets: &[Lang], tone: Tone) -> Result<ExplanationResult, TranslateError>;
  pub fn restore_sentence_columns(result: ExplanationResult, mapping: &[MaskMatch]) -> Result<ExplanationResult, PlaceholderIntegrityError>;
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** `FakeLlm`을 써서 v5 `tests/unit/translation/test_service.py`, `tests/unit/explain/test_service.py`의 단일 Target 케이스를 포팅하고, 아래를 추가한다.
  ```rust
  #[test] fn translate_requests_all_targets_in_one_call_and_restores_per_language() {
      // 사전: TRANSLATE "logical model" {ko:"논리 모델", en:"logical model", ja:"論理モデル"}
      // 입력 "논리 모델 확인" (ko), targets [En, Ja]; FakeLlm.translate → [(En,"check ⟦0⟧"),(Ja,"⟦0⟧を確認")]
      // 기대: 호출 1회, req.destinations == [En, Ja], 결과 [(En,"check logical model"),(Ja,"論理モデルを確認")]
  }
  #[test] fn translate_target_language_input_goes_to_base_only() // "Hello" (en), targets [En, Ja] → req.destinations == [Ko]
  #[test] fn translate_fails_whole_when_one_language_breaks_tokens() // Ja 칸에서 ⟦0⟧ 누락 → Err(Integrity)
  #[test] fn translate_blank_text_fails_without_calling_llm() // "   " → Err(DirectionUnknown), 호출 0회   (Review Focus 3)
  #[test] fn translate_llm_error_is_llm_variant()
  #[test] fn replacement_text_with_and_without_original() // include_original true → "원문\n\nA\n\nB", false → "A\n\nB"
  #[test] fn explain_restores_originals_to_written_form_and_each_language_column() {
      // 원문 칸 토큰 → 사용자가 쓴 표기, en 칸 → en 사전 번역, ja 칸 → ja 사전 번역
  }
  #[test] fn explain_rejects_token_missing_in_any_language_column() // → Err(Integrity)
  #[test] fn explain_allows_token_to_move_between_sentences() // 칸 전체를 합친 기준으로 검증
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-core translate explain` → FAIL.
- [ ] **Step 3: 구현한다.**
  - 흐름은 `mask` → 마스킹된 텍스트로 `detect` → `resolve_plan` → LLM 1회 호출이다.
  - 그다음 언어마다 `validate_integrity` 후 `restore(…, lang)`를 실행한다. 하나라도 실패하면 `Integrity`다.
  - 설명 결과는 원문 칸 전체와 언어별 칸 전체에 각각 `validate_integrity_across`를 적용한 뒤, 원문은 `restore_fragment(None)`, 언어 칸은 `restore_fragment(Some(lang))`로 복원한다.
  - 로그에는 작업 이름, 글자 수, 소요 시간, 오류 종류만 남긴다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-core translate explain` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(core): 다중 Target 번역·설명 서비스`

---

### Task 11: 한글 교정 서비스

**Files:**
- Create: `crates/ct-core/src/correction.rs`

**Interfaces:**
- Consumes: Task 6, 7, 9, `restore_sentence_columns`의 원문 칸 검증 방식
- Produces:
  ```rust
  pub enum CorrectionStatus { Success, Unchanged, BaseNotKorean, NotKorean, LlmFailed, IntegrityFailed }
  impl CorrectionStatus { pub fn user_message(self) -> &'static str; }
  pub struct CorrectionOutcome { pub status: CorrectionStatus, pub corrected_text: Option<String>, pub sentences: Option<Vec<CorrectionSentence>> }
  pub fn correct_text(text: &str, base: Lang, mode: CorrectionMode, entries: &[DictionaryEntry], detector: &dyn LanguageDetector, llm: &dyn LlmProvider) -> CorrectionOutcome;
  pub fn correct_sentences(text: &str, base: Lang, mode: CorrectionMode, entries: &[DictionaryEntry], detector: &dyn LanguageDetector, llm: &dyn LlmProvider) -> CorrectionOutcome;
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `tests/unit/correction/test_service.py`와 `test_modes.py`의 모든 케이스를 포팅한다. 메시지는 글자 그대로 쓴다.
  - Success: `""`
  - Unchanged: `"수정할 부분이 없습니다."`
  - BaseNotKorean: `"한글 교정은 Base Language가 ko(한국어)일 때만 사용할 수 있습니다. 설정에서 Base Language를 ko로 바꿔주세요."`
  - NotKorean: `"선택한 문장의 주 언어가 한국어가 아니어서 교정하지 않았습니다."`
  - LlmFailed: `"교정에 실패하여 원본을 변경하지 않았습니다."`
  - IntegrityFailed: `DICTIONARY_INTEGRITY_MESSAGE`

  앞뒤 공백 보존(`"  원문 \n"` → 앞뒤 공백 유지, 가운데만 교정문)과, 교정문이 원문과 같으면(앞뒤 공백 무시) Unchanged인 경우를 꼭 포함한다.
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-core correction` → FAIL.
- [ ] **Step 3: 구현한다.** v5 `KoreanCorrectionService._run_single`, `_run_sentences`를 그대로 옮긴다. 사전 토큰은 사용자가 쓴 표기로 복원한다(`restore_fragment(None)`).
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-core correction` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(core): 한글 교정 서비스`

---

### Task 12: 프롬프트 (다중 언어, 다듬기 강화)

**Files:**
- Create: `crates/ct-llm/src/prompts.rs`

**Interfaces:**
- Consumes: `TranslationRequest`, `CorrectionRequest`, `Tone`, `CorrectionMode`, `Lang`
- Produces:
  ```rust
  pub const POLISH_BANNED_WORDS: &[&str];
  pub fn translation_prompt(req: &TranslationRequest, model: &str) -> String;
  pub fn explanation_prompt(req: &ExplanationRequest) -> String;
  pub fn correction_prompt(req: &CorrectionRequest, model: &str) -> String;
  pub fn sentence_correction_prompt(req: &CorrectionRequest) -> String;
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `tests/unit/llm/test_claude_cli.py`의 프롬프트 관련 단언(어투 문구, haiku일 때 약한 형식 지시, 토큰 보존 문구, spelling 문구)을 포팅하고, 아래를 추가한다.
  ```rust
  #[test] fn translation_prompt_lists_languages_in_order_and_shape() {
      let p = translation_prompt(&req("x", Lang::Ko, &[Lang::Ja, Lang::En]), "sonnet");
      assert!(p.contains("from ko into each of these languages: ja, en."));
      assert!(p.contains(r#"{"translations": {"ja": str, "en": str}}"#));
      assert!(p.ends_with("\n\nText:\nx"));
  }
  #[test] fn explanation_prompt_shape() // r#"{"sentences": [{"original": str, "translations": {"en": str}}]}"# 포함
  #[test] fn polish_prompt_allows_full_rewrite_and_bans_words() {
      let p = correction_prompt(&CorrectionRequest { text: "x".into(), mode: CorrectionMode::Polish }, "sonnet");
      assert!(p.contains("you may rewrite a sentence completely"));
      assert!(p.contains("keep the politeness level"));
      for w in POLISH_BANNED_WORDS { assert!(p.contains(w)); }
      assert!(p.contains("You may merge or split sentences."));
  }
  #[test] fn sentence_polish_prompt_is_one_to_one() // "Return exactly one corrected sentence for each original sentence" 포함, "merge or split" 없음
  #[test] fn spelling_prompt_has_no_rewrite_or_banned_list() // "rewrite" 와 POLISH_BANNED_WORDS[0] 없음
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-llm prompts` → FAIL.
- [ ] **Step 3: 구현한다.** 아래 문구를 글자 그대로 쓰고, 나머지 조각(어투 `_TONE_INSTRUCTIONS`, 형식 지시 strict/soft, spelling 지시, correction preamble)은 v5 `llm/claude_cli.py`의 문자열을 그대로 쓴다.
  - 번역:
    `"Translate the following text from {src} into each of these languages: {d1, d2}. Translate into each language independently, directly from the original text. Preserve any tokens that look like ⟦N⟧ exactly as-is -- do not translate or remove them. {formatting}{tone}Respond with only JSON matching this shape: {\"translations\": {\"d1\": str, \"d2\": str}} \n\nText:\n{text}"`
  - 설명:
    `"Split the following {src} text into sentences and, for each sentence, provide a translation into each of these languages: {d1, d2}. Preserve any tokens that look like ⟦N⟧ exactly as-is in both original and translations -- do not translate or remove them. {tone}Respond with only JSON matching this shape: {\"sentences\": [{\"original\": str, \"translations\": {\"d1\": str, \"d2\": str}}]} \n\nText:\n{text}"`
  - 다듬기 지시(v5 polish 문자열을 대체):
    `"Fix typos, spelling mistakes and Korean spacing (띄어쓰기) errors, and rewrite the text into natural, easy-to-read Korean. As long as the meaning and facts stay the same, you may rewrite a sentence completely: change word order, structure and word choice; the length may change. Even when little seems wrong, use a more natural expression if there is one. Keep the meaning and facts exactly (do not add or drop information), keep the politeness level and sentence endings (합니다체, 해요체 or 반말), and keep the paragraph and line-break structure. Do not add emphasis, exaggerated modifiers or emotional words that are not in the original; use plain, matter-of-fact wording suitable for work email and chat. In particular, do not introduce these words unless the original already uses them: {POLISH_BANNED_WORDS를 ", "로 연결}. "`
  - 문장별 다듬기 추가 지시:
    `"Return exactly one corrected sentence for each original sentence; within that sentence you may rewrite it completely. "`
  - `POLISH_BANNED_WORDS` = `["혁신적인", "획기적인", "압도적인", "놀라운", "탁월한", "완벽한", "극대화", "최적화된", "시너지", "패러다임", "여정", "도약", "게임 체인저", "핵심적인 역할", "필수적인", "원활한", "한층 더", "매우 중요한", "진정한", "깊이 있는"]`
  - 교정 응답 형태는 v5와 같다: `{"corrected": str}`, `{"sentences": [{"original": str, "corrected": str}]}`.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-llm prompts` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(llm): 다중 언어 프롬프트와 다듬기 강화`

---

### Task 13: ClaudeCliProvider

**Files:**
- Create: `crates/ct-llm/src/provider.rs`

**Interfaces:**
- Consumes: `prompts::*` (Task 12), `LlmProvider`, `parse_json_response`, `parse_language_map` (Task 9)
- Produces:
  ```rust
  pub trait CliTransport: Send + Sync { fn ask(&self, prompt: &str) -> Result<serde_json::Value, CliError>; } // 응답은 stream-json "result" 이벤트
  pub struct ClaudeCliProvider { /* transport: Arc<dyn CliTransport>, model: String */ }
  impl ClaudeCliProvider { pub fn new(transport: Arc<dyn CliTransport>, model: String) -> Self; }
  impl LlmProvider for ClaudeCliProvider { … }
  ```
  `CliError`는 Task 14에서 정의한다. 이 작업에서 먼저 `client.rs`에 enum 선언만 만든다: `Timeout`, `ProcessEnded`, `Spawn(String)`, `Io(String)`, `Rejected(String)`.

- [ ] **Step 1: 실패하는 테스트를 쓴다.** 가짜 transport(미리 정한 result 이벤트를 돌려주고 받은 프롬프트를 기록)로 v5 `test_claude_cli.py`의 응답 처리 케이스를 포팅한다(`is_error` → 오류, 빈 result → 오류, 깨진 JSON → 오류, 필드 누락 → 오류, 교정 빈 값 → 오류). 아래를 추가한다.
  ```rust
  #[test] fn translate_parses_language_map_in_order() // result = r#"{"translations":{"en":"Hi","ja":"やあ"}}"# → [(En,"Hi"),(Ja,"やあ")]
  #[test] fn translate_rejects_missing_language() // ja 누락 → Err
  #[test] fn explain_parses_sentences_with_language_maps()
  #[test] fn explain_rejects_sentence_missing_language()
  #[test] fn haiku_model_uses_soft_formatting() // new(_, "haiku") → 기록된 프롬프트에 "Keep the same line breaks."
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-llm provider` → FAIL.
- [ ] **Step 3: 구현한다.** 오류 메시지는 v5 `ClaudeCliProvider`와 같은 영어 문구를 쓴다. 예: `"Claude CLI returned an error: …"`, `"Claude CLI returned an empty response"`. `CliError`는 `"Claude CLI call failed: {e}"`로 감싼다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-llm provider` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(llm): ClaudeCliProvider`

---

### Task 14: ClaudeCliClient (상주 서브프로세스)

**Files:**
- Create: `crates/ct-llm/src/spawner.rs`, `crates/ct-llm/src/models.rs`
- Modify: `crates/ct-llm/src/client.rs`

**Interfaces:**
- Consumes: `ClaudeCliConfig` (Task 4)
- Produces:
  ```rust
  pub trait CliSpawner: Send + Sync { fn spawn(&self, args: &[String], cwd: Option<&Path>) -> std::io::Result<SpawnedCli>; }
  pub struct SpawnedCli { pub stdin: Box<dyn Write + Send>, pub stdout: Box<dyn Read + Send>, pub child: Box<dyn ChildHandle> }
  pub trait ChildHandle: Send { fn is_running(&mut self) -> bool; fn terminate(&mut self); }
  pub struct RealSpawner;   // std::process::Command, stderr null, CREATE_NO_WINDOW(0x0800_0000)
  pub struct ModelInfo { pub value: String, pub display_name: String, pub description: String, pub effort_levels: Vec<String> }
  pub fn parse_model_info(raw: &serde_json::Value) -> Option<ModelInfo>;
  pub const LIST_MODELS_REQUEST_ID: &str = "claudetranslator-list-models";
  pub fn build_command(config: &ClaudeCliConfig) -> Vec<String>;
  pub struct ClaudeCliClient;
  impl ClaudeCliClient {
      pub fn new(config: ClaudeCliConfig, cwd: Option<PathBuf>, spawner: Arc<dyn CliSpawner>) -> Self;
      pub fn warm_up(&self);
      pub fn list_models(&self, timeout: Duration) -> Result<Vec<ModelInfo>, CliError>;
      pub fn close(&self);
  }
  impl CliTransport for ClaudeCliClient { fn ask(&self, prompt: &str) -> Result<serde_json::Value, CliError>; }
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.**
  - `client.rs` 테스트 모듈에 `FakeSpawner`를 만든다. `std::io::pipe()` 두 쌍과 스크립트 스레드로 구성하고, 스크립트는 "stdin 한 줄을 받으면 응답 줄들을 쓴다"는 클로저다. `FakeSpawner`는 spawn 횟수와 받은 줄을 기록한다.
  - v5 `tests/unit/llm/test_claude_cli_client.py`의 모든 케이스를 포팅한다.
    - 명령 인자
    - 결과 이벤트 반환
    - 결과 전 다른 이벤트 무시
    - 비 JSON 줄 무시
    - 타임아웃(설정 200ms) 시 `Timeout`과 프로세스 정리
    - EOF 시 `ProcessEnded`, 다음 호출에서 재시작
    - N회마다 `/clear`와 그 result 소비, 0이면 `/clear` 없음
    - 재시작 시 카운터 초기화
    - list_models 성공·캐시·거부·타임아웃 시 프로세스 유지, ask가 늦게 온 control_response를 건너뜀
    - warm_up이 오류를 내지 않음
  - 아래를 추가한다.
  ```rust
  #[test] fn build_command_exact() {
      let c = ClaudeCliConfig { cli_path: "claude".into(), model: Some("sonnet".into()), effort: Some("low".into()), ..default() };
      assert_eq!(build_command(&c), ["claude","-p","--input-format","stream-json","--output-format","stream-json","--verbose","--tools","","--safe-mode","--model","sonnet","--effort","low"]);
  }
  #[test] fn user_message_is_stream_json_with_utf8() // stdin 줄 == {"type":"user","message":{"role":"user","content":"안녕"}}
  #[test] fn spawn_failure_is_error_and_next_call_retries() // 첫 spawn Err → CliError::Spawn, 두 번째 ask에서 다시 spawn 시도   (Review Focus 5)
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-llm client` → FAIL.
- [ ] **Step 3: 구현한다.**
  - `Mutex<Inner>`로 호출을 직렬화한다. `Inner`는 실행 중 프로세스(`stdin`, `child`, `events: crossbeam_channel::Receiver<Option<Value>>`), `turns_since_clear`, `models: Option<Vec<ModelInfo>>`를 가진다.
  - reader 스레드는 `BufReader::lines()`로 읽어서 JSON이면 `Some(v)`를 보내고, EOF면 `None`을 보낸 뒤 끝난다.
  - 대기는 `recv_timeout`으로 한다. `Timeout`, `ProcessEnded`, `Io` 오류가 나면 `close()` 후 오류를 반환한다. 단, `list_models`의 `Timeout`은 프로세스를 유지한다(v5 규칙).
  - `RealSpawner`는 `std::os::windows::process::CommandExt::creation_flags`를 `#[cfg(windows)]`로만 적용한다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-llm client` → PASS. 20회 반복해서 불안정하지 않은지 본다: `for i in $(seq 20); do cargo test -p ct-llm client -q || break; done`.
- [ ] **Step 5: 커밋한다.** `feat(llm): Claude CLI 상주 서브프로세스 클라이언트`

---

### Task 15: CLI 경로 탐색, 모델 카탈로그, Mock, 팩토리

**Files:**
- Create: `crates/ct-llm/src/{locator.rs, catalog.rs, mock.rs, factory.rs}`

**Interfaces:**
- Consumes: Task 4, 13, 14
- Produces:
  ```rust
  pub fn resolve_claude_cli_path(configured: &str, which: &dyn Fn(&str) -> Option<PathBuf>, home: &Path) -> String;
  pub fn fallback_models() -> Vec<ModelInfo>; // default "기본값 (자동)", opus, sonnet, haiku(effort 없음); effort = low, medium, high, xhigh, max
  pub enum CatalogSource { Fallback, Cache, Live }
  pub struct ModelCatalog;
  impl ModelCatalog {
      pub fn new(cache_path: Option<PathBuf>) -> Self;
      pub fn get(&self) -> Vec<ModelInfo>;
      pub fn source(&self) -> CatalogSource;
      pub fn add_listener(&self, f: Box<dyn Fn() + Send + Sync>) -> ListenerId;
      pub fn remove_listener(&self, id: ListenerId);
      pub fn refresh(&self, fetch: impl FnOnce() -> Result<Vec<ModelInfo>, CliError>) -> bool;
  }
  pub struct MockProvider;  // impl LlmProvider
  pub struct ProviderOverrides { pub cli_path: String, pub model: String, pub effort: String } // "" = 덮어쓰지 않음
  pub struct BuiltProvider { pub provider: Arc<dyn LlmProvider>, pub client: Option<Arc<ClaudeCliClient>> }
  pub fn build_llm_provider(cfg: &AppConfig, cli_cwd: &Path, overrides: &ProviderOverrides, models: &[ModelInfo], spawner: Arc<dyn CliSpawner>) -> BuiltProvider;
  pub fn refresh_catalog_and_warm_up(built: &BuiltProvider, catalog: &ModelCatalog);
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `test_cli_locator.py`, `test_model_catalog.py`, `test_mock.py`, `test_factory.py`를 포팅한다. 아래를 추가한다.
  ```rust
  #[test] fn mock_translates_each_destination_in_order() // "[en] x", "[ja] x"
  #[test] fn mock_explain_splits_sentences_without_regex_lookbehind() // "A. B? C" → 3문장
  #[test] fn factory_override_precedence() // overrides.model "opus" > env model; "" → env 값
  #[test] fn factory_drops_effort_for_model_without_effort_levels() // model "haiku"(models에 effort 없음) → build_command에 --effort 없음
  #[test] fn factory_mock_kind_has_no_client()
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-llm` → FAIL.
- [ ] **Step 3: 구현한다.**
  - 카탈로그 캐시 파일 형식은 v5와 같다: `{"fetched_at": RFC3339, "models": [{value, display_name, description, effort_levels}]}`. 저장은 `.tmp` 파일에 쓴 뒤 rename한다. `fetched_at`은 `std::time::SystemTime`의 UNIX 초를 문자열로 저장해도 된다(읽을 때 이 값은 쓰지 않음).
  - listener는 refresh를 실행한 스레드에서 호출한다. listener 하나가 panic해도 다른 listener와 refresh 결과에 영향이 없게 `catch_unwind`로 감싼다.
  - `refresh_catalog_and_warm_up`은 client가 있으면 `catalog.refresh(|| client.list_models(10s))`를 실행한다(별도 warm_up 없이, v5 규칙).
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-llm` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(llm): CLI 경로 탐색·모델 카탈로그·Mock·팩토리`

---

### Task 16: opt-in 실제 CLI 통합 테스트

**Files:**
- Create: `crates/ct-llm/tests/live_cli.rs`

**Interfaces:**
- Consumes: `build_llm_provider`, `RealSpawner`, `translate_text`, `explain_text`, `correct_text` (ct-core 일반 의존성으로 사용)

- [ ] **Step 1: 테스트를 작성한다.**
  - 각 테스트는 `RUN_CLAUDE_CLI_LIVE_TESTS`가 `"1"`이 아니면 `eprintln!("skipped")` 후 바로 반환한다.
  - 공통 설정: 임시 폴더를 cwd로 쓰고, 모델은 `sonnet`/`low`, 타임아웃은 120초다.
  - `live_translate_multi_keeps_tokens`: `"DA에서 논리 모델 확인해주세요."`, 사전은 KEEP DA와 TRANSLATE 논리 모델, targets `[En, Ja]`. 결과 두 개에 `"DA"`가 있고, en에는 `"logical model"`, ja에는 `"論理モデル"`가 있다.
  - `live_target_input_goes_to_base`: `"Please check the report."` → 결과 언어 `[Ko]`이고 한글이 들어 있다.
  - `live_explain_multi`: 두 문장, targets `[En, Ja]` → 문장마다 en과 ja가 있다.
  - `live_correct_spelling`: `"오늘 회의 는 3시 에 시작 합니다."` → Success이고 결과에 `"회의는"`이 있다.
  - `live_correct_polish_rewrites_keeps_haeyo_no_banned_words`: `"제가 생각했을 때에는 그 부분은 조금 더 확인을 해보는 것이 필요할 것 같아요."` → Success, 결과가 `"요."`로 끝나고, 원문과 다르며, 원문에 없던 `POLISH_BANNED_WORDS`가 없다.
  - `live_list_models`: 비어 있지 않고 `value`가 비어 있지 않다.
- [ ] **Step 2: 기본 실행에서 건너뛰는지 확인한다.** `cargo test -p ct-llm --test live_cli` → 모두 PASS(skipped 출력).
- [ ] **Step 3: 실제 CLI로 실행한다.** CLI가 설치되어 있고 로그인되어 있을 때만, PowerShell에서 `$env:RUN_CLAUDE_CLI_LIVE_TESTS="1"; cargo test -p ct-llm --test live_cli -- --test-threads=1`를 실행한다. 결과는 통과/실패 목록 그대로 보고하고, 통과는 "이번 실행에서 관찰됨"으로만 적는다. `.cmd` 실행 시 빈 인자(`--tools ""`) 전달이 실패하면 그 오류를 그대로 보고하고 멈춘다(계획 수정 필요).
- [ ] **Step 4: 커밋한다.** `test(llm): opt-in 실제 Claude CLI 통합 테스트`
