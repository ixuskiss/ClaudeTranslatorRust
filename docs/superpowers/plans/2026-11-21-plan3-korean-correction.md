# ClaudeTranslator 계획 3: 한글 교정 단축키 연결

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ctrl+Alt+Alt(한글 교정 후 교체)와 Alt+Ctrl+Ctrl(교정 결과를 창으로 보기)을 실제로 동작하게 연결한다.

**Architecture:**
- 교정 로직(`correct_text`, `correct_sentences`), 다듬기 프롬프트, 라우터의 교차 조합 판정, Settings의 "교정 범위" 콤보와 안내 문구는 계획 1·2에서 이미 만들었다. 이번 계획은 그것들을 잇는다.
- `ct-app/src/flows.rs`에 교정 흐름 두 개를 더한다. Alt+Alt / Ctrl+Ctrl 흐름과 같은 trait(`TextInteraction`, `Badge`, `Notifier`, `ExplainView`)을 쓴다.
- 수정키 해제 대기는 `ct_win::keys::wait_for_modifiers_released`를 클로저로 감싸 흐름에 넘긴다. 그래서 흐름은 가짜로 단위 테스트할 수 있다.
- 설명 창에 교정 결과 표시(`show_correction`)를 더한다. 원문은 굵게, 교정문은 그 아래에 둔다. 언어 표시는 붙이지 않는다.

**Tech Stack:** 계획 2와 같다(Rust 1.100 edition 2024, windows 0.62, slint 1.19). 새 의존성은 없다.

**Spec:** `docs/superpowers/specs/2026-11-20-claude-translator-rust-design.md` (§1.2, §1.4, §3, §5.3, §9 단계 6)

**v5 동작 참조:** `docs/superpowers/plans/v5-windows-ui-reference.md` §1.1(`wait_for_modifiers_released`), §3.2, §3.4(`run_correction`, `_show_result` 1·2번), §3.5, §5.5, §5.6.

**범위 밖:**
- Google Sheets 범위 교정·표 보기, Sheets 전용 문구(`_SHEETS_CORRECTION_*`). 계획 4에서 한다. 이번 계획에서 Sheets 페이지는 다른 페이지와 똑같이 텍스트 흐름을 탄다.
- `2026-11-21-plan2-followups.md`의 항목. 이번 계획에서 고치지 않는다.

## Global Constraints

- 계획 1·2의 공통 규칙을 그대로 따른다.
  - 테스트 밖에서는 `unwrap`/`expect`를 쓰지 않는다.
  - 원문·교정문은 로그에 남기지 않는다. 작업 종류, 상태, 글자 수, 소요 시간, 오류 종류만 남긴다.
  - 커밋 메시지는 한국어로 쓰고, 작성한 모델의 Co-Authored-By를 붙인다.
- 새로 합성하는 키는 없다. 수정키(Ctrl/Alt)가 눌린 채로는 어떤 합성 키도 보내지 않는다(v5 §1.1 "On False the caller MUST NOT send any synthetic keys").
- 수정키 해제 대기 시간은 2.0초다(v5 §1.1).
- 사용자 문구는 글자 그대로 쓴다.
  - Ctrl+Alt+Alt 캡처 실패, 수정키 대기 시간 초과: `텍스트를 선택한 후 Ctrl+Alt+Alt를 눌러주세요.`
  - Alt+Ctrl+Ctrl 캡처 실패, 수정키 대기 시간 초과: `텍스트를 선택한 후 Alt+Ctrl+Ctrl을 실행해주세요.`
  - 교정 상태 문구는 `CorrectionStatus::user_message()`를 그대로 쓴다(이미 v5 §3.5와 같다).
  - 진행 문구는 `CorrectionMode::loading_message()`를 쓴다(`한국어 교정 중...` / `한국어 수정 중...`).
  - CLI 실행 실패는 기존 `MSG_CLI_UNAVAILABLE`, 교체 실패는 기존 `MSG_REPLACE_FAILED`를 쓴다.
- "Alt+Alt 교체 시 원문 포함" 옵션은 교정에 영향을 주지 않는다(스펙 §2 표 "한글 교정 | 영향 없음"). 교정 교체 결과는 교정문만이다.
- 교정 교체 범위는 Alt+Alt와 같고(`REPLACE_STRATEGIES` 어댑터), 교정 보기 범위는 Ctrl+Ctrl과 같다(`READ_ONLY_STRATEGIES` 어댑터). Docs에서는 블록 선택이 있어야 한다.

## v5와 다르게 정한 것 (결정 기록)

- D1: 교정 교체가 Sheets가 아닌 페이지에서 교체에 실패하면 v5는 조용히 넘어간다. 이 버전은 Alt+Alt와 똑같이 `MSG_REPLACE_FAILED`를 보여준다(계획 2 Alt+Alt와 일관성).
- D2: 교정 중 CLI를 띄우지 못한 경우를 구분해 `MSG_CLI_UNAVAILABLE`을 보여준다. 이를 위해 `CorrectionOutcome`이 LLM 오류 문구를 들고 다닌다(Task 1). v5에는 없는 동작이다.
- D3: 교정 흐름 안에서 패닉이 나면 교정 실패 문구(`교정에 실패하여 원본을 변경하지 않았습니다.`)를 보여준다. 계획 2에서는 교차 조합 패닉에도 번역/설명 실패 문구를 썼다.

## Review Focus

1. Ctrl이나 Alt를 2초 넘게 누르고 있음 → 캡처하지 않고(Ctrl+C 등 합성 키 없음) 배지도 띄우지 않으며 안내 문구만 보인다. (Task 3 `replace_waits_for_modifiers_and_never_captures_on_timeout`, `view_timeout_shows_no_selection_and_never_captures`)
2. "원문 포함"이 켜진 상태에서 Ctrl+Alt+Alt → 교정문만 들어가고 원문이 붙지 않는다. (Task 3 `replace_ignores_include_original_option`)
3. Base Language가 ko가 아님 → 수정키 대기·캡처·배지 없이 바로 안내한다. (Task 3 `replace_base_not_korean_notifies_without_capture`, `view_base_not_korean_shows_error_without_capture`)
4. 교정 중 CLI를 띄우지 못함 → "교정에 실패..." 대신 CLI 경로 안내가 보인다. (Task 1 `llm_error_is_kept_on_llm_failed`, Task 3 `replace_cli_start_failure_shows_cli_notice`, `view_cli_start_failure_shows_cli_notice`)
5. 교정할 곳이 없음(Unchanged) → 교체하지 않고 `수정할 부분이 없습니다.`를 보여준다. 배지는 안내보다 먼저 사라진다. (Task 3 `replace_unchanged_notifies_after_badge_hidden_and_never_replaces`)

---

### Task 1: `CorrectionOutcome`에 LLM 오류 문구 보관

**Files:**
- Modify: `crates/ct-core/src/correction.rs` (`CorrectionOutcome`, `status_only`, `run_single`, `run_sentences`, 테스트)

**Interfaces:**
- Produces: `CorrectionOutcome { status, corrected_text, sentences, llm_error: Option<String> }`. `llm_error`는 상태가 `LlmFailed`이고 원인이 LLM의 `Err(LlmError(msg))`일 때만 `Some(msg)`다. 빈 응답으로 인한 `LlmFailed`와 다른 상태는 모두 `None`이다. 이 값은 로그에 남기지 않는다.

- [ ] **Step 1: 실패하는 테스트를 쓴다** (`correction.rs` tests 모듈, 기존 `FakeLlm`/`run` 도우미 사용)

```rust
#[test]
fn llm_error_is_kept_on_llm_failed() {
    // correct()와 correct_sentences() 모두: Err(LlmError("failed to start Claude CLI: x"))
    // → status == LlmFailed, llm_error == Some("failed to start Claude CLI: x")
}
#[test]
fn empty_llm_result_has_no_llm_error() {
    // 공백만 있는 응답 → status == LlmFailed, llm_error == None
}
#[test]
fn non_llm_statuses_have_no_llm_error() {
    // BaseNotKorean, NotKorean, Unchanged, Success 각각 llm_error == None
}
```

- [ ] **Step 2: 실패를 확인한다**

Run: `cargo test -p ct-core correction`
Expected: 컴파일 오류(`llm_error` 필드 없음)

- [ ] **Step 3: 구현한다**

`llm.correct(...)` / `llm.correct_sentences(...)`의 `match`에서 `Err(LlmError(m))` 팔을 따로 두어 `llm_error: Some(m)`을 채운다. 다른 생성 지점은 `llm_error: None`. 기존 테스트의 구조체 비교가 깨지면 기대값에 `llm_error: None`을 더한다.

- [ ] **Step 4: 통과를 확인한다**

Run: `cargo test -p ct-core`
Expected: PASS

- [ ] **Step 5: 커밋한다** — `feat(core): 교정 실패 시 LLM 오류 문구 보관`

---

### Task 2: 설명 창의 교정 결과 표시

**Files:**
- Modify: `crates/ct-app/src/flows.rs` (`ExplainView` trait, 테스트의 `FakeView`)
- Modify: `crates/ct-app/src/ui/explain.rs` (`build_correction_view_state`, `ExplainWindow` impl, 테스트)
- Modify: `crates/ct-app/src/main.rs` (테스트 `Recorder`의 `ExplainView` impl만)

**Interfaces:**
- Consumes: `ct_core::llm::CorrectionSentence { original: String, corrected: String }`, 기존 `ExplainBlock`, `ExplainViewState`, `Present::Fit`.
- Produces:
  - `ExplainView::show_correction(&self, sentences: Vec<CorrectionSentence>, mode: DisplayMode)`
  - `pub fn build_correction_view_state(sentences: &[CorrectionSentence], mode: DisplayMode) -> ExplainViewState` (`ui/explain.rs`)

- [ ] **Step 1: 실패하는 테스트를 쓴다** (`ui/explain.rs` tests)

```rust
#[test]
fn correction_per_sentence_one_block_per_sentence_without_tag() {
    // [("가", "가."), ("나", "나.")], PerSentence
    // → blocks == [ExplainBlock{original:"가", translations: vec![(None,"가.")]},
    //              ExplainBlock{original:"나", translations: vec![(None,"나.")]}], status == None
}
#[test]
fn correction_whole_text_joins_with_newline() {
    // 같은 입력, WholeText → 블록 1개: original "가\n나", translations vec![(None, "가.\n나.")]
}
#[test]
fn correction_empty_sentences_has_no_blocks() {
    // [] , WholeText → blocks.is_empty(), status == None
}
```

- [ ] **Step 2: 실패를 확인한다**

Run: `cargo test -p ct-app explain`
Expected: 컴파일 오류(`build_correction_view_state` 없음)

- [ ] **Step 3: 구현한다**

- `build_correction_view_state`: `build_view_state`와 같은 규칙(whole_text는 `\n` 결합, per_sentence는 문장마다 블록). 교정문에는 언어 표시를 붙이지 않는다(`None`).
- `ExplainWindow::show_correction`은 `self.dispatch(build_correction_view_state(&sentences, mode), Present::Fit)`. 결과 표시이므로 `show_result`처럼 숨긴 창을 다시 띄우지 않는다(v5 §3.4 1번 "NO bring_to_front").
- `flows.rs` 테스트의 `FakeView`는 `push(format!("correction:{}:{mode:?}", sentences.len()))`, `main.rs` 테스트의 `Recorder`는 `push("view.correction")`.

- [ ] **Step 4: 통과를 확인한다**

Run: `cargo test -p ct-app`
Expected: PASS

- [ ] **Step 5: 커밋한다** — `feat(app): 설명 창에 교정 결과 표시`

---

### Task 3: 교정 흐름 두 개

**Files:**
- Modify: `crates/ct-app/src/flows.rs`

**Interfaces:**
- Consumes: Task 1의 `CorrectionOutcome::llm_error`, Task 2의 `ExplainView::show_correction`, `ct_core::{correct_text, correct_sentences, CorrectionStatus, CorrectionOutcome, Lang}`, 기존 `is_cli_start_failure`, `BadgeGuard`, `replace_error_kind`, `MSG_REPLACE_FAILED`, `MSG_CLI_UNAVAILABLE`.
- Produces:

```rust
pub const MSG_CORRECTION_REPLACE_NO_SELECTION: &str = "텍스트를 선택한 후 Ctrl+Alt+Alt를 눌러주세요.";
pub const MSG_CORRECTION_VIEW_NO_SELECTION: &str = "텍스트를 선택한 후 Alt+Ctrl+Ctrl을 실행해주세요.";

/// `modifiers_released`: Ctrl과 Alt가 모두 떼질 때까지(최대 2초) 기다려 결과를 돌려준다.
pub fn correction_replace_flow(
    d: &FlowDeps,
    ti: &dyn TextInteraction,
    modifiers_released: &dyn Fn() -> bool,
    badge: &dyn Badge,
    notifier: &dyn Notifier,
);
pub fn correction_view_flow(
    d: &FlowDeps,
    ti: &dyn TextInteraction,
    modifiers_released: &dyn Fn() -> bool,
    view: &dyn ExplainView,
);
```

**동작 (v5 §5.5, §5.6, §3.4):**

`correction_replace_flow` (Ctrl+Alt+Alt):
1. `settings.base_language != Lang::Ko` → `notifier.notify(CorrectionStatus::BaseNotKorean.user_message())`, 끝.
2. `!modifiers_released()` → `notify(MSG_CORRECTION_REPLACE_NO_SELECTION)`, 끝. 캡처하지 않는다.
3. `badge.show(settings.korean_correction_mode.loading_message())`. 아래 작업은 `BadgeGuard` 안에서 하고, 알릴 문구는 배지가 사라진 뒤에 보낸다(`alt_alt_flow`와 같은 구조).
   - 캡처 실패 → `MSG_CORRECTION_REPLACE_NO_SELECTION` (Docs/Slides 구분 없이 항상).
   - `correct_text(...)` 결과가 `Success`가 아니면 → 문구(아래 공통 규칙). 교체하지 않는다.
   - `Success` → `ti.replace(&captured, corrected_text)`. 실패하면 `MSG_REPLACE_FAILED`(D1). `include_original_on_replace`는 보지 않는다.
4. 로그: `operation = "correction_replace"`, `elapsed_ms`, `notified`.

`correction_view_flow` (Alt+Ctrl+Ctrl):
1. base가 ko가 아님 → `view.show_error(BaseNotKorean 문구)`, 끝.
2. `!modifiers_released()` → `view.show_error(MSG_CORRECTION_VIEW_NO_SELECTION)`, 끝.
3. 캡처 실패 → `view.show_error(MSG_CORRECTION_VIEW_NO_SELECTION)`, 끝. 창은 캡처 뒤에만 띄운다(먼저 띄우면 Chrome 포커스를 뺏는다).
4. `view.show_loading(mode.loading_message())` → `correct_sentences(...)`.
5. `Success`이고 `sentences`가 `Some` → `view.show_correction(sentences, settings.ctrl_ctrl_display_mode)`. 아니면 `view.show_error(문구)`.
6. 로그: `operation = "correction_view"`, `elapsed_ms`, `status`.

공통 문구 규칙 `fn correction_message(o: &CorrectionOutcome) -> &'static str`: `LlmFailed`이고 `llm_error`가 `is_cli_start_failure`면 `MSG_CLI_UNAVAILABLE`, 그 밖에는 `o.status.user_message()`.

- [ ] **Step 1: 실패하는 테스트를 쓴다**

테스트 모듈에 교정 전용 가짜 LLM을 더한다: `struct CorrLlm { single: Result<String, String>, sentences: Result<Vec<CorrectionSentence>, String> }` (`translate`/`explain`은 `Err`). 수정키 대기는 `|| true` / `|| false` 클로저로 흉내 낸다. 기본 설정은 `UserSettings::default()`(base ko, Spelling, WholeText). 텍스트는 한국어(`"안녕 하세요"`)를 쓴다.

```rust
#[test]
fn replace_success_replaces_with_corrected_text_inside_badge() {
    // single = Ok("안녕하세요") → log == ["show:한국어 교정 중...", "capture", "replace:안녕하세요", "hide"]
}
#[test]
fn replace_polish_mode_shows_polish_loading_text() {
    // korean_correction_mode = Polish → 첫 항목 "show:한국어 수정 중..."
}
#[test]
fn replace_ignores_include_original_option() {
    // include_original_on_replace = true → "replace:안녕하세요" (원문 없음)
}
#[test]
fn replace_waits_for_modifiers_and_never_captures_on_timeout() {
    // modifiers_released = || false → log == [format!("notify:{MSG_CORRECTION_REPLACE_NO_SELECTION}")]
}
#[test]
fn replace_base_not_korean_notifies_without_capture() {
    // base_language = En, modifiers_released는 호출되면 log에 "wait"를 남기는 클로저
    // → log == [format!("notify:{}", CorrectionStatus::BaseNotKorean.user_message())]
}
#[test]
fn replace_capture_failure_notifies_after_badge_hidden() {
    // text None → [..., "capture", "hide", format!("notify:{MSG_CORRECTION_REPLACE_NO_SELECTION}")]
}
#[test]
fn replace_unchanged_notifies_after_badge_hidden_and_never_replaces() {
    // single = Ok("안녕 하세요") (원문과 같음) → "replace:" 없음, 마지막 두 항목 ["hide", "notify:수정할 부분이 없습니다."]
}
#[test]
fn replace_not_korean_text_notifies() {
    // text = "hello world" → "replace:" 없음, 마지막 항목 "notify:선택한 문장의 주 언어가 한국어가 아니어서 교정하지 않았습니다."
}
#[test]
fn replace_llm_failure_notifies_correction_failed() {
    // single = Err("boom") → 마지막 항목 "notify:교정에 실패하여 원본을 변경하지 않았습니다."
}
#[test]
fn replace_cli_start_failure_shows_cli_notice() {
    // single = Err(SPAWN_ERR) → 마지막 항목 format!("notify:{MSG_CLI_UNAVAILABLE}")
}
#[test]
fn replace_failure_notifies_replace_failed() {
    // FakeTi.replace_result = Err → 마지막 항목 format!("notify:{MSG_REPLACE_FAILED}")
}
#[test]
fn view_success_shows_loading_then_correction() {
    // sentences = Ok(vec![CorrectionSentence{original:"안녕 하세요", corrected:"안녕하세요"}])
    // → log == ["capture", "loading:한국어 교정 중...", "correction:1:WholeText"]
}
#[test]
fn view_uses_display_mode_setting() {
    // ctrl_ctrl_display_mode = PerSentence → 마지막 항목 "correction:1:PerSentence"
}
#[test]
fn view_timeout_shows_no_selection_and_never_captures() {
    // || false → log == [format!("error:{MSG_CORRECTION_VIEW_NO_SELECTION}")]
}
#[test]
fn view_base_not_korean_shows_error_without_capture() {
    // base En → log == [format!("error:{}", CorrectionStatus::BaseNotKorean.user_message())]
}
#[test]
fn view_capture_failure_shows_error_without_loading() {
    // text None → log == ["capture", format!("error:{MSG_CORRECTION_VIEW_NO_SELECTION}")]
}
#[test]
fn view_llm_failure_shows_correction_failed() {
    // sentences = Err("boom") → 마지막 항목 "error:교정에 실패하여 원본을 변경하지 않았습니다."
}
#[test]
fn view_cli_start_failure_shows_cli_notice() {
    // sentences = Err(SPAWN_ERR) → 마지막 항목 format!("error:{MSG_CLI_UNAVAILABLE}")
}
```

- [ ] **Step 2: 실패를 확인한다**

Run: `cargo test -p ct-app flows`
Expected: 컴파일 오류(`correction_replace_flow` 없음)

- [ ] **Step 3: 위 "동작"대로 두 흐름과 `correction_message`를 구현한다**

- [ ] **Step 4: 통과를 확인한다**

Run: `cargo test -p ct-app`
Expected: PASS

- [ ] **Step 5: 커밋한다** — `feat(app): Ctrl+Alt+Alt 교정 교체와 Alt+Ctrl+Ctrl 교정 보기 흐름`

---

### Task 4: 앱 연결과 수동 테스트 문서

**Files:**
- Modify: `crates/ct-app/src/main.rs` (`AppAction`, 조립부, 테스트)
- Modify: `crates/ct-app/src/flows.rs` (공개 상수 `MSG_CORRECTION_FAILED` 추가만)
- Create: `tests/manual/plan3_correction.md`

**Interfaces:**
- Consumes: Task 3의 두 흐름, `ct_win::keys::{wait_for_modifiers_released, KeySender}`, 조립부의 `keys: Arc<Win32KeySender>`.
- Produces:
  - `pub const MSG_CORRECTION_FAILED: &str = "교정에 실패하여 원본을 변경하지 않았습니다.";` (`flows.rs`. `CorrectionStatus::LlmFailed.user_message()`와 같은 값임을 테스트로 고정)
  - `AppAction`에 필드 `modifiers_released: Box<dyn Fn() -> bool + Send + Sync>`.

- [ ] **Step 1: 실패하는 테스트를 쓴다** (`main.rs` tests. 기존 `action(&log)` 도우미에 `modifiers_released: Box::new(|| true)`를 더한다)

```rust
#[test]
fn run_routes_correction_gestures_to_correction_flows() {
    // CtrlHeldAltAlt → take(&log) == ["badge.show", "capture", "badge.hide",
    //     format!("notify:{MSG_CORRECTION_REPLACE_NO_SELECTION}")]
    // AltHeldCtrlCtrl → take(&log) == ["capture", format!("view.error:{MSG_CORRECTION_VIEW_NO_SELECTION}")]
}
#[test]
fn on_panic_correction_gestures_show_correction_failed() {
    // CtrlHeldAltAlt → ["badge.hide", format!("notify:{MSG_CORRECTION_FAILED}")]
    // AltHeldCtrlCtrl → [format!("view.error:{MSG_CORRECTION_FAILED}")]
    // AltAlt / CtrlCtrl은 기존 문구 그대로(기존 테스트 유지)
}
#[test]
fn correction_failed_constant_matches_core_message() {
    assert_eq!(MSG_CORRECTION_FAILED, CorrectionStatus::LlmFailed.user_message());
}
```

기존 `run_routes_gestures_to_flows`에서 "Chords are not handled" 부분을 지운다. 기존 `on_panic_alt_family_hides_badge_and_notifies` / `on_panic_ctrl_family_shows_explain_error`에서 교차 조합 단언을 지운다(위 새 테스트로 옮김).

- [ ] **Step 2: 실패를 확인한다**

Run: `cargo test -p ct-app --bin ct-app`
Expected: FAIL(교차 조합이 아직 로그만 남김)

- [ ] **Step 3: 구현한다**

- `run`: `CtrlHeldAltAlt` → `correction_replace_flow(&deps, self.replace, self.modifiers_released, self.badge, self.notifier)`, `AltHeldCtrlCtrl` → `correction_view_flow(&deps, self.view_only, self.modifiers_released, self.view)`.
- `on_panic`: 네 제스처를 따로 매칭한다(D3).
- 조립부: `let keys_for_wait = keys.clone();` 후 `modifiers_released: Box::new(move || wait_for_modifiers_released(keys_for_wait.as_ref(), Duration::from_secs(2)))`. 2초는 이름 붙은 상수 `MODIFIER_RELEASE_TIMEOUT`로 둔다.

- [ ] **Step 4: 통과와 품질 검사를 확인한다**

Run: `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`
Expected: 모두 통과

- [ ] **Step 5: 수동 테스트 문서를 쓴다** (`tests/manual/plan3_correction.md`, `plan2_app.md`와 같은 형식: 절차 / 기대 결과 / 결과 칸)

항목:
- C1 Ctrl+Alt+Alt 교정 교체: Chat(작성 중 메시지 전체·선택), Gmail(본문 전체·선택), Slides(텍스트 상자 전체·선택), Docs(블록 선택). 오탈자가 있는 한국어 문장으로 확인한다.
- C2 Docs에서 블록 선택 없이 Ctrl+Alt+Alt → `텍스트를 선택한 후 Ctrl+Alt+Alt를 눌러주세요.`가 빠르게 뜨고 문서가 바뀌지 않는다.
- C3 Alt+Ctrl+Ctrl 교정 보기: whole_text / per_sentence 두 설정. 원문이 굵게, 교정문이 아래에, 언어 표시 없음. 원문은 바뀌지 않는다.
- C4 다듬기(polish) 모드: 결과가 원문과 충분히 다르고, 높임·문체가 유지되며, 스펙 §3의 금지 단어가 원문에 없으면 결과에도 없다. 진행 문구가 `한국어 수정 중...`이다.
- C5 원문 포함 켜짐 상태에서 Ctrl+Alt+Alt → 교정문만 들어간다.
- C6 Base Language를 en으로 바꾼 뒤 두 단축키 → Base 안내 문구. 키를 누르지 않은 상태로 되돌린다.
- C7 영어 문장 선택 → `선택한 문장의 주 언어가 한국어가 아니어서 교정하지 않았습니다.`
- C8 맞춤법이 이미 맞는 문장(spelling 모드) → `수정할 부분이 없습니다.`
- C9 개인 사전 용어가 들어간 문장 → 용어 표기가 그대로 남는다.
- C10 Ctrl을 누른 채 Alt+Alt 후 Ctrl을 3초 넘게 계속 누름 → 아무 키도 입력되지 않고 안내만 뜬다.
- C11 Ctrl+Alt+Alt 중 Alt+Alt(번역)가 끼어들지 않고, Alt+Ctrl+Ctrl 중 Ctrl+Ctrl이 끼어들지 않는다(같은 계열은 하나만 실행).
- C12 응답 시간 기록(spelling/polish, 짧은 문장·긴 문단).

- [ ] **Step 6: 커밋한다** — `feat(app): 한글 교정 단축키 연결과 수동 테스트 문서`
