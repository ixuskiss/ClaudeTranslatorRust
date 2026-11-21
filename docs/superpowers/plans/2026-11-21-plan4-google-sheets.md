# ClaudeTranslator 계획 4: Google Sheets

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Google Sheets에서 네 단축키가 동작하게 한다. 셀 범위를 선택하면 Alt+Alt는 범위 일괄 번역 교체, Ctrl+Alt+Alt는 범위 일괄 교정 교체, Ctrl+Ctrl은 번역 표 보기, Alt+Ctrl+Ctrl은 교정 표 보기를 한다. 셀을 편집 중이면 선택 글자(또는 셀 전체)를 기존 텍스트 흐름으로 처리한다. 다중 Target과 "원문 포함"을 적용한다.

**Architecture:**
- 새 크레이트 `ct-sheets`(Windows 의존 없음): 셀 모델, 라우팅, 주소·지원 판정, CF_HTML·Chromium 사용자 정의 클립보드 코덱, Sheets HTML 파서, 교체 HTML 조립, 쓰기 후 검증, 범위 작업 서비스(`replace_cells`, `capture_for_table`)를 둔다. Windows 쪽은 `SheetsInteraction` trait 뒤에 숨긴다.
- `ct-core`: 배치 번역·교정 서비스와 `LlmProvider` 배치 메서드(기본 구현은 "지원하지 않음" 오류)를 더한다. 셀마다 감지 언어가 달라 항목마다 목적 언어 목록을 따로 둔다.
- `ct-llm`: 배치 프롬프트와 응답 검증, Mock 배치 응답.
- `ct-win`: 다중 포맷 클립보드와 가드, Sheets 역할 분류, 셀 편집 캡처와 `GoogleSheetsCellEdit` 전략, `SheetsEditSafe` 래퍼, `WindowsSheetsBackend`(이름 상자 읽기, 범위 복사·붙여넣기).
- `ct-app`: 단축키마다 먼저 Sheets 라우팅을 하고, 범위면 범위 흐름, 편집 중이면 기존 텍스트 흐름, 그 밖의 Sheets 상태면 안내를 띄운다. 설명 창에 표 보기를 더한다.

**Tech Stack:** 계획 2·3과 같다. 새 외부 의존성은 없다(`ct-sheets`는 `serde_json`, `thiserror`, `tracing`, `ct-core`만 쓴다).

**Spec:** `docs/superpowers/specs/2026-11-20-claude-translator-rust-design.md` (§1.2, §1.4, §2.6, §2.7, §4.1, §4.4, §5.3, §6 "Sheets", §7.1, §9 단계 7)

**v5 동작 참조(필독):**
- `docs/superpowers/plans/v5-sheets-reference.md` (이하 **S-ref**): Sheets 전체. 문자열·상수·순서는 여기 값을 그대로 쓴다.
- `docs/superpowers/plans/v5-windows-ui-reference.md` (이하 **UI-ref**): §2.3(다중 포맷 클립보드·가드), §3.4(`run_sheets`), §3.5(SheetsOutcome 문구), §5.3~§5.6(단축키별 Sheets 문구).
- v5 원본 `C:\python_workspace\ClaudeLangTrans_v5`는 읽기 전용이다. 참조 문서로 부족할 때만 연다.

**범위 밖:** 마무리 단계(exe 이름 `ClaudeTranslator.exe`, 아이콘·버전 리소스, 정적 CRT, 사용자 가이드, 배포, 계획 2 후속 항목)는 계획 5로 넘긴다(결정 D1).

## Global Constraints

- 계획 1~3의 공통 규칙을 따른다: 테스트 밖 `unwrap`/`expect` 금지, 커밋은 한국어 메시지 + 작성 모델의 Co-Authored-By.
- 셀 텍스트, 클립보드 내용, 지문(fingerprint)은 로그에 남기지 않는다. 개수, 상태, 오류 종류, 파서의 고정 오류 문구만 남긴다(S-ref §10.17).
- 합성 키는 Ctrl+C, Ctrl+A, Ctrl+V, Esc, F15뿐이다. Enter, Ctrl+Enter, Ctrl+Shift+V는 절대 보내지 않는다.
- `ct-sheets`, `ct-core`, `ct-llm`은 Windows 없이 빌드·테스트된다.
- 클립보드 구간 잠금(`lock_clipboard_section`)은 재진입되지 않는 `Mutex`다. Sheets 백엔드의 공개 메서드(`capture_selection`, `write_selection`)는 각각 잠금을 한 번만 잡고, 잠금을 잡은 채 다른 공개 메서드를 부르지 않는다. 라우팅(`url_and_state`)은 잠금을 잡지 않는다. LLM 호출 중에는 잠금을 잡지 않는다.
- 글자 수는 모두 문자(`chars().count()`) 단위다. CF_HTML 오프셋만 UTF-8 바이트 단위다(S-ref §2.4).
- 단일 쓰기 원칙: 범위 쓰기는 한 번만 한다. 재시도, 다시 붙여넣기, 자동 Undo를 하지 않는다(S-ref §10.9).
- 사용자 문구는 UI-ref §3.5, §5.3, §5.4, §5.5와 S-ref의 문자열을 글자 그대로 쓴다. 이 계획에서 새로 정한 문구는 없다.
- 기본값: `SheetsConfig`(`ct-core/src/config.rs`) 그대로. `batch_max_cells` 30, `batch_max_chars` 5000, `max_cell_chars` 5000, `max_range_cells` 200, `job_timeout` 240초, `range_replace_enabled`·`edit_full_cell_replace_enabled` 켜짐.

## v5와 다르게 정한 것 (결정 기록)

- D1: 원래 계획 4였던 "Sheets와 마무리"를 나눈다. 이 계획은 Sheets만, 마무리는 계획 5. 각각 독립적으로 테스트 가능한 결과를 낸다.
- D2: 배치 번역은 다중 Target을 쓴다(스펙 §2.6). 요청 항목마다 `targets` 목록, 응답은 `{"translations": [{"id": "c0", "texts": {"en": "...", "ja": "..."}}]}`. 항목의 언어 키는 그 항목의 `targets`와 정확히 같아야 한다.
- D3: 범위 교체에서 셀마다 `compose_replacement(원문 포함이면 원문, 번역들)`로 셀 내용을 만든다. 묶음 나누기 크기는 `글자 수 × (목적 언어 수 + 원문 포함이면 1)`(스펙 §2.7). 조립한 셀 내용이 `max_cell_chars`를 넘으면 쓰기 전에 작업 전체를 `TRANSLATION_FAILED(CELL_TOO_LARGE)`로 중단한다.
- D4: 지문은 SHA-256 대신 셀 튜플 목록을 그대로 비교한다(같은 프로세스 안에서만 비교하므로 동등, S-ref §4.4). 해시 크레이트를 쓰지 않는다.
- D5: HTML은 직접 만든 작은 토크나이저로 읽는다. 문자 참조는 숫자 참조와 `&amp; &lt; &gt; &quot; &apos; &#39; &nbsp;`만 풀고, 모르는 이름 참조는 글자 그대로 둔다. compact 데이터가 있으면 HTML과 값이 다를 때 캡처가 실패하므로 안전하다.
- D6: v5에서 잡지 않던 transform 단계의 LLM 외 오류(패닉 아님, `Err`)는 failure 상태로 바꾼다(S-ref §10.11).
- D7: 쓰이지 않던 v5 코드는 옮기지 않는다: `build_replacement_tsv`, `editor_rich_text_state`, `allow_rich_text`, `sheets_select_all_enabled`, `sheet_name`(항상 없음, 캡션은 `gid N · 주소`).
- D8: 범위 교체가 Sheets가 아닌 경로처럼 조용히 넘어가는 경우는 없다. 범위 결과가 SUCCESS가 아니면 항상 안내한다(v5와 같음). 셀 편집 경로의 번역 실패도 계획 2처럼 안내한다(v5는 조용함).
- D9: 붙여넣기 확인(3초 안에 클립보드 변화)이 안 되면 v5처럼 사용자 클립보드를 복원하지 않고 검증 단계로 넘어간다.
- D10: `ReplaceError`에 `FormulaPrefix`, `FullCellNotEnabled`를 더한다. Sheets 셀 편집에서 재확인 불일치(`target_changed`)는 기존 `ContextChanged`를 쓴다.

## Review Focus

1. 언어가 섞인 범위(한국어 셀 + 영어 셀, Target en·ja): 한국어 셀은 en·ja 모두, 영어 셀은 ko만 받는다. (Task 5 `mixed_language_cells_get_per_item_destinations`)
2. 숫자·날짜가 있는 최신 Sheets 복사본: 패닉 없이 `CAPTURE_FAILED`로 끝나고 안내가 뜬다. (Task 3 `compact_number_type_rejects_capture`, Task 8 `parse_error_is_capture_failed`)
3. LLM을 기다리는 동안 사용자가 탭을 바꾸거나 셀을 고침: 아무것도 쓰지 않고 `TARGET_CHANGED`. (Task 8 `reread_differs_is_target_changed_and_never_writes`)
4. Target 3개 + 원문 포함으로 셀 내용이 `max_cell_chars`를 넘음: 쓰기 전에 전체 중단, `셀 내용이 너무 깁니다. 더 작은 텍스트를 선택해주세요.` (Task 5 `assembled_cell_over_limit_is_cell_too_large`, Task 12 `range_cell_too_large_notifies_and_never_writes`)
5. Sheets 작업 중 사용자가 다른 것을 복사함: 작업이 끝나도 사용자의 새 클립보드를 덮어쓰지 않는다. (Task 9 `guard_keeps_user_copy_made_during_section`)

---

### Task 1: `ct-sheets` 크레이트, 모델·결과·라우팅·주소·지원 판정

**Files:**
- Create: `crates/ct-sheets/Cargo.toml`, `crates/ct-sheets/src/lib.rs`, `src/model.rs`, `src/outcome.rs`, `src/routing.rs`, `src/support.rs`
- Modify: `crates/ct-core/src/lib.rs`(필요하면 `Lang` 등 재노출 확인만)

**Interfaces:**
- Consumes: `ct_core::{Lang, BatchFailure}` — `BatchFailure`는 Task 5에서 만든다. 이 Task에서는 `outcome.rs`에 `pub enum OutcomeReason { Unsupported(UnsupportedReason), CellTooLarge, Other }`를 두어 ct-core에 의존하지 않는다. Task 8이 `BatchFailure`를 `OutcomeReason`으로 바꾼다(`CellTooLarge`만 구분, 나머지는 `Other`).
- Produces:
  - `model.rs`: `CellKind { Text, Formula, Number, Date, Boolean, Empty, Other }`, `SheetCell { row, col, kind, display_text: String, has_rich_text, has_link, row_span, col_span, formula: Option<String> }`, `SheetGrid { rows, cols, cells: Vec<SheetCell>, html: String, cell_spans: Vec<(usize, usize)> /* html 바이트 오프셋 */, compact_json: Option<String> }` + `fn cell(&self, row, col) -> Option<&SheetCell>`, `SheetLocation { document_id: String, gid: Option<String>, range_address: Option<String> }`, `SheetSelection { location, grid, hidden_or_filtered: Option<bool> }`, `pub type CellKey = (usize, usize);`
  - `outcome.rs`: `SheetsStatus`(S-ref §1.1의 12개), `UnsupportedReason { UnidentifiedTarget, MultipleRanges, WholeRowOrColumn, MergedCells, HiddenOrFiltered, RichTextOrLink, UnknownCellType, RangeTooLarge }`, `SheetsOutcome { status, reason: Option<OutcomeReason> }` + `fn user_message(&self) -> String`, `TableCell { row, col, original: String, results: Vec<(Option<Lang>, String)> }`, `SheetsTable { location, rows, cols, cells: Vec<TableCell> }`
  - `routing.rs`: `SheetsInputState { CellSelection, CellEditing, Other }`, `Route { SheetsRange, TextInteraction, SheetsUnsupported }`, `SheetsRole { Grid, CellEditor, FormulaBar, NameBox, Other }`, `fn is_google_sheets_url(url: &str) -> bool`, `fn choose_route(url: Option<&str>, state: Option<SheetsInputState>) -> Route`, `fn input_state_from_role(role: SheetsRole, editor_value: Option<&str>) -> SheetsInputState`, `fn parse_sheets_url(url: &str) -> (Option<String>, Option<String>)`, `pub trait SheetsRouting: Send + Sync { fn url_and_state(&self) -> (Option<String>, Option<SheetsInputState>); }`, `fn route_of(r: &dyn SheetsRouting) -> (Option<String>, Route)`
  - `support.rs`: `SheetsMode { Replace, Explain }`, `enum ParsedAddress { Size(usize, usize), Unsupported(UnsupportedReason) }`, `fn parse_range_address(address: &str) -> ParsedAddress`, `fn check_range_support(sel: &SheetSelection, mode: SheetsMode, max_range_cells: usize) -> Option<UnsupportedReason>`, `fn translatable_cells(grid: &SheetGrid) -> Vec<(CellKey, String)>`, `fn column_letters(index: usize) -> String`, `fn range_origin(address: Option<&str>) -> (usize, usize)`

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `tests/unit/sheets/test_routing.py`(6), `test_support.py`(14)의 케이스를 같은 입력과 기대값으로 옮긴다(S-ref §9 표의 "notable cases"를 모두 포함). 추가:

```rust
#[test] fn user_message_matches_ui_ref_table()
// SheetsStatus 12개 각각의 문구가 UI-ref §3.5와 같다. UNSUPPORTED_RANGE는 사유별 문구 8개 + 사유 없음 문구,
// TRANSLATION_FAILED/CORRECTION_FAILED + CellTooLarge → "셀 내용이 너무 깁니다. 더 작은 텍스트를 선택해주세요."
#[test] fn route_of_maps_url_and_state()   // 가짜 SheetsRouting으로 choose_route 결과 확인
#[test] fn column_letters_after_z()        // 0→"A", 25→"Z", 26→"AA", 701→"ZZ", 702→"AAA"
```

- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-sheets` → 컴파일 오류.
- [ ] **Step 3: S-ref §1.1, §1.2, §2.7대로 구현한다.** 크레이트를 워크스페이스에 넣는다(`members = ["crates/*"]`라 자동). `lib.rs`에 `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]`(다른 크레이트와 같은 속성)를 둔다.
- [ ] **Step 4: 통과를 확인한다.** `cargo test -p ct-sheets` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(sheets): ct-sheets 모델·라우팅·주소·지원 판정`

---

### Task 2: CF_HTML과 Chromium 사용자 정의 클립보드 코덱

**Files:**
- Create: `crates/ct-sheets/src/cf_html.rs`, `src/custom_clipboard.rs`
- Modify: `crates/ct-sheets/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub struct SheetsParseError(pub &'static str);` (`lib.rs` 또는 `model.rs`, `thiserror`. 고정 문구만 담는다)
  - `fn extract_cf_html_fragment(cf_html: &str) -> Result<String, SheetsParseError>`, `fn build_cf_html(fragment: &str) -> String`
  - `pub const CUSTOM_MIME: &str = "application/x-vnd.google-spreadsheet-compact-table+json";`, `pub const CUSTOM_FORMAT: &str = "Chromium Web Custom MIME Data Format";`
  - `fn decode_custom_data(raw: &[u8]) -> Result<Vec<(String, String)>, SheetsParseError>`, `fn encode_custom_data(map: &[(String, String)]) -> Vec<u8>`, `fn compact_from_custom(raw: Option<&[u8]>) -> Result<Option<serde_json::Value>, SheetsParseError>`
  - `fn expand_runs(values: &[serde_json::Value], count: usize) -> Result<Vec<i64>, SheetsParseError>`
  - `pub struct CompactItem { pub text: Option<String>, pub string_index: Option<usize>, pub formula: Option<String>, pub rich_index: Option<usize> }`, `fn compact_cells(data: &serde_json::Value) -> Result<(usize, usize, Vec<CompactItem>), SheetsParseError>`
  - `fn replace_compact_strings(data: &serde_json::Value, replacements: &[(CellKey, String)]) -> Result<serde_json::Value, SheetsParseError>`

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `test_custom_clipboard.py`(11)와 `test_clipboard_html.py`의 CF_HTML 왕복(한국어 포함) 케이스를 옮긴다. 추가:

```rust
#[test] fn cf_html_offsets_are_utf8_bytes()   // 한글 조각으로 build → 헤더의 StartFragment/EndFragment가 바이트 기준, extract로 원래 조각 복원
#[test] fn cf_html_missing_offsets_rejected() // "CF_HTML header has no fragment offsets"
#[test] fn cf_html_offsets_out_of_range_rejected()
#[test] fn custom_data_rejects_more_than_100_entries()
#[test] fn compact_unknown_flag_bit_rejected()  // 0x4CB 밖 비트
```

- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-sheets` → 컴파일 오류.
- [ ] **Step 3: S-ref §2.4, §2.5대로 구현한다.** 헤더 필드는 10자리 0 채움, 접두·접미 문자열은 S-ref 그대로. 실패 문구는 S-ref에 나온 고정 문구를 쓴다.
- [ ] **Step 4: 통과를 확인한다.** `cargo test -p ct-sheets` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(sheets): CF_HTML과 Sheets 사용자 정의 클립보드 코덱`

---

### Task 3: Sheets HTML 표 파서와 서식 판정

**Files:**
- Create: `crates/ct-sheets/src/html.rs`
- Modify: `crates/ct-sheets/src/lib.rs`

**Interfaces:**
- Consumes: Task 1 모델, Task 2 `compact_cells`, `SheetsParseError`.
- Produces: `fn parse_sheets_html(fragment: &str, compact: Option<&serde_json::Value>) -> Result<SheetGrid, SheetsParseError>`, `fn html_rich_text_state(clipboard_html: &str) -> Option<bool>`

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `test_clipboard_html.py`(14, CF_HTML 왕복 제외)와 `test_rich_to_plain.py` 중 파서 관련 케이스를 옮긴다. 테스트용 HTML은 v5 테스트의 픽스처 문자열을 그대로 쓴다. 추가:

```rust
#[test] fn compact_number_type_rejects_capture()   // compact에 값 타입 3(숫자) → Err("unsupported compact metadata")
#[test] fn single_cell_span_becomes_synthetic_table() // data-sheets-root span + compact 1x1 → grid.html이 S-ref §2.6-2의 합성 표
#[test] fn cell_spans_are_byte_offsets_of_each_td()  // 한글 셀이 있어도 &grid.html[s..e]가 각 <td ...>...</td>
#[test] fn unknown_named_entity_kept_literally()     // "&copy;" → 표시 텍스트에 "&copy;" 그대로 (D5)
#[test] fn plain_overflow_div_is_not_rich()          // style만 있고 font/color 등이 없는 <div>
```

- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-sheets html` → 컴파일 오류.
- [ ] **Step 3: S-ref §2.6, §5.4(`html_rich_text_state`)대로 구현한다.** 의존성 없는 작은 토크나이저(시작 태그·끝 태그·자기 닫힘·텍스트·주석, 따옴표 안의 `>` 처리)를 쓴다(D5). `cell_spans`는 `grid.html` 안의 바이트 오프셋이다.
- [ ] **Step 4: 통과를 확인한다.** `cargo test -p ct-sheets` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(sheets): Sheets 클립보드 HTML 표 파서`

---

### Task 4: 교체 HTML 조립, 네이티브 페이로드, 쓰기 검증

**Files:**
- Create: `crates/ct-sheets/src/payload.rs`, `src/verify.rs`
- Modify: `crates/ct-sheets/src/lib.rs`

**Interfaces:**
- Consumes: Task 1~3.
- Produces:
  - `fn build_replacement_html(grid: &SheetGrid, replacements: &[(CellKey, String)]) -> Result<String, PayloadError>` (`PayloadError(&'static str)`: `"no cell at ..."`류는 고정 문구 `"no such cell"`, `"not a text cell"`, `"unterminated <td> tag"`)
  - `fn native_payload(grid: &SheetGrid, new_fragment: &str) -> Result<Option<Vec<u8>>, SheetsParseError>`: `grid.compact_json`이 없으면 `None`. 있으면 S-ref §4.3-3대로 새 조각을 다시 파싱해 비어 있지 않은 원래 TEXT 셀 전부의 문자열을 바꾸고 `encode_custom_data([(CUSTOM_MIME, json)])`를 돌려준다.
  - `verify.rs`: `fn same_target(before: &SheetSelection, after: &SheetSelection) -> bool`, `fn written_as_expected(before: &SheetGrid, replacements: &[(CellKey, String)], after: &SheetGrid) -> bool` (지문은 셀 튜플 목록 비교, D4)

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `test_write_payload.py`(11, TSV 2개 제외 — D7), `test_rich_to_plain.py`의 페이로드 케이스, `test_verify.py`(10), `test_windows_sheets_backend.py`의 `native_paste_keeps_untranslated_whitespace_cell_formatting`(네이티브 페이로드 부분)을 옮긴다.
- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-sheets` → 컴파일 오류.
- [ ] **Step 3: S-ref §4.2, §4.3-3, §4.4대로 구현한다.** 원래 조각을 오프셋으로 이어 붙이고 다시 직렬화하지 않는다. `data-sheets-value` JSON의 `"1": 2`, 문자 이스케이프, `\n` → `<br>`, `data-sheets-textstyleruns` 제거, compact가 있을 때 수식 셀에 `data-sheets-formula` 추가를 S-ref 그대로 한다.
- [ ] **Step 4: 통과를 확인한다.** `cargo test -p ct-sheets` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(sheets): 교체 HTML 조립과 쓰기 후 검증`

---

### Task 5: 배치 LLM 포트와 배치 번역 서비스 (ct-core)

**Files:**
- Modify: `crates/ct-core/src/llm.rs` (배치 타입, trait 기본 메서드)
- Create: `crates/ct-core/src/batch.rs`
- Modify: `crates/ct-core/src/lib.rs`, `crates/ct-core/src/dictionary/protector.rs`(재번호 매기기 도우미가 필요하면), `crates/ct-core/src/test_support.rs`(FakeLlm 배치 응답)

**Interfaces:**
- Consumes: `mask`, `MaskMatch`, `validate_integrity`, `restore`, `resolve_plan`, `LanguageDetector`, `compose_replacement`, `SheetsConfig`.
- Produces:
  - `llm.rs`: `BatchItem { id: String, text: String, source: Lang, destinations: Vec<Lang> }`, `BatchTranslationRequest { items: Vec<BatchItem>, tone: Tone }`, `BatchTranslationResult { items: Vec<(String, Vec<(Lang, String)>)> }`, `BatchCorrectionItem { id: String, text: String }`, `BatchCorrectionRequest { items: Vec<BatchCorrectionItem>, mode: CorrectionMode }`, `BatchCorrectionResult { items: Vec<(String, String)> }`. `LlmProvider`에 기본 메서드 `fn translate_batch(&self, _: &BatchTranslationRequest) -> Result<BatchTranslationResult, LlmError> { Err(LlmError("batch translation not supported by this provider".into())) }`, `fn correct_batch(...)`도 같은 형태(`"correction not supported by this provider"`).
  - `batch.rs`: `pub type CellKey = (usize, usize);`, `CellText { key: CellKey, text: String }`, `BatchLimits { max_cells, max_chars, max_cell_chars, job_timeout: Duration }` + `impl From<&SheetsConfig> for BatchLimits`, `BatchFailure { CellTooLarge, DirectionUnknown, Llm(String), Integrity, Timeout, NoKoreanText }`,
    `fn translate_cells(cells: &[CellText], entries: &[DictionaryEntry], detector: &dyn LanguageDetector, llm: &dyn LlmProvider, base: Lang, targets: &[Lang], tone: Tone, include_original: bool, limits: &BatchLimits, elapsed: &dyn Fn() -> Duration) -> Result<Vec<(CellKey, Vec<(Lang, String)>)>, BatchFailure>`,
    `fn compose_cell_replacements(cells: &[CellText], translated: &[(CellKey, Vec<(Lang, String)>)], include_original: bool, max_cell_chars: usize) -> Result<Vec<(CellKey, String)>, BatchFailure>`
  - `ct-sheets`의 `CellKey`는 이 타입을 다시 쓴다(`pub use ct_core::CellKey`). Task 1에서 따로 정의했다면 이 Task에서 ct-core 것으로 바꾼다.

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `translation/test_batch.py`(17)를 다중 Target에 맞게 옮긴다(목적 언어 목록, 응답의 언어별 텍스트). 추가:

```rust
#[test] fn mixed_language_cells_get_per_item_destinations()
// base ko, targets [En, Ja]; 셀 "안녕하세요"(ko), "Hello"(en)
// → LLM이 받은 항목: c0 destinations [En, Ja], c1 destinations [Ko]
#[test] fn batch_size_counts_destinations_and_original()
// max_chars 10, 셀 3개(각 4글자), targets 2개, include_original true → 4×3=12 > 10이므로 항목마다 한 묶음(LLM 3회)
#[test] fn assembled_cell_over_limit_is_cell_too_large()
// compose_cell_replacements: 원문 포함 + 번역 2개 조립 결과가 max_cell_chars를 넘으면 Err(CellTooLarge)
#[test] fn compose_cell_replacements_joins_blocks_with_blank_line()
// 원문 포함 → "원문\n\nEN\n\nJA", 미포함 → "EN\n\nJA"
#[test] fn default_provider_batch_methods_are_unsupported()
#[test] fn compose_replacement_drops_blank_blocks()   // 계획 1 후속 항목: 현재 동작(빈 블록 제거)을 테스트로 고정
```

- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-core batch` → 컴파일 오류.
- [ ] **Step 3: S-ref §3.3대로 구현한다.** 차이: 방향은 `resolve_plan(detected, base, targets)`로 정하고 항목마다 `destinations`를 둔다. 묶음 크기는 D3 공식. 항목별 무결성 검사·복원은 목적 언어마다 `restore(text, mapping, lang)`. 응답 id 집합 불일치, 언어 키 불일치는 `BatchFailure::Llm("response ids do not match request")`. 전부 성공해야 결과를 돌려준다.
- [ ] **Step 4: 통과를 확인한다.** `cargo test --workspace` → PASS(기존 `LlmProvider` 구현은 기본 메서드로 그대로 컴파일된다).
- [ ] **Step 5: 커밋한다.** `feat(core): 배치 LLM 포트와 다중 Target 배치 번역`

---

### Task 6: 배치 교정 서비스 (ct-core)

**Files:**
- Modify: `crates/ct-core/src/batch.rs`, `crates/ct-core/src/test_support.rs`

**Interfaces:**
- Consumes: Task 5 타입, `restore_fragment`, `validate_integrity`.
- Produces: `fn correct_cells(cells: &[CellText], entries: &[DictionaryEntry], detector: &dyn LanguageDetector, llm: &dyn LlmProvider, mode: CorrectionMode, limits: &BatchLimits, elapsed: &dyn Fn() -> Duration) -> Result<Vec<(CellKey, String)>, BatchFailure>` (한국어 셀만 담는다. 바뀌지 않은 셀도 원문 그대로 담는다)

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `correction/test_batch.py`(7)를 옮긴다. 추가: `#[test] fn correction_batch_size_ignores_destinations()` (교정은 글자 수 그대로 묶음 크기 계산).
- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-core batch` → 컴파일 오류.
- [ ] **Step 3: S-ref §3.4대로 구현한다.** 항목 id는 보낸 한국어 셀 기준 `c{n}`, 앞뒤 공백 병합 규칙 그대로.
- [ ] **Step 4: 통과를 확인한다.** `cargo test -p ct-core` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(core): 배치 교정`

---

### Task 7: 배치 프롬프트와 응답 검증 (ct-llm)

**Files:**
- Modify: `crates/ct-llm/src/prompts.rs`, `crates/ct-llm/src/provider.rs`, `crates/ct-llm/src/mock.rs`

**Interfaces:**
- Consumes: Task 5 배치 타입, 기존 `ask_json`, `correction_preamble`, `correction_formatting_instruction`, `POLISH_MERGE_SPLIT_INSTRUCTION`, `formatting_instruction`, `tone_instruction`.
- Produces: `pub fn batch_translation_prompt(req: &BatchTranslationRequest, model: &str) -> String`, `pub fn batch_correction_prompt(req: &BatchCorrectionRequest, model: &str) -> String`, `ClaudeCliProvider::translate_batch`/`correct_batch`, `MockProvider` 배치 응답(`"[{dest}] {text}"` 언어마다, `"[교정] {text}"`).

번역 프롬프트 문구(이 계획이 정함, v5 문구를 다중 Target에 맞게 고친 것):

```
Translate the text of every item below from its source language into each of its target languages. Translate each item independently: never merge, split, reorder, add or drop items, and return each item's id unchanged. Keep line breaks and tabs inside each text as they are. Preserve any tokens that look like ⟦N⟧ exactly as-is within the same item -- do not translate, move to another item, or remove them. {formatting}{tone}Respond with only JSON matching this shape: {"translations": [{"id": str, "texts": {"<target code>": str}}]} where each item's "texts" has exactly that item's target codes as keys.

Items:
{"items": [{"id": "c0", "source": "ko", "targets": ["en", "ja"], "text": "..."}]}
```

교정 프롬프트는 S-ref §3.5 그대로(다듬기면 `POLISH_MERGE_SPLIT_INSTRUCTION` 포함). 단, 교정 프롬프트에 "translate"가 들어가지 않는다는 기존 규칙(R13)을 지킨다. 항목 JSON은 `serde_json`으로 직렬화한다(비ASCII 그대로).

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `llm/test_batch_translate.py`(7)를 옮긴다. 응답 위반 문구는 S-ref §3.5 목록 그대로(`Claude CLI batch response missing 'translations' list` 등). 추가:

```rust
#[test] fn batch_translation_rejects_wrong_language_keys()  // 항목 targets [en, ja]인데 texts에 en만 → Err
#[test] fn batch_translation_prompt_lists_targets_per_item()
#[test] fn batch_correction_prompt_never_mentions_translation()
#[test] fn mock_batch_translates_every_destination()
```

- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-llm batch` → FAIL.
- [ ] **Step 3: 구현한다.** 응답 파싱은 `ct_core::llm`의 기존 JSON 도우미(코드 펜스 제거 등)를 다시 쓴다.
- [ ] **Step 4: 통과를 확인한다.** `cargo test -p ct-llm` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(llm): 다중 Target 배치 번역·교정 프롬프트와 응답 검증`

---

### Task 8: 범위 작업 서비스 (ct-sheets)

**Files:**
- Create: `crates/ct-sheets/src/service.rs`
- Modify: `crates/ct-sheets/src/lib.rs`, `crates/ct-sheets/src/outcome.rs`(`BatchFailure` → `OutcomeReason` 변환), `crates/ct-sheets/Cargo.toml`

**Interfaces:**
- Consumes: Task 1~6.
- Produces:

```rust
pub struct CaptureError { pub reason: Option<UnsupportedReason> }
pub trait SheetsInteraction: Send + Sync {
    fn capture_selection(&self) -> Result<SheetSelection, CaptureError>;
    fn reread_selection(&self) -> Result<SheetSelection, CaptureError> { self.capture_selection() }
    fn can_edit(&self) -> bool;
    fn write_selection(&self, selection: &SheetSelection, cf_html: &str) -> Result<(), String>;
}
pub type Transform<'a> = dyn FnMut(&[CellText]) -> Result<Vec<(CellKey, String)>, SheetsOutcome> + 'a;
/// skip_unchanged: transform 결과 중 원문과 같은 셀을 빼고, 남은 것이 없으면 NO_CHANGES(쓰기 없음). 번역 false, 교정 true.
pub fn replace_cells(ia: &dyn SheetsInteraction, max_range_cells: usize, transform: &mut Transform, no_text: SheetsStatus, failure: SheetsStatus, skip_unchanged: bool) -> SheetsOutcome;
pub fn capture_for_table(ia: &dyn SheetsInteraction, max_range_cells: usize) -> Result<SheetSelection, SheetsOutcome>;
pub fn table_from(selection: &SheetSelection, results: &[(CellKey, Vec<(Option<Lang>, String)>)]) -> SheetsTable;
pub fn translation_failure(f: &BatchFailure) -> SheetsOutcome;   // TRANSLATION_FAILED(+CellTooLarge)
pub fn correction_failure(f: &BatchFailure) -> SheetsOutcome;    // NoKoreanText → NO_KOREAN_TEXT, 나머지 CORRECTION_FAILED(+CellTooLarge)
```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `sheets/test_service.py`(33)를 가짜 `SheetsInteraction`(캡처 결과 목록, 쓰기 기록, 실패 주입)으로 옮긴다. 이름이 정해진 테스트:

```rust
#[test] fn reread_differs_is_target_changed_and_never_writes()
#[test] fn parse_error_is_capture_failed()            // capture_selection이 reason 없는 CaptureError → CAPTURE_FAILED
#[test] fn transform_error_maps_to_failure_status()   // D6
#[test] fn payload_error_reports_failure_status_not_write_failed()
#[test] fn write_error_is_write_failed_without_retry()
#[test] fn verify_mismatch_is_write_unverified()
#[test] fn correction_nothing_changed_is_no_changes_and_never_writes()  // 교정 transform이 빈 목록 → NO_CHANGES
#[test] fn table_keeps_every_cell_in_order()
```

- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-sheets service` → 컴파일 오류.
- [ ] **Step 3: S-ref §4.1(10단계), §7.1대로 구현한다.** 쓰기 페이로드는 `build_cf_html(build_replacement_html(...))`. 교정 경로의 "바뀐 셀만 남기기"와 `NO_CHANGES`는 ct-app이 넘기는 transform 안에서 하지 않고 `replace_cells`의 `skip_unchanged`로 한다. 로그는 개수·상태·소요 시간만.
- [ ] **Step 4: 통과를 확인한다.** `cargo test -p ct-sheets` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(sheets): 범위 교체·표 보기 서비스`

---

### Task 9: 다중 포맷 클립보드와 가드 (ct-win)

**Files:**
- Modify: `crates/ct-win/src/clipboard.rs` (또는 새 파일 `crates/ct-win/src/format_clipboard.rs` + `lib.rs`)
- Modify: `crates/ct-win/Cargo.toml` (`ct-sheets` 의존성은 Task 11에서 추가)

**Interfaces:**
- Produces:

```rust
pub struct ClipboardSnapshot { formats: Vec<(u32, Vec<u8>)> }
pub trait FormatClipboard: Send + Sync {
    fn sequence_number(&self) -> u32;
    fn snapshot(&self) -> Option<ClipboardSnapshot>;
    fn restore(&self, snapshot: &ClipboardSnapshot) -> Result<(), ClipboardError>;
    fn read_text(&self) -> Option<String>;
    fn read_registered(&self, format_name: &str) -> Option<Vec<u8>>;          // "HTML Format", CUSTOM_FORMAT
    fn write_registered(&self, formats: &[(&str, &[u8])]) -> Result<(), ClipboardError>; // Empty 후 모두 쓰기, CF_UNICODETEXT 없음
}
pub struct ClipboardGuard<'a> { /* begin 때 스냅샷과 시퀀스 번호 */ }
impl<'a> ClipboardGuard<'a> {
    pub fn begin(cb: &'a dyn FormatClipboard) -> Self;
    pub fn mark_own_write(&mut self);
    pub fn finish(self);   // UI-ref §2.3의 복원 규칙
}
pub fn wait_for_clipboard_change(cb: &dyn FormatClipboard, before: u32, timeout: Duration) -> bool; // stable 0.05 s, poll 0.02 s
pub struct Win32FormatClipboard;  // 실제 구현
```

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `sheets/test_clipboard_guard.py`(11) 중 복원 규칙·대기 케이스를 가짜 `FormatClipboard`로 옮긴다(스레드 구간 테스트는 기존 `lock_clipboard_section` 테스트로 충분하면 생략하고 보고서에 적는다). 이름이 정해진 테스트:

```rust
#[test] fn guard_restores_after_own_write()
#[test] fn guard_keeps_user_copy_made_during_section()  // begin 뒤 사용자가 복사(시퀀스 증가, mark 없음) → finish가 restore를 부르지 않는다
#[test] fn guard_swallows_restore_failure()
#[test] fn wait_returns_false_on_timeout()
#[test] fn wait_requires_stable_sequence()
```

- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-win clipboard` → 컴파일 오류.
- [ ] **Step 3: UI-ref §2.3대로 구현한다.** 실제 구현은 `EnumClipboardFormats`/`GetClipboardData`로 HGLOBAL 포맷만 복사하고 비트맵·메타파일 등 핸들 포맷은 건너뛴다(UI-ref §2.3의 v5 규칙과 같게). `RegisterClipboardFormatW`로 이름 포맷 번호를 얻는다.
- [ ] **Step 4: 통과를 확인한다.** `cargo test -p ct-win` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(win): 다중 포맷 클립보드와 복원 가드`

---

### Task 10: Sheets 역할 분류, 셀 편집 캡처, 셀 편집 전략과 안전 래퍼 (ct-win)

**Files:**
- Modify: `crates/ct-win/src/chrome/role.rs`, `strategy.rs`, `uia_backend.rs`, `adapter.rs`, `mod.rs`
- Create: `crates/ct-win/src/chrome/sheets_edit.rs`

**Interfaces:**
- Consumes: Task 9 `FormatClipboard`, `ClipboardGuard`, `wait_for_clipboard_change`; `ct_sheets::routing::{is_google_sheets_url, SheetsRole}`; `ct_sheets::html_rich_text_state`(로그용).
- Produces:
  - `Role`에 `SheetsGrid`, `SheetsCellEditor`, `SheetsFormulaBar`, `SheetsNameBox` 추가. `classify_focused_role(url, name, control_type, automation_id: &str)`(인자 추가, 기존 호출부 수정). `control_type_name`에 `50003 => "ComboBoxControl"` 추가. `fn sheets_role(role: Role) -> SheetsRole`.
  - `FocusedElement`에 `editor_full_text: Option<String>` 추가(Sheets가 아니면 `None`).
  - `Strategy::GoogleSheetsCellEdit` 추가, `REPLACE_STRATEGIES`·`READ_ONLY_STRATEGIES` 맨 끝에 넣는다(배열 길이 5).
  - `ReplaceError::FormulaPrefix`, `ReplaceError::FullCellNotEnabled` (D10).
  - `pub struct SheetsEditSafe<T: TextInteraction> { inner: T, allow_full_cell_replace: bool }` + `TextInteraction` 구현(`try_capture`는 그대로 위임).

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `interaction/test_sheets_strategy.py`(19), `interaction/test_sheets_edit.py`(10)를 옮긴다. 추가:

```rust
#[test] fn sheets_combobox_cases()  // automation_id "waffle-rich-text-editor": name "" → SheetsGrid, "B12" → SheetsCellEditor; 다른 id → Unknown
#[test] fn editor_capture_never_sends_select_all()  // 가짜 키 전송기로 셀 편집 캡처가 Ctrl+C 한 번만 보내는지
#[test] fn sheets_edit_safe_forwards_try_capture_miss_kind()
```

- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-win` → 컴파일 오류.
- [ ] **Step 3: 구현한다.**
  - 역할 분류: S-ref §1.3. Sheets URL일 때만 automation id를 읽는다(`UIA_AutomationIdPropertyId`). 비Sheets 분기는 그대로.
  - 셀 편집 캡처: S-ref §5.1. 역할이 `SheetsCellEditor`/`SheetsFormulaBar`면 sentinel 대신 시퀀스 번호로 선택을 판정(0.3초, poll 0.02)하고, ValuePattern 값(끝의 `\n` 하나 제거)을 `editor_full_text`에 넣는다. Ctrl+A를 보내지 않는다. 이 경로는 `ClipboardGuard`(다중 포맷)를 쓴다. `WindowsUiaBackend::new`가 `Arc<dyn FormatClipboard>`도 받게 바꾼다.
  - 전략: S-ref §5.2. `=`로 시작하는 셀(앞 공백 무시)은 선택이 있어도 컨텍스트 없음.
  - `SheetsEditSafe::replace`: S-ref §5.3-1 순서. `=`/`+`(앞 공백 무시) → `FormulaPrefix`, 선택 없음 + 전체 교체 꺼짐 → `FullCellNotEnabled`, 다시 캡처한 `EditingContext` 전체가 다르면 `ContextChanged`. Sheets가 아닌 컨텍스트(`app != "google_sheets"`)는 그대로 통과.
  - 전체 셀 교체는 기존 `paste_replacing_full_text`(Ctrl+A, 0.2초, Ctrl+V)를 쓴다.
- [ ] **Step 4: 통과를 확인한다.** `cargo test --workspace` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(win): Sheets 역할 분류와 셀 편집 교체`

---

### Task 11: `WindowsSheetsBackend` (ct-win)

**Files:**
- Create: `crates/ct-win/src/chrome/sheets_backend.rs`
- Modify: `crates/ct-win/src/chrome/uia_backend.rs`(`ChromeWindow`와 포커스 해석을 `pub(crate)`로 열기), `mod.rs`, `crates/ct-win/Cargo.toml`(`ct-sheets` 추가)

**Interfaces:**
- Consumes: Task 9, Task 10 포커스 해석, `ct_sheets::{SheetsInteraction, SheetsRouting, parse_sheets_url, parse_range_address, extract_cf_html_fragment, parse_sheets_html, compact_from_custom, native_payload, CUSTOM_FORMAT}`.
- Produces: `pub struct WindowsSheetsBackend` + `pub fn new(keys: Arc<dyn KeySender>, clipboard: Arc<dyn FormatClipboard>, assume_editable: bool, empty_editor_is_selection: bool, max_range_cells: usize) -> Self`. `SheetsRouting`, `SheetsInteraction` 구현. 순수 판단 부분은 함수로 분리해 테스트한다: `fn capture_decision(url: Option<&str>, state: Option<SheetsInputState>, address: Option<&str>, max_range_cells: usize) -> Result<(), CaptureError>`(복사 전 거절), `fn paste_target_matches(captured: &SheetSelection, url: Option<&str>, state: Option<SheetsInputState>, address: Option<&str>) -> bool`.

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `sheets/test_windows_sheets_backend.py`(8) 중 순수 판단으로 옮길 수 있는 것(이름 상자 없음 → 주소 None, 빈 편집기 선택 처리 opt-in)과:

```rust
#[test] fn whole_row_selection_is_rejected_before_copy()
#[test] fn oversized_address_is_rejected_before_copy()
#[test] fn unknown_address_still_copies()          // capture_decision이 Ok
#[test] fn paste_target_requires_same_document_gid_and_address()
```

- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-win sheets` → 컴파일 오류.
- [ ] **Step 3: 구현한다.** S-ref §1.4(`url_and_state`), §2.1~§2.3(캡처: 이름 상자 먼저, 거절, Ctrl+C, 1.0초 대기, HTML·사용자 정의 포맷 읽기, 파싱), §2.2(이름 상자 검색 순서와 깊이 24), §4.3(쓰기: 재확인, 가드, `write_registered([("HTML Format", cf_html), (CUSTOM_FORMAT, native)])`, Alt 해제 대기, Ctrl+V 후 바로 Ctrl+C, 3.0초 확인, 확인 안 되면 복원하지 않음 — D9). 각 공개 메서드는 `lock_clipboard_section()`을 한 번만 잡는다.
- [ ] **Step 4: 통과를 확인한다.** `cargo test --workspace`, `cargo clippy --workspace -- -D warnings` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(win): Windows Sheets 백엔드`

---

### Task 12: 단축키 흐름의 Sheets 라우팅 (ct-app)

**Files:**
- Create: `crates/ct-app/src/sheets_flows.rs`
- Modify: `crates/ct-app/src/flows.rs`, `crates/ct-app/src/lib.rs`, `crates/ct-app/Cargo.toml`

**Interfaces:**
- Consumes: Task 5~8, Task 10의 `ReplaceError` 변형, 기존 흐름들.
- Produces:
  - `pub struct SheetsDeps<'a> { pub routing: &'a dyn SheetsRouting, pub range: &'a dyn SheetsInteraction, pub config: &'a SheetsConfig }`
  - 문구 상수(UI-ref §5.3~§5.5 그대로): `MSG_SHEETS_EDIT_TRANSLATE_FAILED`(`편집 중인 셀을 번역할 수 없습니다(수식·서식 또는 셀 상태를 확인할 수 없음).`), `MSG_SHEETS_EDIT_CORRECT_FAILED`, `MSG_SHEETS_RANGE_REPLACE_DISABLED`, `MSG_SHEETS_CORRECTION_RANGE_REPLACE_DISABLED`, 셀 편집 교체 오류 문구 번역·교정 각 3개.
  - 네 흐름이 맨 앞에서 `route_of(routing)`을 부른다(교정 흐름은 base 확인·수정키 대기 뒤, v5 순서). 동작:
    - `SheetsUnsupported` → `SheetsOutcome{WrongInputState}` 문구(교체 계열은 notifier, 보기 계열은 view.show_error).
    - 교체 계열 + `SheetsRange` + `!config.range_replace_enabled` → 범위 교체 꺼짐 문구.
    - Alt+Alt + `SheetsRange` → 배지(`Translating...`) 안에서 `replace_cells(range, max_range_cells, transform, NoText, TranslationFailed, skip_unchanged=false)`. transform = `translate_cells(... include_original_on_replace ...)` → `compose_cell_replacements` → 실패는 `translation_failure`. SUCCESS가 아니면 배지가 사라진 뒤 문구.
    - Ctrl+Alt+Alt + `SheetsRange` → 배지(교정 진행 문구) 안에서 `replace_cells(..., NoKoreanText, CorrectionFailed, skip_unchanged=true)`, transform = `correct_cells` → 실패는 `correction_failure`.
    - Ctrl+Ctrl + `SheetsRange` → `capture_for_table` → 실패 문구 / 성공이면 `show_loading("Translating...")` → `translate_cells(include_original=false)` → `table_from`(언어 태그 `Some(lang)`) → `view.show_table`. 번역할 셀이 없으면 `NO_TEXT`.
    - Alt+Ctrl+Ctrl + `SheetsRange` → 같은 순서, 진행 문구는 교정 모드 문구, `correct_cells` → `table_from`(태그 `None`), 한국어 셀이 없으면 `NO_KOREAN_TEXT`.
    - `TextInteraction` 경로(편집 중이거나 Sheets 아님) → 기존 흐름. 단, `on_sheets`(경로가 TextInteraction이 아니거나 URL에 `docs.google.com/spreadsheets`)면 캡처 실패 문구를 Sheets 편집 문구로, `ReplaceError::{FormulaPrefix, FullCellNotEnabled, ContextChanged}`를 Sheets 문구로 바꾼다.
  - `ExplainView::show_table(&self, table: SheetsTable)` 추가(테스트 가짜는 `push(format!("table:{}x{}", rows, cols))`). 실제 그리기는 Task 13.
  - 기존 `alt_alt_flow`의 `docs_or_slides: &dyn Fn() -> bool` 인자를 `no_capture_message: &dyn Fn() -> Option<&'static str>`로 바꾼다(Sheets 편집 문구를 넣기 위해).

- [ ] **Step 1: 실패하는 테스트를 쓴다.** 가짜 `SheetsRouting`(경로 고정), 가짜 `SheetsInteraction`(Task 8의 가짜를 공유하지 말고 ct-app 테스트용으로 작게), 기존 FakeLlm에 배치 응답 추가. 이름이 정해진 테스트:

```rust
#[test] fn alt_alt_range_success_is_silent_and_badge_wraps_work()
#[test] fn alt_alt_range_disabled_notifies_without_capture()
#[test] fn alt_alt_unsupported_state_notifies_wrong_input_state()
#[test] fn range_cell_too_large_notifies_and_never_writes()  // Target 3개 + 원문 포함, max_cell_chars 작게
#[test] fn alt_alt_range_include_original_writes_original_first()
#[test] fn ctrl_ctrl_range_shows_loading_then_table()
#[test] fn ctrl_ctrl_range_capture_failure_shows_error_without_loading()
#[test] fn correction_range_runs_after_base_check_and_modifier_wait()
#[test] fn correction_view_range_shows_table_without_tags()
#[test] fn sheets_editing_capture_failure_uses_sheets_message()
#[test] fn sheets_editing_formula_prefix_uses_sheets_message()  // 번역·교정 각각
#[test] fn non_sheets_page_keeps_existing_behaviour()          // 경로 TextInteraction, Sheets URL 아님 → 기존 문구
```

- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-app` → 컴파일 오류.
- [ ] **Step 3: 위 동작대로 구현한다.** 경과 시간 클로저는 흐름 시작 시각의 `Instant::elapsed`. 로그는 작업 종류·경로·상태·셀 수·소요 시간만.
- [ ] **Step 4: 통과를 확인한다.** `cargo test --workspace` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(app): 네 단축키에 Google Sheets 범위·셀 편집 라우팅`

---

### Task 13: 설명 창 표 보기 (ct-app UI)

**Files:**
- Modify: `crates/ct-app/src/ui/explain.rs`, `crates/ct-app/ui/explain.slint`, `crates/ct-app/src/ui/mod.rs`

**Interfaces:**
- Consumes: `SheetsTable`, `TableCell`, `range_origin`, `column_letters`, 기존 `tag(lang)`, `fit_size`, `Present`.
- Produces: `pub struct SheetTableView { caption: Option<String>, col_headers: Vec<String>, row_headers: Vec<String>, rows: Vec<Vec<Option<SheetCellView>>> }`, `pub struct SheetCellView { original: String, lines: Vec<(Option<&'static str>, String)> }`, `pub fn build_table_view(table: &SheetsTable) -> SheetTableView`, `pub fn table_size(...)`(S-ref §7.2-7 크기 규칙), `ExplainWindow::show_table`.

- [ ] **Step 1: 실패하는 테스트를 쓴다.** v5 `ui/test_sheet_table.py`(18) 중 순수 부분(헤더, 캡션, 빈 셀, 여러 줄, 크기 범위)을 옮긴다. 추가:

```rust
#[test] fn caption_is_gid_and_address()             // gid "0", "A1:C3" → "gid 0 · A1:C3"; 둘 다 없으면 None
#[test] fn headers_start_at_range_origin()          // "C5:D6" → 열 ["C","D"], 행 ["5","6"]
#[test] fn translation_tags_only_with_two_or_more_languages()
#[test] fn empty_cell_without_result_has_no_view()
#[test] fn correction_cell_has_untagged_line()
```

- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-app explain` → 컴파일 오류.
- [ ] **Step 3: 구현한다.** 표는 S-ref §7.2: 열 너비 220, 최소 행 높이 28, 원문 굵게, 결과 줄은 굵게 accent 색(`#2563EB`), 일반 텍스트(서식 해석 안 함), 줄바꿈, 창 크기 `+60`/`+130`을 480x240~700x800(화면 크기 이내)로 제한. 표가 있을 때 다른 상태·결과가 오면 표를 지운다. 결과 표시이므로 `Present::Fit`(닫힌 창을 다시 띄우지 않음).
- [ ] **Step 4: 통과를 확인한다.** `cargo test -p ct-app` → PASS.
- [ ] **Step 5: 커밋한다.** `feat(app): 설명 창에 Sheets 표 보기`

---

### Task 14: 앱 조립과 수동 테스트 문서

**Files:**
- Modify: `crates/ct-app/src/main.rs`
- Create: `tests/manual/plan4_sheets.md`

**Interfaces:**
- Consumes: Task 9~13.
- Produces: 조립부에서
  - `Win32FormatClipboard`를 만들어 `WindowsUiaBackend`와 두 Sheets 백엔드에 넘긴다.
  - `sheets_replace = WindowsSheetsBackend::new(keys, fmt_cb, assume_editable = config.sheets.range_replace_enabled, empty_editor_is_selection = false, max_range_cells)`, `sheets_explain = ...(false, true, max_range_cells)`(S-ref §1.4 두 인스턴스).
  - 교체 계열 텍스트 상호작용은 `RetryingInteraction::new(SheetsEditSafe { inner: ChromeAdapter(REPLACE_STRATEGIES), allow_full_cell_replace: config.sheets.edit_full_cell_replace_enabled })`.
  - `AppAction`에 Sheets 의존성과 `SheetsConfig`를 더하고 네 흐름에 넘긴다.

- [ ] **Step 1: 실패하는 테스트를 쓴다.** `main.rs` 테스트의 `action(&log)` 도우미에 가짜 Sheets 라우팅(항상 TextInteraction)을 넣고, 기존 `run_routes_*` 테스트가 그대로 통과하는지와 `#[test] fn sheets_range_route_reaches_range_flow()`(가짜 라우팅이 SheetsRange, 가짜 범위 캡처 실패 → `notify:선택한 셀을 읽을 수 없습니다.`)를 쓴다.
- [ ] **Step 2: 실패를 확인한다.** `cargo test -p ct-app --bin ct-app` → FAIL.
- [ ] **Step 3: 구현한다.**
- [ ] **Step 4: 통과와 품질 검사를 확인한다.** `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check` → 모두 통과.
- [ ] **Step 5: 수동 테스트 문서를 쓴다.** `tests/manual/plan4_sheets.md`, `plan3_correction.md`와 같은 형식(준비 / 절차 / 기대 결과 / 결과 칸, 통과 표시 없음). Settings 항목 이름은 실제 화면 이름을 쓴다. 항목:
  - H1 범위 Alt+Alt: 한국어 셀 3개, Target 1개 / 3개, "Alt+Alt 교체 시 원문 포함" 켜짐·꺼짐. 셀 안에 빈 줄로 나뉜 블록이 순서대로 들어간다.
  - H2 언어가 섞인 범위(한국어 셀 + 영어 셀): 영어 셀은 한국어로만 바뀐다.
  - H3 범위 Ctrl+Ctrl 표: 셀 위치 유지, Target 2개 이상이면 언어 표시, 숫자·빈 셀 표시.
  - H4 범위 Ctrl+Alt+Alt / Alt+Ctrl+Ctrl: 바뀐 한국어 셀만 쓰인다, 다 맞으면 `수정할 부분이 없습니다.`
  - H5 지원하지 않는 선택: 행 전체, 여러 범위, 병합 셀, 필터로 숨긴 행, 링크 셀(교체만), 201칸 이상 → 각 사유 문구.
  - H6 숫자·날짜 셀이 섞인 범위 → 현재 동작 기록(`선택한 셀을 읽을 수 없습니다.` 예상, S-ref §10.5).
  - H7 LLM 대기 중 다른 탭으로 이동 / 셀 수정 → `번역 중 시트·선택 범위 또는 내용이 바뀌어 교체를 취소했습니다. 다시 실행해주세요.`, 시트 변경 없음.
  - H8 셀 편집 중: 글자 선택 후 Alt+Alt / Ctrl+Alt+Alt, 선택 없이(셀 전체), 수식 셀(`=SUM(...)`) → 안내, 번역 결과가 `=`로 시작하는 경우.
  - H9 작업 중 사용자가 다른 텍스트를 복사 → 끝난 뒤 사용자의 복사 내용이 남아 있다.
  - H10 Target 3개 + 원문 포함, 긴 셀 → `셀 내용이 너무 깁니다. 더 작은 텍스트를 선택해주세요.`
  - H11 응답 시간 기록(셀 10개·30개·60개, Target 1개·3개).
- [ ] **Step 6: 커밋한다.** `feat(app): Google Sheets 조립과 수동 테스트 문서`
