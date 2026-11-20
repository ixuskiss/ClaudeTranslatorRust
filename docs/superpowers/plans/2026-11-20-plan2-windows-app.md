# ClaudeTranslator 계획 2: Windows 연동과 앱 1차 (ct-win + ct-app)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 실제로 실행되는 첫 버전을 만든다. 트레이에 상주하고, Chrome(Gmail/Chat/Slides/Docs 블록)에서 Alt+Alt로 다중 Target 번역 교체(원문 포함 옵션)를, Ctrl+Ctrl로 번역 보기 창을 쓸 수 있다. Settings와 개인 사전 창도 동작한다.

**Architecture:**
- 새 크레이트 `ct-win`이 Windows 연동을 맡는다. 키보드 훅(F15 처리), 코드 라우터, 키 전송, 클립보드, UIA 백엔드, Chrome 전략, 레지스트리, 단일 실행 보장이 여기 있다.
- `ct-app`이 조립, 작업 디스패치, Slint UI(트레이·Settings·사전·설명 창·배지)를 맡는다.
- 판단 로직(감지기, 라우터, F15 결정, 역할 분류, 전략, Chrome 어댑터, 작업 흐름, Settings 뷰모델)은 순수 함수와 trait 뒤에 두어 단위 테스트한다. Win32/COM/Slint 호출부는 얇게 둔다.

**Tech Stack:** Rust 1.100 (edition 2024), `windows` 0.62, slint 1.19 (`raw-window-handle-06`), tray-icon 0.26, crossbeam-channel, tracing / tracing-subscriber / tracing-appender. 계획 1의 `ct-core`, `ct-llm`을 쓴다.

**Spec:** `docs/superpowers/specs/2026-11-20-claude-translator-rust-design.md`

**v5 동작 참조(필독, 계획의 "v5 참조 §x"는 이 문서의 절):** `docs/superpowers/plans/v5-windows-ui-reference.md`. v5 원본 `C:\python_workspace\ClaudeLangTrans_v5`는 읽기 전용이다.

**계획 1 후속 항목:** `docs/superpowers/plans/2026-11-20-plan1-followups.md`. "계획 2 초반" 항목은 이 계획의 Task 11·13·16에서 처리한다.

**범위 밖:** 한글 교정 단축키 연결(계획 3)과 Google Sheets 라우팅·셀 처리(계획 4). 라우터는 교차 조합(Ctrl을 누른 채 Alt+Alt, Alt를 누른 채 Ctrl+Ctrl)까지 판정한다. 다만 이번 계획에서 그 동작은 로그만 남기고 아무것도 하지 않는다. Sheets 셀 편집 전략도 만들지 않는다.

## Global Constraints

- 앱 이름은 `ClaudeTranslator`, 데이터 폴더는 `%APPDATA%\ClaudeTranslator`다. 트레이 툴팁과 창 제목도 `ClaudeTranslator`를 쓴다(v5의 `ClaudeLangTrans`를 대체).
- 계획 1의 공통 규칙을 그대로 따른다.
  - 테스트 밖에서는 `unwrap`/`expect`를 쓰지 않는다(`lib.rs` deny 속성).
  - 원문·번역문은 로그에 남기지 않는다.
  - 커밋은 한국어 메시지에, 작성한 모델의 Co-Authored-By를 붙인다.
- 절대 합성하지 않는 키: Enter, Ctrl+Enter, 전송 클릭. 코드에 존재하는 합성 키는 Ctrl+C, Ctrl+A, Ctrl+V, Esc, F15뿐이다.
- 사용자 문구는 v5 참조 §3.4, §3.5, §4, §5.3에 있는 문자열을 글자 그대로 쓴다. 단, 앱 이름이 들어간 문자열(`ClaudeLangTrans`)은 `ClaudeTranslator`로 바꾼다. 이 계획에서 새로 정한 문구는 계획에 적힌 그대로 쓴다.
- 작업 완료 조건: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`. Win32/UI 작업은 여기에 `cargo build -p ct-app`도 추가한다. 화면 동작은 터미널로 확인할 수 없으므로 "검증했다"고 쓰지 않고, `tests/manual/plan2_app.md`에 절차를 남긴다.
- 실제 CLI 테스트는 PowerShell에서만 실행한다(Git Bash는 os error 5).

## v5와 다르게 정한 것 (계획 결정)

| # | 결정 | 이유 |
|---|---|---|
| P1 | 단축키 간격을 Settings에서 저장하면 즉시 반영한다(v5는 재시작해야 반영). 간격은 `Arc<AtomicU64>`(밀리초)로 감지기가 매 판정 때 읽는다. | 사용자 기대. 회귀 없음 |
| P2 | 선택이 없을 때 전체 텍스트를 복사하는 경로도 클립보드에 sentinel을 먼저 넣는다. 복사 후에도 sentinel 그대로면 빈 텍스트로 본다. | v5에서 빈 입력란이 예전 클립보드 내용을 읽던 문제 |
| P3 | Alt+Alt에서 번역이 실패하면(v5는 아무것도 안 보여줌) 안내한다. 방향 판별 실패는 `번역 방향을 판단할 수 없어 원문을 변경하지 않았습니다.`, LLM 실패는 `번역에 실패하여 원본을 변경하지 않았습니다.`, 사전 토큰 실패는 `사전 용어 처리에 실패했습니다. 다시 시도해주세요.`를 띄운다. Chrome 경로의 교체 실패(v5는 조용히 넘김)도 `교체 대상이 바뀌어 원문을 변경하지 않았습니다.`로 안내한다. | 스펙 6장 "실패 시 안내", 사용자 기대 |
| P4 | 이름 있는 뮤텍스 `Local\ClaudeTranslator.SingleInstance`로 두 번째 실행을 막는다. 이미 실행 중이면 `ClaudeTranslator가 이미 실행 중입니다.` 메시지를 띄우고 종료한다. | 같은 앱이 두 번 뜨면 단축키 한 번에 두 번 반응함 |
| P5 | Python의 `SendKeys`가 몰래 넣던 0.5초 대기를 명시적으로 넣는다. Ctrl+C 뒤에는 클립보드 시퀀스 번호가 바뀔 때까지 최대 500ms 기다린 뒤 50ms 더 둔다. Ctrl+V 뒤에는 500ms 기다린 다음에야 클립보드를 복원한다. Ctrl+A 뒤에는 200ms를 둔다. | 붙여넣기가 끝나기 전에 클립보드를 복원하면 원래 내용이 붙여짐 |
| P6 | 트레이 Target 메뉴 표시와 체크는 Settings를 저장할 때마다 갱신한다. | v5에서는 갱신되지 않던 문제 |
| P7 | 자동 시작 레지스트리 값은 `ClaudeTranslator`다. release 빌드(`cfg!(not(debug_assertions))`)에서만 쓴다. v3/v4/v5 감지 경고는 넣지 않는다(스펙 1.3). | 스펙 8장 |

## Review Focus

1. **Alt+Alt를 누른 뒤 사용자가 손을 늦게 떼거나 다른 창을 클릭해도** 엉뚱한 창에 붙여넣으면 안 된다. 교체 직전에 다시 확인한 문맥(앱과 field_id)이 다르면 교체하지 않는다. 테스트: Task 8 `replace_aborts_when_focus_moved_to_other_field`.
2. **클립보드에 사용자가 복사해 둔 이미지나 서식 텍스트**가 있어도 앱이 죽지 않아야 하고, 텍스트 경로에서는 v5처럼 텍스트만 복원한다. 클립보드가 비어 있으면 빈 상태로 둔다. 테스트: Task 5 `temporary_content_restores_previous_text_and_skips_when_none`.
3. **Ctrl+Ctrl 창을 띄운 상태에서 또 Ctrl+Ctrl**을 누르면 같은 창이 새 결과로 바뀌어야 하고, 창이 두 개 생기면 안 된다. 테스트: Task 14 `explain_view_reuses_single_window` (뷰 상태 테스트).
4. **Target이 2개 이상이고 원문 포함이 켜진 상태에서 영어 입력**이면 "원문 + 한국어 번역 1개"만 교체되어야 한다. 테스트: Task 12 `alt_alt_target_input_replaces_with_original_and_base_only`.
5. **번역이 진행되는 동안 Settings에서 Target을 바꿔 저장**해도 진행 중인 작업은 시작할 때 읽은 설정으로 끝나야 하고, 앱이 멈추면 안 된다. 테스트: Task 13 `dispatch_snapshots_settings_at_trigger`.

---

## 파일 구조

```
crates/ct-win/                      (새 크레이트, windows 크레이트 의존, ct-core 의존)
  src/lib.rs
  src/hotkey/detector.rs            DoublePressDetector
  src/hotkey/router.rs              ChordRouter, Gesture, Modifier, KeyDirection
  src/hotkey/f15.rs                 AltTapState (F15 결정, 순수)
  src/hotkey/hook.rs                KeyboardHook (Win32 LL 훅 스레드)
  src/keys.rs                       KeySender trait, Win32KeySender, wait_for_modifiers_released, wait_until_alt_released
  src/clipboard.rs                  Clipboard trait, Win32Clipboard, run_with_temporary_content, copy_with_sentinel
  src/chrome/role.rs                url 판정, classify_focused_role
  src/chrome/strategy.rs            FocusedElement, EditingContext, Strategy, strategy 목록
  src/chrome/adapter.rs             UiaBackend trait, ChromeAdapter, RetryingInteraction, TextInteraction
  src/chrome/uia_backend.rs         WindowsUiaBackend (COM/UIA + 키 + 클립보드)
  src/registry.rs                   apply_autostart
  src/single_instance.rs            SingleInstance
crates/ct-llm/src/spawner.rs        (수정) Job Object, stderr 끝부분 수집
crates/ct-app/
  Cargo.toml                        slint/tray-icon/windows를 일반 의존성으로
  build.rs                          slint-build로 ui/*.slint 컴파일
  ui/explain.slint, ui/settings.slint, ui/dictionary.slint, ui/badge.slint
  src/main.rs                       조립, 시작·종료 순서
  src/logging.rs                    tracing 설정, 패닉 → crash.log
  src/flows.rs                      alt_alt_flow, ctrl_ctrl_flow (순수 오케스트레이션)
  src/dispatch.rs                   Dispatcher (가드, 일시정지, 설정 스냅샷, catch_unwind)
  src/ui/win32.rs                   hwnd_of, wait_for_hwnd, bring_to_front, set_no_activate, message_box
  src/ui/badge.rs                   Badge
  src/ui/explain.rs                 ExplainWindow, ExplainViewState
  src/ui/tray.rs                    Tray, target_menu_items
  src/ui/settings.rs                SettingsWindow, SettingsForm(뷰모델)
  src/ui/dictionary.rs              DictionaryWindow
tests/manual/plan2_app.md           실제 Chrome 수동 테스트
```

`crates/ct-app/examples/spike.rs`는 그대로 둔다. Task 13에서 스파이크 전용 dev-dependency를 일반 의존성으로 옮긴다.

---

### Task 1: ct-win 뼈대, 더블 탭 감지기

**Files:**
- Create: `crates/ct-win/Cargo.toml`, `crates/ct-win/src/lib.rs`, `crates/ct-win/src/hotkey/{mod.rs,detector.rs}`

**Interfaces:**
- Produces:
  ```rust
  pub struct DoublePressDetector { /* interval_ms: Arc<AtomicU64>, is_down, last_press: Option<Instant> */ }
  impl DoublePressDetector {
      pub fn new(interval_ms: Arc<AtomicU64>) -> Self;
      pub fn on_key_down(&mut self, at: Instant) -> bool;   // true = 두 번째 down (쌍 소비)
      pub fn on_key_up(&mut self);
      pub fn reset_pending(&mut self);                      // last_press만 지움
  }
  ```
- `ct-win/Cargo.toml`: `windows` 0.62(이번 계획 전체에 필요한 feature: Foundation, UI_WindowsAndMessaging, UI_Input_KeyboardAndMouse, System_LibraryLoader, System_Com, UI_Accessibility, System_Threading, System_DataExchange, System_Memory, System_Registry, Security), `ct-core`, `tracing`, `thiserror`, `crossbeam-channel`.

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 참조 §1.2 규칙을 따른다. 시각은 `Instant` 기준점에 ms를 더해 만든다.
  ```rust
  #[test] fn second_down_within_interval_fires_once()  // 200ms: down0 up down150 → false, true
  #[test] fn interval_is_inclusive()                   // down0, down200 → true
  #[test] fn beyond_interval_rearms()                  // down0 up down250 → false (새 기준), down400 → true
  #[test] fn auto_repeat_is_ignored()                  // down0 down50(up 없음) → false,false; up; down100 → true
  #[test] fn measured_down_to_down_not_hold()          // down0 (180ms 유지) up down190 → true
  #[test] fn triple_tap_needs_fourth()                 // d0 u d100(true) u d150 → false, u d200 → true
  #[test] fn reset_pending_keeps_is_down()             // down0 reset → down(반복)50 false, up, down60 → false (쌍 지워짐)
  #[test] fn interval_change_applies_immediately()     // AtomicU64를 500으로 바꾸면 d0 u d400 → true
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-win detector` → FAIL.
- [ ] **Step 3: 구현한다.**
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-win detector` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(win): ct-win 크레이트와 더블 탭 감지기`

---

### Task 2: 코드 라우터 (Alt+Alt / Ctrl+Ctrl / 교차 조합)

**Files:**
- Create: `crates/ct-win/src/hotkey/router.rs`

**Interfaces:**
- Consumes: `DoublePressDetector` (Task 1)
- Produces:
  ```rust
  pub enum Modifier { Alt, Ctrl }
  pub enum KeyDirection { Down, Up }
  pub enum Gesture { AltAlt, CtrlCtrl, CtrlHeldAltAlt, AltHeldCtrlCtrl }
  pub struct ChordRouter;
  impl ChordRouter {
      pub fn new(alt_interval_ms: Arc<AtomicU64>, ctrl_interval_ms: Arc<AtomicU64>,
                 is_physically_down: Box<dyn Fn(Modifier) -> Option<bool> + Send>) -> Self;
      /// 훅 스레드에서 동기 호출. 발동한 제스처를 돌려준다(작업 실행은 호출자 몫).
      pub fn on_key(&mut self, key: Modifier, dir: KeyDirection, at: Instant) -> Option<Gesture>;
  }
  ```
  감지기는 네 개다: alt plain, alt chord, ctrl plain, ctrl chord.

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `tests/unit/hotkey/test_chord_router.py`의 모든 케이스를 포팅하고, 아래를 포함한다(v5 참조 §1.4).
  ```rust
  #[test] fn alt_alt_fires_plain()
  #[test] fn ctrl_held_through_alt_alt_fires_correction_replace_gesture()  // CtrlHeldAltAlt
  #[test] fn alt_held_through_ctrl_ctrl_fires_correction_view_gesture()    // AltHeldCtrlCtrl
  #[test] fn ctrl_tap_between_alt_taps_cancels()                           // Alt, Ctrl d/u, Alt → None
  #[test] fn ctrl_only_on_second_tap_fires_nothing()
  #[test] fn up_for_unknown_key_is_ignored()
  #[test] fn stale_other_modifier_is_released_on_next_down()  // is_physically_down(Ctrl)=Some(false)이면 plain으로 판정
  #[test] fn probe_error_keeps_recorded_state()               // None → 기록 유지
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-win router` → FAIL.
- [ ] **Step 3: 구현한다.** 라우터는 가드와 작업 실행을 모른다. 제스처만 돌려준다. 가드는 Task 13의 Dispatcher가 맡는다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-win router` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(win): Alt·Ctrl 코드 라우터`

---

### Task 3: F15 결정 로직과 키보드 훅

**Files:**
- Create: `crates/ct-win/src/hotkey/f15.rs`, `crates/ct-win/src/hotkey/hook.rs`

**Interfaces:**
- Consumes: `Modifier`, `KeyDirection` (Task 2)
- Produces:
  ```rust
  pub enum RawKey { Alt, Ctrl, Other }
  pub struct AltTapState;   // alt_is_down, combo_used
  impl AltTapState {
      pub fn new() -> Self;
      /// 주입되지 않은 키 이벤트마다 호출. true면 "지금 F15 down을 보내고 50ms 뒤 up".
      pub fn on_event(&mut self, key: RawKey, dir: KeyDirection) -> bool;
  }
  pub const F15_RELEASE_DELAY: Duration = Duration::from_millis(50);
  pub struct KeyboardHook;
  impl KeyboardHook {
      /// 훅 스레드를 띄우고 설치 결과를 기다린다(최대 5초). 실패하면 Err.
      pub fn start(on_key: Box<dyn FnMut(Modifier, KeyDirection, Instant) + Send>) -> Result<KeyboardHook, HookError>;
      pub fn stop(self);    // PostThreadMessageW(WM_QUIT) 후 join(최대 5초), Unhook
  }
  ```

- [ ] **Step 1: `AltTapState`의 실패하는 테스트를 쓴다.** v5 참조 §1.7 규칙을 따른다.
  ```rust
  #[test] fn solo_alt_tap_requests_f15()                // Alt d, Alt u → u에서 true
  #[test] fn alt_with_other_key_skips_f15()             // Alt d, Other d, Alt u → false
  #[test] fn alt_with_ctrl_after_skips_f15()            // Alt d, Ctrl d, Alt u → false (Alt를 누른 채 Ctrl+Ctrl)
  #[test] fn ctrl_before_alt_still_requests_f15()       // Ctrl d, Alt d, Alt u → true
  #[test] fn alt_up_without_down_still_requests_f15()   // v5: _alt_is_down이 false여도 combo 아니면 true
  #[test] fn repeat_alt_down_does_not_reset_combo()
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-win f15` → FAIL.
- [ ] **Step 3: 구현한다.**
  - `AltTapState`를 구현한다.
  - 훅 스레드를 구현한다. 스파이크 `crates/ct-app/examples/spike.rs`의 `ll_proc`/`send_key`/`spawn_hook_thread`가 실제 Windows에서 검증된 출발점이다.
  - 훅 규칙:
    - `LLKHF_INJECTED` 이벤트는 판정에서 제외하고 그대로 넘긴다.
    - VK_LMENU/VK_RMENU/VK_MENU는 Alt, VK_LCONTROL/VK_RCONTROL/VK_CONTROL은 Ctrl, 나머지는 Other로 분류한다.
    - Other는 `AltTapState`에만 넘기고 `on_key`에는 넘기지 않는다(v5: 수식키 외의 키는 라우터로 가지 않음).
    - F15 down은 `CallNextHookEx` 전에 보내고, up은 별도 스레드에서 50ms 뒤에 보낸다.
    - `SendInput` 반환값이 1이 아니면 `tracing::warn!`(vk, flags, GetLastError)을 남긴다.
    - 콜백 본문은 `catch_unwind`로 감싼다. 무슨 일이 있어도 `CallNextHookEx`를 반환한다.
    - 시각은 콜백에서 `Instant::now()`로 잡는다.
- [ ] **Step 4: 테스트가 통과하고 빌드되는지 확인한다.** `cargo test -p ct-win`, `cargo clippy -p ct-win --all-targets -- -D warnings` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(win): F15 처리를 포함한 저수준 키보드 훅`

---

### Task 4: 키 전송과 수식키 대기

**Files:**
- Create: `crates/ct-win/src/keys.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum Chord { CtrlC, CtrlA, CtrlV, Esc }
  pub trait KeySender: Send + Sync {
      fn send(&self, chord: Chord) -> Result<(), KeyError>;   // SendInput 한 번에 down/up 묶음
      fn is_down(&self, m: Modifier) -> bool;                  // GetAsyncKeyState & 0x8000
  }
  pub struct Win32KeySender;
  pub const MODIFIER_RELEASE_TIMEOUT: Duration = Duration::from_secs(2);
  pub const ALT_RELEASE_TIMEOUT: Duration = Duration::from_millis(500);
  pub const POLL: Duration = Duration::from_millis(20);
  pub fn wait_for_modifiers_released(keys: &dyn KeySender, timeout: Duration) -> bool;  // 둘 다 떼면 true
  pub fn wait_until_alt_released(keys: &dyn KeySender, timeout: Duration);              // 시간이 다 돼도 그냥 반환
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** 가짜 `KeySender`(is_down 응답 순서를 지정)로 확인한다.
  ```rust
  #[test] fn returns_immediately_when_released()
  #[test] fn waits_until_both_released()
  #[test] fn times_out_false_when_held()      // timeout 100ms
  #[test] fn alt_wait_returns_after_timeout_even_if_held()
  #[test] fn chord_never_includes_enter()     // Chord의 모든 변형에 대응하는 VK 목록에 VK_RETURN이 없음
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-win keys` → FAIL.
- [ ] **Step 3: 구현한다.** `Win32KeySender::send`는 Ctrl down → 키 down → 키 up → Ctrl up을 한 번의 `SendInput`으로 보내고, 반환값을 확인한다. 정착 대기(P5)는 이 함수가 아니라 호출자(Task 9)가 넣는다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-win keys` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(win): 키 전송과 수식키 해제 대기`

---

### Task 5: 클립보드

**Files:**
- Create: `crates/ct-win/src/clipboard.rs`

**Interfaces:**
- Produces:
  ```rust
  pub trait Clipboard: Send + Sync {
      fn get_text(&self) -> Option<String>;               // CF_UNICODETEXT, 실패·없음 → None
      fn set_text(&self, text: &str) -> Result<(), ClipboardError>;
      fn sequence_number(&self) -> u32;
  }
  pub struct Win32Clipboard;   // OpenClipboard 최대 5회, 20ms 간격
  pub const SENTINEL: &str = "\u{0}claudetranslator_probe\u{0}";
  /// v5 ClipboardFallback.run_with_temporary_content: 이전 텍스트 보존 → set_text(최대 3회) → action → 이전 텍스트가 있었으면 복원
  pub fn run_with_temporary_content<T>(cb: &dyn Clipboard, text: &str, action: impl FnOnce() -> Result<T, String>) -> Result<T, String>;
  /// sentinel을 넣고 copy를 실행한 뒤 시퀀스 번호가 바뀔 때까지(최대 500ms) 기다리고 50ms 더 둔다.
  /// 결과가 비었거나 sentinel이면 None. 끝나면 이전 텍스트를 복원한다(P2·P5).
  pub fn copy_with_sentinel(cb: &dyn Clipboard, copy: impl FnOnce() -> Result<(), String>) -> Option<String>;
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** 메모리 가짜 `Clipboard`로 확인한다.
  ```rust
  #[test] fn temporary_content_restores_previous_text_and_skips_when_none()  // Review Focus 2
  #[test] fn temporary_content_retries_set_three_times_then_errors()
  #[test] fn temporary_content_restores_even_when_action_fails()
  #[test] fn copy_with_sentinel_returns_copied_text_and_restores()
  #[test] fn copy_with_sentinel_unchanged_is_none()          // copy가 아무것도 안 하면 None, 이전 텍스트 복원
  #[test] fn copy_with_sentinel_identical_to_previous_still_counts()  // 이전 클립보드와 같은 텍스트를 복사해도 Some
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-win clipboard` → FAIL.
- [ ] **Step 3: 구현한다.** `Win32Clipboard`는 `OpenClipboard`를 재시도하고, `set_text`는 `EmptyClipboard`, `GlobalAlloc`, `SetClipboardData(CF_UNICODETEXT)` 순서로 쓴다. 이 세 단계 중 어디서 실패하든 핸들을 해제하고 `CloseClipboard`를 부른다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-win clipboard` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(win): 클립보드 보존·복원과 sentinel 복사`

---

### Task 6: URL 판정과 역할 분류

**Files:**
- Create: `crates/ct-win/src/chrome/{mod.rs,role.rs}`

**Interfaces:**
- Produces:
  ```rust
  pub enum Role { Compose, MessageHistory, Subject, Body, Cc, Bcc, To, Canvas, Textbox, SlideCanvas, Unknown }
  pub fn is_google_chat_url(url: &str) -> bool;
  pub fn is_gmail_url(url: &str) -> bool;            // "mail.google.com"
  pub fn is_docs_url(url: &str) -> bool;             // "docs.google.com/document"
  pub fn is_slides_url(url: &str) -> bool;           // "docs.google.com/presentation"
  pub fn is_supported_url(url: &str) -> bool;        // chat || mail.google.com || docs.google.com
  pub fn classify_focused_role(url: &str, name: &str, control_type: &str) -> Role;
  ```
  Sheets 역할은 계획 4에서 추가한다.

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `tests/unit/interaction/test_windows_role_classifier.py`의 Chat/Gmail/Docs/Slides 케이스를 모두 포팅하고(Sheets 제외), v5 참조 §2.5.2 규칙의 경계 사례를 넣는다.
  - Gmail 필드 이름이 `#chat` URL보다 우선한다.
  - "참조"는 "숨은참조"와 구분한다.
  - Chat에서는 `EditControl`이면 `Compose`다.
  - Docs의 `ButtonControl`은 `Unknown`이다.
  - Slides에서 "text box"/"shape"는 `Textbox`다.
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-win role` → FAIL.
- [ ] **Step 3: 구현한다.** 판정 순서는 v5 참조 §2.5.2와 같다(Gmail 이름 규칙 → Chat → Gmail unknown → Docs → Slides → unknown). 이름 비교는 영어는 소문자로, 한글은 원문 그대로 한다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-win role` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(win): Chrome URL 판정과 포커스 역할 분류`

---

### Task 7: Chrome 앱별 전략

**Files:**
- Create: `crates/ct-win/src/chrome/strategy.rs`

**Interfaces:**
- Consumes: `Role`, URL 판정 (Task 6)
- Produces:
  ```rust
  pub struct FocusedElement { pub url: String, pub role: Role, pub name: String, pub full_text: String, pub selection_text: Option<String> }
  #[derive(Clone, PartialEq, Eq, Debug)]
  pub struct EditingContext { pub app: &'static str, pub field_id: &'static str, pub has_selection: bool, pub text: String }
  pub enum Strategy { GoogleChat, Gmail, GoogleChatReadOnly, GmailReadOnly, GoogleDocs, GoogleSlides }
  impl Strategy {
      pub fn matches(&self, f: &FocusedElement) -> bool;
      pub fn identify_context(&self, f: &FocusedElement) -> Option<EditingContext>;
  }
  pub const REPLACE_STRATEGIES: [Strategy; 4];   // [GoogleChat, Gmail, GoogleDocs, GoogleSlides]
  pub const READ_ONLY_STRATEGIES: [Strategy; 4]; // [GoogleChatReadOnly, GmailReadOnly, GoogleDocs, GoogleSlides]
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `tests/unit/interaction/test_strategies.py`의 Chat/Gmail/Docs/Slides 케이스(Sheets 제외)를 모두 포팅하고, v5 참조 §2.5.1 표의 규칙을 확인한다.
  - Chat 교체는 `compose`에서만 한다.
  - Gmail은 `subject`/`body`만 쓰고, To/Cc/Bcc는 `None`이다.
  - 읽기 전용 전략은 선택이 있어야 하고, 역할과 상관없이 `field_id="selection"`이다.
  - Docs는 선택이 있어야 한다.
  - Slides는 두 경우 모두 `field_id="textbox"`이고, 선택이 없으면 `Textbox`/`SlideCanvas`이면서 full_text가 비어 있지 않아야 한다.
  - 공백만 있는 선택도 선택으로 본다.
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-win strategy` → FAIL.
- [ ] **Step 3: 구현한다.**
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-win strategy` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(win): Google 앱별 캡처 전략`

---

### Task 8: Chrome 어댑터와 캡처 재시도

**Files:**
- Create: `crates/ct-win/src/chrome/adapter.rs`

**Interfaces:**
- Consumes: Task 7
- Produces:
  ```rust
  pub trait UiaBackend: Send + Sync {
      fn focused_element(&self) -> Option<FocusedElement>;
      fn foreground_is_docs_or_slides(&self) -> bool;
      fn paste_over_selection(&self) -> Result<(), String>;
      fn paste_replacing_full_text(&self) -> Result<(), String>;
  }
  pub struct CapturedText { pub context: EditingContext }
  pub trait TextInteraction: Send + Sync {
      fn capture(&self) -> Option<CapturedText>;
      fn replace(&self, captured: &CapturedText, new_text: &str) -> Result<(), ReplaceError>;
  }
  #[derive(thiserror::Error, Debug, PartialEq)]
  pub enum ReplaceError { #[error("Nothing is focused anymore; aborting to protect original text")] NothingFocused,
      #[error("Focus moved to an unsupported app; aborting replace")] Unsupported,
      #[error("Editing context changed since capture; aborting replace to protect original text")] ContextChanged,
      #[error("{0}")] Paste(String) }
  pub struct ChromeAdapter { /* backend: Arc<dyn UiaBackend>, clipboard: Arc<dyn Clipboard>, strategies: &'static [Strategy] */ }
  pub struct RetryingInteraction<T: TextInteraction> { /* inner, attempts=5, delay=80ms */ }
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** 가짜 `UiaBackend`(호출 순서대로 `FocusedElement` 반환, paste 호출 기록)와 메모리 클립보드로 확인한다. v5 `tests/unit/interaction/test_chrome.py`, `test_retrying.py`를 포팅하고, 아래를 넣는다.
  ```rust
  #[test] fn capture_uses_first_matching_strategy_without_fallthrough()
  #[test] fn replace_aborts_when_focus_moved_to_other_field()   // Review Focus 1: Gmail subject로 캡처 → body로 바뀜 → ContextChanged, paste 0회
  #[test] fn replace_compares_only_app_and_field()              // 텍스트가 달라져도 같은 field면 교체
  #[test] fn replace_with_selection_pastes_over_selection()
  #[test] fn replace_without_selection_reselects_all_then_pastes()
  #[test] fn replace_puts_new_text_on_clipboard_during_paste_and_restores()
  #[test] fn retrying_capture_tries_five_times_then_none()
  #[test] fn retrying_replace_is_not_retried()
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-win adapter` → FAIL.
- [ ] **Step 3: 구현한다.** 교체 직전에는 `focused_element()`를 다시 불러 확인한다(v5 §2.5). 붙여넣기는 `run_with_temporary_content` 안에서 한다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-win adapter` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(win): Chrome 어댑터와 캡처 재시도`

---

### Task 9: Windows UIA 백엔드

**Files:**
- Create: `crates/ct-win/src/chrome/uia_backend.rs`

**Interfaces:**
- Consumes: Task 4 `KeySender`, Task 5 `Clipboard`/`copy_with_sentinel`, Task 6, Task 8 `UiaBackend`
- Produces: `pub struct WindowsUiaBackend; impl WindowsUiaBackend { pub fn new(keys: Arc<dyn KeySender>, clipboard: Arc<dyn Clipboard>) -> Self; } impl UiaBackend for WindowsUiaBackend`, `pub fn cursor_position() -> Option<(i32, i32)>`.

- [ ] **Step 1: 순수 결정 함수의 실패하는 테스트를 쓴다.**
  ```rust
  /// 툴바 포커스 복구 대상인가 (v5 §2.6 e)
  pub fn needs_toolbar_recovery(control_type: &str, role: Role, url: &str) -> bool;
  /// 선택이 없을 때 전체 텍스트를 복사할 역할인가 (Compose, Subject, Body, Textbox, SlideCanvas)
  pub fn full_text_fallback_allowed(role: Role) -> bool;
  ```
  ```rust
  #[test] fn recovery_only_for_button_on_history_or_unknown_in_supported_url()
  #[test] fn docs_canvas_never_uses_full_text_fallback()
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-win uia_backend` → FAIL.
- [ ] **Step 3: 구현한다.** v5 참조 §2.6의 단계를 그대로 따르되, 아래만 다르다.
  - 스레드마다 COM을 초기화한다: `CoInitializeEx(COINIT_MULTITHREADED)`를 thread_local 플래그로 한 번만 부른다. `CUIAutomation`도 스레드마다 만든다.
  - 포그라운드 창 클래스가 `Chrome_WidgetWin`으로 시작해야 한다.
  - omnibox URL은 `OmniboxViewViews` EditControl(깊이 20)의 ValuePattern에서 읽는다. 실패하면 Name, 그것도 실패하면 빈 문자열이다.
  - 포커스된 요소는 `GetFocusedElement`의 Name과 ControlType으로 읽는다. ControlType 숫자는 `EditControl`, `ButtonControl` 같은 이름으로 바꿔 `classify_focused_role`에 넘긴다.
  - 툴바 포커스 복구:
    - Docs/Slides는 `Chrome_RenderWidgetHostHWND` Pane(깊이 10)에 `SetFocus`만 한다. Esc는 보내지 않는다.
    - 그 밖에는 `wait_until_alt_released` 후 Esc를 보내고 100ms 기다린 뒤 다시 읽는다.
  - 선택 확인은 `copy_with_sentinel(Ctrl+C)`로 한다.
  - 선택이 없고 `full_text_fallback_allowed`이면 Ctrl+A, 200ms 대기 후 `copy_with_sentinel(Ctrl+C)`로 전체 텍스트를 읽는다(P2).
  - 붙여넣기:
    - `paste_over_selection`은 `wait_until_alt_released` 후 Ctrl+V를 보내고 500ms 기다린다(P5).
    - `paste_replacing_full_text`는 `wait_until_alt_released` 후 Ctrl+A, 200ms 대기, Ctrl+V, 500ms 대기 순서다.
  - 키는 모두 `wait_until_alt_released` 다음에 보낸다.
  - 커서 위치는 `GetCursorPos`로 읽는다.
  - 텍스트 내용은 로그에 남기지 않는다.
- [ ] **Step 4: 테스트·lint·빌드를 확인한다.** `cargo test -p ct-win`, `cargo clippy -p ct-win --all-targets -- -D warnings` → PASS. 실제 동작은 Task 18의 수동 테스트에서 본다.
- [ ] **Step 5: 커밋한다.** `feat(win): Windows UI Automation 백엔드`

---

### Task 10: 자동 시작 레지스트리와 단일 실행

**Files:**
- Create: `crates/ct-win/src/registry.rs`, `crates/ct-win/src/single_instance.rs`

**Interfaces:**
- Produces:
  ```rust
  pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
  pub const RUN_VALUE: &str = "ClaudeTranslator";
  pub fn autostart_command(is_release: bool, exe: &Path) -> Option<String>;   // release면 "\"{exe}\"", 아니면 None
  pub fn apply_autostart(enabled: bool, is_release: bool, exe: &Path);          // 실패는 warn 로그만
  pub struct SingleInstance;
  impl SingleInstance { pub fn acquire(name: &str) -> Result<Option<SingleInstance>, std::io::Error>; } // 이미 있으면 Ok(None)
  pub const SINGLE_INSTANCE_NAME: &str = r"Local\ClaudeTranslator.SingleInstance";
  pub const ALREADY_RUNNING_MESSAGE: &str = "ClaudeTranslator가 이미 실행 중입니다.";
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.**
  ```rust
  #[test] fn command_is_quoted_only_in_release()   // Some("\"C:\\a b\\x.exe\""), debug → None
  #[test] fn second_acquire_returns_none()        // 테스트 전용 고유 이름으로 두 번 acquire
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-win registry single_instance` → FAIL.
- [ ] **Step 3: 구현한다.**
  - 레지스트리: 켜면 `RegSetKeyValueW(HKCU, RUN_KEY, RUN_VALUE, REG_SZ, …)`, 끄면 `RegDeleteKeyValueW`(값이 없으면 무시). release가 아니면 아무것도 하지 않는다.
  - 단일 실행: `CreateMutexW`로 만들고, `GetLastError() == ERROR_ALREADY_EXISTS`면 `Ok(None)`. Drop될 때 핸들을 닫는다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-win` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(win): 자동 시작 레지스트리와 단일 실행 보장`

---

### Task 11: CLI 프로세스 트리 종료와 stderr 수집 (계획 1 후속 R12)

**Files:**
- Modify: `crates/ct-llm/src/spawner.rs`, `crates/ct-llm/src/client.rs`, `crates/ct-llm/Cargo.toml` (windows feature: `Win32_System_JobObjects`, `Win32_System_Threading`, `Win32_Foundation`)

**Interfaces:**
- Produces:
  - `RealSpawner`가 띄운 자식 프로세스를 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` Job에 넣는다. `RealChild::terminate`는 `TerminateJobObject`로 트리 전체를 종료한다(`#[cfg(windows)]`).
  - stderr를 파이프로 받는다. 별도 스레드가 마지막 4KB만 링 버퍼에 보관한다.
  - 프로세스가 끝나서 `CliError::ProcessEnded`를 낼 때는 종료 코드와 stderr 끝부분의 길이만 `tracing::warn!`으로 남긴다(내용은 남기지 않음).
  - `ChildHandle`에 `fn stderr_tail_len(&self) -> usize`를 추가한다. 가짜 child는 0을 돌려준다.

- [ ] **Step 1: 실패하는 테스트를 쓴다.**
  ```rust
  #[test] fn stderr_ring_keeps_last_4096_bytes()   // StderrTail::push 순수 테스트
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-llm stderr` → FAIL.
- [ ] **Step 3: 구현한다.** Job 생성이나 할당에 실패해도 프로세스 실행은 계속하고 warn만 남긴다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-llm` → PASS. 가능하면 PowerShell에서 live 테스트를 1회 돌려 회귀가 없는지 보고 결과를 그대로 보고한다.
- [ ] **Step 5: 커밋한다.** `fix(llm): CLI 프로세스 트리 종료(Job Object)와 stderr 끝부분 수집`

---

### Task 12: 작업 흐름 (Alt+Alt / Ctrl+Ctrl)

**Files:**
- Create: `crates/ct-app/src/flows.rs` (ct-app을 lib + bin으로 바꾼다: `src/lib.rs`에 `pub mod flows; pub mod dispatch;`)

**Interfaces:**
- Consumes: `ct_core::{translate_text, explain_text, Translated, TranslateError, UserSettings, DictionaryEntry, LanguageDetector, LlmProvider, ExplanationResult, DICTIONARY_INTEGRITY_MESSAGE}`, `ct_win::chrome::{TextInteraction, CapturedText}`
- Produces:
  ```rust
  pub trait Notifier: Send + Sync { fn notify(&self, message: &str); }           // 설명 창에 "Error: {message}"
  pub trait Badge: Send + Sync { fn show(&self, text: &str); fn hide(&self); }
  pub trait ExplainView: Send + Sync {
      fn show_loading(&self, text: &str);
      fn show_error(&self, message: &str);
      fn show_result(&self, result: ExplanationResult, mode: DisplayMode);
  }
  pub struct FlowDeps<'a> { pub settings: &'a UserSettings, pub entries: &'a [DictionaryEntry],
      pub detector: &'a dyn LanguageDetector, pub llm: &'a dyn LlmProvider }
  pub fn alt_alt_flow(d: &FlowDeps, ti: &dyn TextInteraction, docs_or_slides: &dyn Fn() -> bool,
                      badge: &dyn Badge, notifier: &dyn Notifier);
  pub fn ctrl_ctrl_flow(d: &FlowDeps, ti: &dyn TextInteraction, view: &dyn ExplainView);
  pub const MSG_DIRECTION_UNKNOWN: &str = "번역 방향을 판단할 수 없어 원문을 변경하지 않았습니다.";
  pub const MSG_TRANSLATE_FAILED: &str = "번역에 실패하여 원본을 변경하지 않았습니다.";
  pub const MSG_REPLACE_FAILED: &str = "교체 대상이 바뀌어 원문을 변경하지 않았습니다.";
  pub const MSG_ALT_NO_SELECTION_DOCS: &str = "텍스트를 선택한 후 Alt+Alt를 눌러주세요.";
  pub const MSG_CTRL_NO_SELECTION: &str = "텍스트를 선택한 후 Ctrl+Ctrl을 실행해주세요.";
  pub const MSG_EXPLAIN_FAILED: &str = "Could not translate/explain the selected text";
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** 가짜 TextInteraction/Badge/Notifier/ExplainView, ct-core의 `FakeLlm` 대용(이 크레이트 테스트 안에 간단한 가짜 `LlmProvider`), `RuleBasedDetector`로 확인한다.
  ```rust
  #[test] fn alt_alt_replaces_with_targets_in_order()             // targets [Ja, En], 원문 포함 꺼짐 → "JA\n\nEN" 교체
  #[test] fn alt_alt_includes_original_when_enabled()             // "원문\n\nEN\n\nJA"
  #[test] fn alt_alt_target_input_replaces_with_original_and_base_only()  // Review Focus 4: 영어 입력, targets [En, Ja], 원문 포함 → "Hello\n\n안녕"
  #[test] fn alt_alt_badge_shown_before_capture_and_hidden_after()   // 호출 순서: badge.show("Translating...") → capture → … → badge.hide()
  #[test] fn alt_alt_capture_failure_silent_outside_docs_slides()
  #[test] fn alt_alt_capture_failure_on_docs_notifies()          // MSG_ALT_NO_SELECTION_DOCS
  #[test] fn alt_alt_translate_failures_notify_and_never_replace() // DirectionUnknown/Llm/Integrity → 각 문구, replace 0회
  #[test] fn alt_alt_replace_failure_notifies()                   // MSG_REPLACE_FAILED
  #[test] fn ctrl_ctrl_shows_loading_only_after_capture()         // capture 전에는 view 호출 없음
  #[test] fn ctrl_ctrl_capture_failure_shows_no_selection()
  #[test] fn ctrl_ctrl_integrity_and_failure_messages()           // DICTIONARY_INTEGRITY_MESSAGE / MSG_EXPLAIN_FAILED
  #[test] fn ctrl_ctrl_result_passes_display_mode()
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-app flows` → FAIL.
- [ ] **Step 3: 구현한다.**
  - 교체 텍스트는 `Translated::replacement_text(&captured.context.text, settings.include_original_on_replace)`로 만든다.
  - 배지 해제는 함수 끝에서 항상 실행되게 한다(guard 패턴).
  - 로그에는 작업 이름, 소요 시간, 오류 종류만 남긴다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-app flows` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(app): Alt+Alt·Ctrl+Ctrl 작업 흐름`

---

### Task 13: 디스패처 (가드, 일시정지, 설정 스냅샷)

**Files:**
- Create: `crates/ct-app/src/dispatch.rs`
- Modify: `crates/ct-app/Cargo.toml`. slint, tray-icon, windows, raw-window-handle을 일반 의존성으로 옮기고, `ct-core`, `ct-llm`, `ct-win`, `tracing`, `tracing-subscriber`, `tracing-appender`, `crossbeam-channel`을 넣는다. build-dependency로 `slint-build`를 넣는다.

**Interfaces:**
- Consumes: `Gesture` (Task 2), flows (Task 12)
- Produces:
  ```rust
  pub struct AppState { pub settings: RwLock<UserSettings>, pub entries: RwLock<Vec<DictionaryEntry>>,
                        pub llm: RwLock<Arc<dyn LlmProvider>>, pub paused: AtomicBool }
  pub trait GestureAction: Send + Sync { fn run(&self, gesture: Gesture, snapshot: Snapshot); }
  pub struct Snapshot { pub settings: UserSettings, pub entries: Vec<DictionaryEntry>, pub llm: Arc<dyn LlmProvider> }
  pub struct Dispatcher;
  impl Dispatcher {
      pub fn new(state: Arc<AppState>, action: Arc<dyn GestureAction>) -> Self;
      /// 훅 스레드에서 호출. 일시정지면 무시. 같은 계열(Alt: AltAlt·CtrlHeldAltAlt / Ctrl: CtrlCtrl·AltHeldCtrlCtrl)이
      /// 실행 중이면 버린다. 아니면 그 순간의 스냅샷을 잡아 새 스레드에서 catch_unwind로 실행한다.
      pub fn dispatch(&self, gesture: Gesture);
  }
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.**
  ```rust
  #[test] fn same_family_while_running_is_dropped()
  #[test] fn alt_and_ctrl_families_run_concurrently()
  #[test] fn paused_drops_gesture()
  #[test] fn panic_in_action_releases_guard()             // 다음 dispatch는 실행됨
  #[test] fn dispatch_snapshots_settings_at_trigger()     // Review Focus 5: 실행 중 settings를 바꿔도 action이 받은 스냅샷은 그대로
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-app dispatch` → FAIL.
- [ ] **Step 3: 구현한다.** 계열마다 `AtomicBool` 가드를 두고, 스레드가 끝날 때(패닉 포함) 해제한다. 교차 조합 제스처는 계획 3 전까지 `GestureAction` 구현에서 `tracing::info!("not available yet")`만 남긴다.
- [ ] **Step 4: 테스트가 통과하는지 확인한다.** `cargo test -p ct-app dispatch` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(app): 단축키 디스패처`

---

### Task 14: 배지, 설명 창, Win32 창 보조 함수

**Files:**
- Create: `crates/ct-app/src/ui/{mod.rs,win32.rs,badge.rs,explain.rs}`, `crates/ct-app/ui/{badge.slint,explain.slint}`, `crates/ct-app/build.rs`

**Interfaces:**
- Consumes: flows의 `Badge`, `ExplainView`, `Notifier` trait (Task 12)
- Produces:
  ```rust
  // win32.rs
  pub fn hwnd_of(win: &slint::Window) -> Result<HWND, String>;
  pub fn wait_for_hwnd(get: impl Fn() -> Result<HWND, String> + 'static, tries: u32, done: impl FnOnce(Result<HWND, String>) + 'static);
  pub fn set_no_activate_topmost(hwnd: HWND);
  pub fn show_no_activate(hwnd: HWND);
  pub fn bring_to_front(hwnd: HWND);        // HWND_TOPMOST → SetForegroundWindow → HWND_NOTOPMOST (v5 §4.6, 스파이크 S3)
  pub fn message_box(title: &str, text: &str);   // 별도 스레드에서 MessageBoxW(MB_ICONWARNING|MB_TOPMOST|MB_SETFOREGROUND), 비모달
  // explain.rs
  pub struct ExplainViewState { pub status: Option<String>, pub blocks: Vec<ExplainBlock> }   // 순수 뷰 상태
  pub struct ExplainBlock { pub original: String, pub translations: Vec<(Option<&'static str>, String)> } // 언어 표시(EN/JA)는 번역 2개 이상일 때만 Some
  pub fn build_view_state(result: &ExplanationResult, mode: DisplayMode) -> ExplainViewState;
  pub struct ExplainWindow;    // impl ExplainView + Notifier; 다른 스레드에서 호출하면 slint::invoke_from_event_loop로 넘김
  pub struct BadgeWindow;      // impl Badge
  ```

- [ ] **Step 1: 뷰 상태의 실패하는 테스트를 쓴다.**
  ```rust
  #[test] fn whole_text_joins_originals_and_each_language_column()  // 원문 "\n" 연결, 언어별 "\n" 연결, 순서 = Target 순서
  #[test] fn per_sentence_one_block_per_sentence()
  #[test] fn language_tags_only_when_two_or_more()
  #[test] fn error_status_has_prefix()       // "Error: " + message
  #[test] fn explain_view_reuses_single_window()  // Review Focus 3: show_result 두 번 → 상태 교체, 창 생성 1회(생성 카운터)
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-app explain` → FAIL.
- [ ] **Step 3: 구현한다.**
  - **설명 창**:
    - v5 참조 §4.6을 따른다: 테두리 없음, 배경 `#FBEEEC`, 모서리 14, 헤더 `ClaudeTranslator`와 ✕, 푸터 `닫기 Esc`.
    - 카드는 흰색, 원문은 굵게 `#2B2B2B`, 번역은 굵게 `#2563EB`, 언어 표시는 작은 회색 `#8A8A8A`다.
    - 최소 480×240이다. 결과를 표시할 때만 크기를 조절한다(최대 700×800, 화면 크기 이내).
    - 스크롤, Esc로 닫기(숨김), 드래그 이동, 가장자리 6px 크기 조절을 지원한다. 드래그와 크기 조절은 `slint::winit_030::WinitWindowAccessor`의 `drag_window`/`drag_resize_window`를 쓴다.
    - 창은 하나만 만들어 재사용한다.
    - 로딩·오류 표시 때는 `bring_to_front`를 부른다. 결과 표시 때는 다시 띄우지 않는다(v5).
  - **배지**: 스파이크 방식으로 만든다. 시작할 때 한 번 화면 밖에 띄워 HWND를 얻고, 숨긴 상태에서 no-activate·topmost·toolwindow 스타일을 적용한다. 표시는 `SW_SHOWNOACTIVATE`로 한다.
    - 위치는 커서 +16/+16에서, 커서가 있는 모니터의 작업 영역 안으로 제한한다.
    - 스타일은 `background #fff3cd; color #333`이다.
- [ ] **Step 4: 테스트·빌드를 확인한다.** `cargo test -p ct-app`, `cargo build -p ct-app` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(app): 배지와 번역 보기 창`

---

### Task 15: 트레이

**Files:**
- Create: `crates/ct-app/src/ui/tray.rs`

**Interfaces:**
- Consumes: `ct_core::{toggle_target, Lang, SettingsStore}`
- Produces:
  ```rust
  pub struct TargetMenuItem { pub lang: Lang, pub label: String, pub checked: bool }
  /// base를 뺀 언어를 [En, Vi, Ja, Ko] 중에서 고른다. 체크된 언어는 "en (1)"처럼 순서 번호를 붙인다.
  pub fn target_menu_items(base: Lang, targets: &[Lang]) -> Vec<TargetMenuItem>;
  pub enum TrayCommand { ToggleTarget(Lang), OpenDictionary, OpenSettings, SetPaused(bool), Exit }
  pub struct Tray;   // tray-icon + muda. 메뉴 이벤트를 TrayCommand로 바꿔 메인 스레드 채널로 보낸다. refresh(settings)로 라벨·체크 갱신(P6)
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.**
  ```rust
  #[test] fn menu_excludes_base_and_numbers_checked_in_order()  // base Ko, targets [Ja, En] → en (2)✓, vi, ja (1)✓
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-app tray` → FAIL.
- [ ] **Step 3: 구현한다.**
  - 메뉴 순서: `Target Language`▸, `Personal Dictionary`, `Settings`, `Pause Translation`(체크형), `Exit`.
  - 툴팁은 `ClaudeTranslator`다.
  - 아이콘은 32px 투명 바탕에 `#2563EB` 원과 흰색 `CT`다. 코드로 RGBA를 만든다. 글자 렌더링이 어려우면 원만 그리고 보고서에 적는다.
  - `ToggleTarget`은 `toggle_target` 결과를 `SettingsStore::update`로 저장하고 `refresh`한다. 마지막 남은 언어는 풀 수 없다.
- [ ] **Step 4: 테스트·빌드를 확인한다.** `cargo test -p ct-app tray`, `cargo build -p ct-app` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(app): 트레이 메뉴와 다중 Target 체크`

---

### Task 16: Settings 창

**Files:**
- Create: `crates/ct-app/src/ui/settings.rs`, `crates/ct-app/ui/settings.slint`

**Interfaces:**
- Consumes: `ct_core::{UserSettings, normalize_targets, move_target, toggle_target, with_base, Tone, DisplayMode, CorrectionMode}`, `ct_llm::{ModelCatalog, CatalogSource, ModelInfo, resolve_claude_cli_path}`
- Produces:
  ```rust
  pub struct SettingsForm { pub settings: UserSettings }   // 순수 뷰모델
  pub struct ComboItem { pub label: String, pub value: String }
  impl SettingsForm {
      pub fn target_rows(&self) -> Vec<(Lang, bool, Option<usize>)>;  // base 제외, 체크된 언어가 먼저(순서대로), 순서 번호
      pub fn set_base(&mut self, base: Lang);
      pub fn toggle(&mut self, lang: Lang);
      pub fn move_up(&mut self, lang: Lang); pub fn move_down(&mut self, lang: Lang);
      pub fn can_save(&self) -> bool;       // target 1개 이상
      pub fn model_items(&self, models: &[ModelInfo]) -> Vec<ComboItem>;   // default → "기본값 (자동)"/""; 목록에 없는 저장값 → "(목록에 없음) {v}"
      pub fn effort_items(&self, models: &[ModelInfo]) -> Vec<ComboItem>;  // "기본값 (자동)"/"" + low→Low, medium→Medium, high→High, xhigh→X-High, max→Max
      pub fn effort_supported(&self, models: &[ModelInfo]) -> bool;
  }
  pub const SHORTCUT_GUIDE: &[&str];   // v5 §4.4의 6줄 + "Target 언어가 여러 개면 정한 순서대로 모두 번역합니다."
  pub const INCLUDE_ORIGINAL_LABEL: &str = "Alt+Alt 교체 시 원문 포함";
  pub const INCLUDE_ORIGINAL_DESC: &str = "켜면 입력한 원문 아래에 빈 줄을 두고 번역문을 넣습니다.";
  pub const NO_TARGET_MESSAGE: &str = "Target 언어를 하나 이상 선택하세요.";
  pub struct SettingsWindow;   // 하나만 열림. 저장 → on_saved 콜백 → 창 닫기
  ```

- [ ] **Step 1: 뷰모델의 실패하는 테스트를 쓴다.** v5 `tests/unit/ui/test_settings_dialog.py`의 모델·effort 규칙 케이스를 포팅하고, 아래를 넣는다.
  ```rust
  #[test] fn target_rows_checked_first_with_numbers()
  #[test] fn set_base_removes_it_from_targets_and_falls_back()
  #[test] fn cannot_save_without_target()          // 마지막 하나를 풀 수 있는 UI라면(Settings는 허용) can_save=false
  #[test] fn unknown_saved_model_is_appended_not_swapped()
  #[test] fn haiku_has_no_effort()                  // effort_supported=false, effort_items는 "기본값 (자동)"만
  #[test] fn model_default_maps_to_empty_value()    // 계획 1 후속: "default" → ""
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-app settings` → FAIL.
- [ ] **Step 3: 구현한다.** 화면은 v5 참조 §4.4의 카드 구성과 문구를 따르고, 아래를 더한다.
  - 일반 카드의 Target Languages 행은 체크박스와 ▲/▼ 버튼으로 만든다.
  - `INCLUDE_ORIGINAL_LABEL` 체크박스와 설명을 넣는다.
  - 자동 시작 체크박스 문구는 `Start ClaudeTranslator when I log in`이다.
  - Model 목록은 `ModelCatalog` 리스너로 갱신한다. 메인 스레드로 넘겨 선택을 유지하고, 폴백 목록이면 안내 문구를 보인다.
  - 사용자가 haiku를 고르면 `message_box("모델 선택", "길거나 복잡한 문장에서 성능이 저하될 수 있습니다.")`를 띄운다. 처음 불러올 때나 목록이 갱신될 때는 띄우지 않는다.
  - Target이 없으면 Save 버튼을 비활성화하고 `NO_TARGET_MESSAGE`를 보인다.
  - Save는 `SettingsStore::update` 후 `on_saved`를 부르고 창을 닫는다.
- [ ] **Step 4: 테스트·빌드를 확인한다.** `cargo test -p ct-app settings`, `cargo build -p ct-app` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(app): Settings 창 (다중 Target 순서, 원문 포함)`

---

### Task 17: 개인 사전 창

**Files:**
- Create: `crates/ct-app/src/ui/dictionary.rs`, `crates/ct-app/ui/dictionary.slint`

**Interfaces:**
- Consumes: `ct_core::{PersonalDictionaryStore, DictionaryEntry, Action, DictionaryError}`
- Produces:
  ```rust
  pub struct DictionaryForm { pub term: String, pub action: Action, pub ko: String, pub en: String, pub vi: String, pub ja: String }
  impl DictionaryForm { pub fn to_entry(&self) -> Result<DictionaryEntry, String>; pub fn from_entry(e: &DictionaryEntry) -> Self; }
  pub fn error_message(e: &DictionaryError) -> String;   // v5 §4.5 문구
  pub const SELECT_TO_UPDATE: &str = "Select an entry to update first";
  pub const SELECT_TO_DELETE: &str = "Select an entry to delete first";
  pub const EMPTY_TERM: &str = "Term을 입력하세요.";
  pub struct DictionaryWindow;   // 하나만 열림. 변경할 때마다 on_changed(entries) 콜백
  ```

- [ ] **Step 1: 실패하는 테스트를 쓴다.**
  ```rust
  #[test] fn form_keeps_only_non_empty_translations()
  #[test] fn translate_without_translations_is_error()  // "TRANSLATE entry for term '{t}' requires at least one translation"
  #[test] fn empty_term_is_rejected()                    // EMPTY_TERM (v5에는 없던 검증, 계획 결정)
  #[test] fn duplicate_message_matches_v5()              // "Term '{t}' already exists in the personal dictionary"
  ```
- [ ] **Step 2: 테스트가 실패하는지 확인한다.** `cargo test -p ct-app dictionary` → FAIL.
- [ ] **Step 3: 구현한다.**
  - 창 제목은 `ClaudeTranslator - Personal Dictionary`다.
  - 표의 열은 `Term`, `Action`, `Translations`다(번역 값을 `, `로 연결).
  - 입력 행은 Term, Action(KEEP/TRANSLATE), ko/en/vi/ja 순서다. 그 아래 오류 문구, 버튼 `Add`/`Update`/`Delete`를 둔다.
  - 행을 고르면 입력 칸을 채운다.
  - 변경할 때마다 `on_changed(store.entries().to_vec())`를 불러 `AppState.entries`를 바꾼다. 다음 번역부터 바로 반영된다.
- [ ] **Step 4: 테스트·빌드를 확인한다.** `cargo test -p ct-app dictionary`, `cargo build -p ct-app` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(app): 개인 사전 창`

---

### Task 18: 조립, 로그, 시작·종료, 수동 테스트 문서

**Files:**
- Create: `crates/ct-app/src/logging.rs`, `tests/manual/plan2_app.md`
- Modify: `crates/ct-app/src/main.rs`

**Interfaces:**
- Consumes: 모든 이전 작업

- [ ] **Step 1: 시작 순서를 구현한다** (`main.rs`). v5 참조 §5.1을 따르되, 아래와 같이 바꾼다.
  1. `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`
  2. `SingleInstance::acquire(SINGLE_INSTANCE_NAME)`. 이미 실행 중이면 `message_box("ClaudeTranslator", ALREADY_RUNNING_MESSAGE)` 후 종료한다(P4).
  3. 데이터 폴더를 만들고 `logging::init(data_dir)`를 부른다. release면 `logs/`에 일자별 파일, debug면 콘솔이다. `LOG_LEVEL` 기본값은 INFO다. 패닉 훅은 `logs/crash.log`에 위치와 메시지만 덧붙인다.
  4. `load_app_config_from_environment(exe_dir, data_dir)`, `SettingsStore::load`, `PersonalDictionaryStore::load`를 부른다. 문제 메시지는 모아 둔다.
  5. `apply_autostart(settings.start_on_login, cfg!(not(debug_assertions)), current_exe)`
  6. `ModelCatalog::new(data_dir/models_cache.json)`. `build_llm_provider`의 overrides는 Settings 값이고, model이 `"default"`면 `""`로 넘긴다. 그다음 백그라운드 스레드에서 `refresh_catalog_and_warm_up`을 부른다.
  7. `Win32KeySender`, `Win32Clipboard`, `WindowsUiaBackend`, 교체용 `RetryingInteraction<ChromeAdapter(REPLACE_STRATEGIES)>`, 보기용 `RetryingInteraction<ChromeAdapter(READ_ONLY_STRATEGIES)>`를 만든다.
  8. Slint 창(설명 창, 배지)과 트레이를 만든다.
  9. `Dispatcher`와 `GestureAction`을 만든다. AltAlt는 `alt_alt_flow`, CtrlCtrl은 `ctrl_ctrl_flow`, 나머지는 로그만 남긴다.
  10. `ChordRouter`와 `KeyboardHook::start`를 만든다. 훅 콜백은 `router.on_key`의 결과를 `dispatcher.dispatch`로 넘긴다. 훅 설치가 실패하면 `message_box`로 알리고 계속 실행한다(트레이·Settings는 쓸 수 있음).
  11. 파일 문제가 있으면 `file_problems_warning`을 비모달 메시지로 띄운다.
  12. `slint::run_event_loop_until_quit()`. 종료할 때 `hook.stop()` 후 CLI client를 `close()`한다.
  - 트레이 명령 처리(50ms 타이머 폴링):
    - Settings 저장(`on_saved`)이면 다음을 한다.
      - 새 provider를 만들고 warm-up한다. `AppState.llm`을 교체한 뒤 이전 client를 close한다.
      - `apply_autostart`를 다시 부른다.
      - 간격 `AtomicU64` 두 개를 갱신한다(P1).
      - `tray.refresh`를 부른다.
- [ ] **Step 2: 수동 테스트 문서를 쓴다** (`tests/manual/plan2_app.md`). 각 항목에 절차, 기대 결과, 빈 결과 칸을 둔다.
  - A. 시작: 트레이 아이콘, 두 번째 실행 시 안내.
  - B. Gmail 본문(선택 있음/없음), 제목, To 칸(교체 안 됨).
  - C. Chat 작성창, 대화 내역 선택(Alt+Alt 교체 안 됨, Ctrl+Ctrl 보기 됨).
  - D. Slides 텍스트 상자(선택 있음/없음).
  - E. Docs 블록 선택 교체, 선택 없을 때 안내.
  - F. 다중 Target [en, ja] 순서와 [ja, en] 순서, 원문 포함 켜짐/꺼짐, 영어 입력 시 한국어만.
  - G. Ctrl+Ctrl whole_text/per_sentence, 언어 표시, 창 재사용, Esc, 드래그·크기 조절.
  - H. 트레이 Target 체크와 순서 번호, Settings에서 ▲/▼ 후 트레이 반영, 저장 즉시 단축키 간격 반영.
  - I. 개인 사전 추가 후 바로 번역에 반영.
  - J. 일시정지.
  - K. Claude CLI가 없을 때(경로를 잘못 지정): 앱이 죽지 않고 번역 실패를 안내.
  - L. 배지가 포커스를 뺏지 않음.
  - M. Alt 단독 탭 후 Chrome 메뉴에 포커스가 가지 않음.
  - N. 3개 언어 응답 시간 기록.
- [ ] **Step 3: 빌드·테스트를 확인한다.** `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, `cargo build -p ct-app --release` → PASS. 실행 화면은 확인하지 않았다고 보고하고, 사용자에게 `tests/manual/plan2_app.md` 실행을 요청한다.
- [ ] **Step 4: 커밋한다.** `feat(app): 앱 조립과 시작·종료, 계획 2 수동 테스트 문서`
