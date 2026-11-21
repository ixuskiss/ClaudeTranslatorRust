# v5 Google Sheets behavioral reference (for the Rust port)

Source: `C:/python_workspace/ClaudeLangTrans_v5/src/claudelangtrans/` (paths relative to it). Read-only reference; line numbers = source lines. Companion: `v5-windows-ui-reference.md` (hereafter **UI-ref**). Already covered there and NOT repeated here: `Win32MultiFormatClipboard` snapshot/restore and `ClipboardGuard`/`wait_for_clipboard_change` rules (UI-ref 2.3), the non-Sheets role-classifier rules and URL predicates (UI-ref 2.5.2), `SheetsEditSafeInteraction` (UI-ref 2.7), `CtrlCtrlController.run_sheets` / `_show_result` dispatch (UI-ref 3.4), the full user-message string table (UI-ref 3.5), and the app.py Alt+Alt / Ctrl+Alt+Alt / Alt+Ctrl+Ctrl / Ctrl+Ctrl skeletons and Sheets routing paragraph (UI-ref 5.3-5.7).

Conventions as in UI-ref. All timeouts are seconds. "Copy" = synthetic `{Ctrl}c` via `uiautomation.SendKeys` (which adds its own ~0.5 s settle, UI-ref 2.5/7.11). Nothing in this feature ever logs cell text: logs carry counts, statuses, exception type names and the parser's fixed error phrases only.

Header comments in `clipboard_html.py:1-7` and `windows_sheets_backend.py:1-10` say the attribute names and the Name Box / copy / paste round trip were written from unverified assumptions ("spike H1-H7"); later comments record real-Chrome observations dated 2026-10-13 and 2026-10-27. Treat every "observed" rule below as load-bearing.

---

## 1. Data model, routing, role classifier

### 1.1 Model (`sheets/model.py`)
- `CellKind` = `TEXT|FORMULA|NUMBER|DATE|BOOLEAN|EMPTY|OTHER` (values = lowercase names) (`:5-12`).
- `SheetCell{row, col (0-based inside the selected range), kind, display_text, has_rich_text, has_link, row_span=1, col_span=1, formula: Option<str>}` (`:15-26`).
- `SheetGrid{rows, cols, cells (parse order = document order of <td>), html (the fragment actually parsed), cell_spans: [(start,end)] (Python str index) offsets of each `<td ...>...</td>` in `html`, same order as cells, compact_json: Option<str>}`; `cell(r,c)` linear search, KeyError if absent (`:29-45`). Offsets are Python string (code-point) indices; Rust must use one consistent unit (byte offsets into the UTF-8 `String` are fine if produced and consumed by the same code).
- `SheetLocation{document_id, sheet_gid: Option, sheet_name: Option, range_address: Option}` (`:48-53`). `sheet_name` is ALWAYS `None` in production (`windows_sheets_backend.py:149`).
- `SheetSelection{location, grid, hidden_or_filtered: Option<bool>}`; `None` = could not be determined, never treated as "not hidden" (`:56-61`).
- Outcome types (`sheets/outcome.py`): `SheetsStatus` (`:9-21`): `SUCCESS, UNSUPPORTED_RANGE, NO_TEXT, TRANSLATION_FAILED, TARGET_CHANGED, WRITE_FAILED, WRITE_UNVERIFIED, CAPTURE_FAILED, WRONG_INPUT_STATE, NO_KOREAN_TEXT, NO_CHANGES, CORRECTION_FAILED`. `SheetsOutcome{status, reason: UnsupportedReason | BatchFailure | None}`; `user_message()` texts = UI-ref 3.5 (`:57-66`): UNSUPPORTED_RANGE uses the reason text (empty/unknown reason -> `이 선택 범위는 지원하지 않습니다.`); TRANSLATION_FAILED/CORRECTION_FAILED with reason `CELL_TOO_LARGE` -> `셀 내용이 너무 깁니다. 더 작은 텍스트를 선택해주세요.`; any other reason is ignored for the text. `TableCell{row, col, original, translation: Option}`; `SheetsTableResult{location, rows, cols, cells}` (`:69-82`).

### 1.2 Routing (`sheets/routing.py`)
- `SheetsInputState` = `CELL_SELECTION | CELL_EDITING | OTHER`; `Route` = `SHEETS_RANGE | TEXT_INTERACTION | SHEETS_UNSUPPORTED` (`:13-22`).
- `choose_route(url, state)` (`:29-37`): url empty/None or not `is_google_sheets_url` (substring `docs.google.com/spreadsheets`) -> `TEXT_INTERACTION`; `CELL_SELECTION` -> `SHEETS_RANGE`; `CELL_EDITING` -> `TEXT_INTERACTION`; anything else (incl. `None` state) -> `SHEETS_UNSUPPORTED`.
- `input_state_from_role(role, editor_value=None)` (`:40-55`), in order:
  1. `role == "sheets_grid"` -> `CELL_SELECTION`.
  2. `role == "sheets_cell_editor"` and `editor_value is not None` and `editor_value.strip("\n") == ""` -> `CELL_SELECTION` (observed 2026-10-27: a selected range can leave the address as the editor's name with an empty value). Strip only `\n`; a space-only value stays editing. `None` (unreadable) keeps editing.
  3. role in `sheets_cell_editor`, `sheets_formula_bar` -> `CELL_EDITING`.
  4. else (`sheets_name_box`, `unknown`, ...) -> `OTHER`.
- `parse_sheets_url(url)` (`:58-67`): not Sheets -> `(None, None)`; document id = regex `/spreadsheets/d/([^/?#]+)` (no match -> `(None, None)`); gid = regex `[#&?]gid=(\d+)` (first match; `None` if absent). Both returned as strings.

### 1.3 Role classifier, Sheets branch (`interaction/windows_role_classifier.py:117-146`)
Reached only after the Gmail/Chat/Docs/Slides branches miss (UI-ref 2.5.2 has the order). `name` = UIA Name, `lname = name.lower()`, `control_type` = UIA ControlTypeName, `automation_id` = UIA AutomationId (passed only for Sheets URLs, `windows_uia_backend.py:277-281`, which re-classifies with the id after any stray-focus recovery).
1. `ComboBoxControl`: `automation_id != "waffle-rich-text-editor"` -> `unknown` (font/zoom combos); `name == ""` -> `sheets_grid`; `re.fullmatch(r"[A-Z]+[1-9][0-9]*", name)` -> `sheets_cell_editor`; else `unknown`. (Observed 2026-10-13: both selection and editing focus this combobox.)
2. `ButtonControl` -> `unknown`.
3. `EditControl`: `automation_id == "t-name-box"` or `"name box" in lname` or `"이름 상자" in name` -> `sheets_name_box`; `"formula" in lname` or `"수식" in name` -> `sheets_formula_bar`; any hint of `_SHEETS_NON_CELL_EDIT_HINTS` (`:43-46`: `address, search, find, omnibox, rename, title, comment, filter, 주소, 검색, 찾기, 제목, 댓글, 필터, 이름 바꾸기`) in `lname` or `name` -> `unknown`; else `sheets_cell_editor`.
4. control type in `("", "PaneControl", "DocumentControl", "GroupControl", "CustomControl")` (`:49`) -> `sheets_grid`; else `unknown`.

### 1.4 `WindowsSheetsBackend.current_url_and_state()` (`interaction/windows_sheets_backend.py:81-100`)
1. Ensure COM for this thread. `GetForegroundControl()`; missing or class not starting `Chrome_WidgetWin` -> `(None, None)`.
2. `url = uia.chrome_url(foreground)` (omnibox). Not a Sheets URL -> `(url, None)` (no focus work, no keys: cheap for every non-Sheets trigger).
3. `uia.resolve_focus()` (same Alt-release wait <= 0.5 s and stray-toolbar recovery as the text path, UI-ref 2.6). `None` -> `(url, None)`; control `None` -> `(resolved_url or url, None)`.
4. If the backend was built with `empty_editor_is_selection=True` and role is `sheets_cell_editor`: `editor_value = _read_editor_value(control)` (ValuePattern.Value with ONE trailing `\n` removed; any error -> `None`, `windows_uia_backend.py:346-356`).
5. Return `(resolved_url or url, input_state_from_role(role, editor_value))`.

Two backend instances exist (`app.py:278-291`): `sheets_backend` (Alt+Alt, Ctrl+Alt+Alt; `assume_editable = cfg.range_replace_enabled`, `empty_editor_is_selection=False`) and `explain_sheets_backend` (Ctrl+Ctrl, Alt+Ctrl+Ctrl; `assume_editable=False`, `empty_editor_is_selection=True`). Both get `max_range_cells = cfg.max_range_cells` and share one `WindowsUiaBackend` (built with `sheets_select_all_enabled = cfg.edit_full_cell_replace_enabled`, a flag that is stored but never read, `windows_uia_backend.py:151`).

`app._sheets_route(backend)` (`app.py:189-197`) wraps this: any exception -> log `Sheets route detection failed` and `(None, TEXT_INTERACTION)`; else `(url, choose_route(url, state))`.

---

## 2. Range capture

### 2.1 `capture_selection()` step by step (`windows_sheets_backend.py:105-149`)
Whole body runs inside `clipboard_section()` (process-wide re-entrant lock).
1. `(url, state) = current_url_and_state()` (a second full focus resolution; routing already did one). `parse_sheets_url(url or "")`. `document_id is None` or `state != CELL_SELECTION` -> `SheetsCaptureError("focus is not on a Sheets cell selection")` (no reason -> CAPTURE_FAILED).
2. Read the Name Box BEFORE copying (`_read_range_address`, 2.2). `parsed = parse_range_address(address)` if address else `None`.
3. `parsed` is an `UnsupportedReason` other than `UNIDENTIFIED_TARGET` -> `SheetsCaptureError("unsupported selection", reason=parsed)` (whole row/column, multiple ranges: never copied).
4. `parsed` is `(rows, cols)` and `rows*cols > max_range_cells` -> `SheetsCaptureError("range too large", reason=RANGE_TOO_LARGE)` (never copied).
5. `_copy_selection_data()` (2.3) -> `(cf_html, custom_bytes)`.
6. `grid = parse_sheets_html(extract_cf_html_fragment(cf_html), compact_data=compact_from_custom(custom_bytes))`. `SheetsParseError | ValueError | KeyError | TypeError` -> log `Sheets clipboard metadata rejected: <Type>(<msg>) cause=<Type>(<msg>)` and raise `SheetsCaptureError("copied data is not recognizable Sheets cells")` (CAPTURE_FAILED).
7. `hidden = False` if `parsed == (grid.rows, grid.cols)` else `None`.
8. Return `SheetSelection(SheetLocation(document_id, gid, None, address), grid, hidden)`.
`reread_selection()` is literally `capture_selection()` again (`:151-152`).

### 2.2 `_read_range_address()` (`:232-251`)
`GetForegroundControl()`, then try, in order, `EditControl(AutomationId="t-name-box")`, `EditControl(SubName="이름 상자")`, `EditControl(SubName="Name Box")`, each with `searchDepth=24` (`_NAME_BOX_SEARCH_DEPTH`) and `Exists(0, 0)` (no wait). First one whose ValuePattern value is a non-blank string -> return it `.strip()`ed. Any exception or no hit -> `None`. (Observed: the label is on a parent GroupControl; the input's own Name is empty, hence AutomationId first.)

### 2.3 `_copy_selection_data()` (`:214-230`)
1. `guard = ClipboardGuard(clipboard); guard.begin()` (multi-format snapshot).
2. `before = sequence_number()`; `_wait_until_alt_released()` (<= 0.5 s, poll 0.02); `SendKeys("{Ctrl}c")`.
3. `wait_for_clipboard_change(clipboard, before, 1.0)` (`_COPY_TIMEOUT_SECONDS`; stable 0.05, poll 0.02). False -> `SheetsCaptureError("copy produced no (or unsettled) clipboard data")`.
4. `guard.mark_own_write()`; `html = read_html()` (registered `HTML Format`, UTF-8 decode with replacement); `custom = read_custom_data()` (registered `Chromium Web Custom MIME Data Format`, raw bytes or `None`).
5. `finally: guard.finish()` (restores the user's clipboard unless the user changed it meanwhile).
6. `html` empty/None -> `SheetsCaptureError("copied data has no HTML")`.
CF_UNICODETEXT is never read on the range path.

### 2.4 CF_HTML helpers (`sheets/clipboard_html.py:17-50`)
- `extract_cf_html_fragment(cf_html)`: header fields by regex `^(StartFragment|EndFragment):(\d+)\s*$` (multiline). Either missing -> `SheetsParseError("CF_HTML header has no fragment offsets")`. Offsets are BYTE offsets into the UTF-8 encoding of the whole string; require `0 <= start <= end <= len(bytes)` else `SheetsParseError("CF_HTML fragment offsets out of range")`; return `bytes[start:end]` decoded UTF-8 (strict).
- `build_cf_html(fragment)`: header `Version:0.9\r\nStartHTML:{:010d}\r\nEndHTML:{:010d}\r\nStartFragment:{:010d}\r\nEndFragment:{:010d}\r\n`, then prefix `<html><body>\r\n<!--StartFragment-->`, fragment, suffix `<!--EndFragment-->\r\n</body></html>`. StartHTML = header byte length (fixed, 10-digit fields); StartFragment = StartHTML + len(prefix bytes); EndFragment = StartFragment + len(fragment UTF-8 bytes); EndHTML = EndFragment + len(suffix bytes).

### 2.5 Chromium custom MIME map (`sheets/custom_clipboard.py`)
- Constants: `MIME = "application/x-vnd.google-spreadsheet-compact-table+json"`, `FORMAT = "Chromium Web Custom MIME Data Format"` (`:10-11`).
- `decode_custom_data(raw)` (`:14-39`): `len < 8` or `> 10_000_000` -> error; LE u32 `size`, u32 `count`; require `size == len-4` and `count <= 100`; then `count` pairs of strings, each = u32 length in UTF-16 code units + UTF-16LE bytes, next offset rounded up to 4; truncation, duplicate key, or trailing bytes -> `ValueError`.
- `encode_custom_data(map)` (`:42-49`): payload = u32 count, then per string u32 unit-length + UTF-16LE + zero pad to 4; output = u32 `len(payload)` + payload.
- `compact_from_custom(raw)` (`:155-159`): empty -> `None`; decode; value under `MIME` -> `json.loads`; absent -> `None`.
- `expand_runs(values, count)` (`:52-72`): run-length list; a negative int `-n` means "repeat the next value n times"; non-int, negative value, overflow or final length != count -> `ValueError`.
- `compact_cells(data)` (`:75-126`), fail-closed decoder returning `(rows, cols, items)`:
  1. `rows, cols = data["15"]["1"], data["15"]["2"]`; must be ints with `0 < rows*cols <= 10000`.
  2. `flags = expand_runs(data["2"], rows*cols)`; `types = data["3"]["1"]`, `strings = data["3"]["4"]`, `formula_indices = data["9"]`, `formulas = data["8"]`, `runs = data["17"]` (all default empty).
  3. Per cell flag: any bit outside `0x4CB` -> `"unverified compact cell attributes"`. Bit `1` (value): next type must be `2` (text) or `4` (quote-prefixed text `'aaa`), next string must be a str -> item text/string_index. Any other type (numbers, dates, booleans) -> `"unverified compact value type"` => the whole capture fails (CAPTURE_FAILED) when compact data is present. Bit `8` (formula): next formula index must be a valid int into `formulas`, formula must be a str starting `=`. Bit `1024` (character runs): next entry of `data["17"]` must be a dict -> `rich_index`.
  4. Every list must be fully consumed (`"unused compact values"`, `"unused character run payload"`).
- `replace_compact_strings(data, {(r,c): text})` (`:129-152`): deep copy; each target must be inside the table and a non-formula string cell; overwrite `["3"]["4"][string_index]`; if the cell had runs, clear bit 1024 and drop that run; if any run was dropped, `["2"]` becomes the fully EXPANDED flag list (not re-encoded) and `["17"]` the remaining runs (key removed when empty). Types (2 or 4) are kept, so quote-prefixed text stays forced text.

### 2.6 HTML grid parser (`parse_sheets_html(fragment, compact_data=None)`, `clipboard_html.py:172-221`)
1. If `compact_data` given: `metadata = compact_cells(...)`; any error -> `SheetsParseError("unsupported compact metadata")`.
2. Single-cell special case: marker `google-sheets-html-origin` absent AND metadata is 1x1 AND the fragment full-matches (DOTALL) `(?:<style\b.*?</style>)?\s*<span(?P<attrs>[^>]*\bdata-sheets-root=["']1["'][^>]*)>(?P<content>.*)</span>\s*` -> fragment is REWRITTEN to `<google-sheets-html-origin><table><tr><td{attrs}>{content}</td></tr></table>`. This synthetic HTML becomes `grid.html` and is what the write payload is built from.
3. Marker still absent -> `SheetsParseError("not Google Sheets clipboard HTML")`.
4. Run `_GridParser` (charrefs decoded):
   - `<table>` increments a table counter; `<tr>` increments `row` (starts -1).
   - `<td>`/`<th>` only when `row >= 0`: column = first column not in the occupied set for this row; `rowspan`/`colspan` = `max(1, int(attr or "1"))` (non-int -> ValueError -> capture fails); mark all spanned (r,c) occupied; `max_cols = max(max_cols, col+col_span)`; `link` starts true if any attribute name starts with `data-sheets-hyperlink`; records start offset.
   - Inside a cell: `<br>` appends `\n`; `<a>` sets link; any other start tag sets `rich`, EXCEPT a `<div>` whose attributes are all `style` and whose style does not match `(?:font|color|text-decoration|vertical-align)\s*[-:]` (case-insensitive) (plain overflow divs occur in real copies). Self-closing tags are handled as start tags.
   - Text data appended. `</td>`/`</th>`: end offset = index just past the next `>`; cell built.
5. Exactly one table else `SheetsParseError("expected exactly one table, found N")`; no cells -> `SheetsParseError("table has no cells")`.
6. Legacy kind mapping without metadata (`_build_cell`, `:131-159`): `data-sheets-formula` present -> FORMULA; no parseable `data-sheets-value` dict -> EMPTY if text blank else OTHER; value `"1"`: `2` -> TEXT (EMPTY if text == ""), `3` -> DATE if `data-sheets-numberformat` `"1"` in `(5,6,7)` else NUMBER, `4` -> BOOLEAN, else OTHER. `has_rich_text = rich or "data-sheets-textstyleruns" attr present`.
7. With metadata: require `(row+1, max_cols, len(cells)) == (rows, cols, len(items))` else `"HTML and compact dimensions differ"`; per cell (zip order) `display_text == item.text` else `"HTML and compact values differ"`; kind = FORMULA if item formula, TEXT if string_index set, else EMPTY; `formula = item.formula`; `has_rich_text |= rich_index is not None`.
8. `SheetGrid(rows=row+1, cols=max_cols, cells, html=fragment, cell_spans, compact_json = json.dumps(compact_data, ensure_ascii=False) or None)`.

### 2.7 Support checks (`sheets/support.py`)
- `SheetsMode` = `REPLACE` (Alt+Alt, Ctrl+Alt+Alt) | `EXPLAIN` (Ctrl+Ctrl, Alt+Ctrl+Ctrl) (`:10-12`).
- `parse_range_address(address)` (`:42-60`): strip; remove sheet prefix `^(?:'(?:[^']|'')*'|[^!]+)!`; empty -> `UNIDENTIFIED_TARGET`; contains `,` -> `MULTIPLE_RANGES`; `^[A-Za-z]{1,3}:[A-Za-z]{1,3}$`, `^\d+:\d+$`, or open-ended `A1:B` / `A:B1` -> `WHOLE_ROW_OR_COLUMN`; single cell `^([A-Za-z]{1,3})(\d+)$` -> `(1,1)`; `cell:cell` -> `(|r2-r1|+1, |c2-c1|+1)` (columns base-26, A=1, case-insensitive; reversed corners allowed); anything else -> `UNIDENTIFIED_TARGET`.
- `check_range_support(selection, mode, max_range_cells)` (`:63-89`), first hit wins:
  1. address `None` -> `UNIDENTIFIED_TARGET`.
  2. parse reason -> that reason.
  3. parsed != grid dims: grid smaller-or-equal in both -> `HIDDEN_OR_FILTERED` (rows were dropped from the copy); else `UNIDENTIFIED_TARGET`.
  4. `hidden_or_filtered is not False` -> `HIDDEN_OR_FILTERED`.
  5. any span > 1 -> `MERGED_CELLS`.
  6. `rows*cols > max_range_cells` -> `RANGE_TOO_LARGE`.
  7. REPLACE only: any `OTHER` cell -> `UNKNOWN_CELL_TYPE`; any TEXT cell with `has_link` -> `RICH_TEXT_OR_LINK`. Rich text WITHOUT a link is allowed in both modes (replaced as plain text). EXPLAIN never rejects links/OTHER.
- `translatable_cells(grid)` (`:92-94`): kind TEXT and `display_text.strip()` non-empty, sorted by (row, col).
- `column_letters(i)` 0-based -> `A..Z, AA..`; `range_origin(address)` -> 0-based (row, col) of the first corner after prefix removal, `(0,0)` when unknown (`:97-116`).
- Size limits summary: `max_range_cells` (default 200) checked pre-copy from the address and post-copy from the grid; `compact_cells` hard cap 10000 cells; custom data 10 MB / 100 entries; `max_cell_chars` per cell in the batch layer (3.3).

---

## 3. Batch LLM

### 3.1 Config (`config/model.py:30-52`, env in `config/loader.py:28-40`)
| field | default | env |
|---|---|---|
| `batch_max_cells` | `30` | `SHEETS_BATCH_MAX_CELLS` |
| `batch_max_chars` | `5000` | `SHEETS_BATCH_MAX_CHARS` |
| `max_cell_chars` | `5000` | `SHEETS_MAX_CELL_CHARS` |
| `max_range_cells` | `200` | `SHEETS_MAX_RANGE_CELLS` |
| `job_timeout_seconds` | `240.0` | `SHEETS_JOB_TIMEOUT_SECONDS` |
| `range_replace_enabled` | `True` | `SHEETS_RANGE_REPLACE_ENABLED` (true only when exactly `"1"`; default `"1"`) |
| `edit_full_cell_replace_enabled` | `True` | `SHEETS_EDIT_FULL_CELL_REPLACE_ENABLED` (same rule) |
Numeric env values must parse and be positive, else the default is used. `to_limits()` -> `BatchLimits{max_cells, max_chars, max_cell_chars, job_timeout_seconds}`. Read once at startup; not in the Settings UI.

### 3.2 Shapes (`llm/base.py:36-75`)
`BatchItem{item_id, text, source_language, destination_language}`; `BatchTranslationRequest{items, tone="neutral"}`; `BatchCorrectionRequest{items, mode="spelling"}` (items' languages are `"ko"`); both return `BatchTranslationResult{translations: {item_id: text}}`. Base provider methods raise `LLMProviderError("batch translation not supported by this provider")` / `("correction not supported by this provider")`. Mock: `"[{dest}] {text}"` / `"[교정] {text}"` (`llm/mock.py:45-68`).

### 3.3 `BatchTranslationService.translate_cells(cells, base, target, tone)` (`translation/batch.py:189-260`)
Input `CellText{key:(row,col), text}` (the translatable cells, sorted). Providers (dictionary entries, LLM provider, limits) are closures read per job.
1. Empty -> success with empty map (no LLM call).
2. For each cell `index`: `masked = mask(text, entries)` then `_renumber(masked, offset)`: every placeholder `⟦n⟧` owned by this cell becomes `⟦n+offset⟧` (mapping placeholders renamed to `⟦i+offset⟧`), `offset += len(mapping)` -> placeholders are unique across the job (`:40-58`).
3. `detector.detect(masked_text)`; `resolve_translation_direction(lang or None, base, target)`; `None` -> fail `DIRECTION_UNKNOWN` (whole job, before any LLM call).
4. `len(masked_text) > max_cell_chars` (code points) -> fail `CELL_TOO_LARGE`.
5. `item_id = f"c{index}"`; `BatchItem(id, masked_text, direction.source, direction.destination)`. Direction is per cell (mixed-language ranges work).
6. `plan_batches` (`:104-122`): greedy, order-preserving; start a new batch when the current is non-empty and `len >= max_cells` or `chars + size > max_chars` (`size = len(text)`).
7. `run_planned_batches` (`:125-147`): before EACH batch, `clock() - started > job_timeout_seconds` -> `TIMEOUT` (not a hard cancel of an in-flight call); `LLMProviderError` -> `LLM_ERROR` (message kept); other exceptions propagate. After all: response id set must equal request id set exactly else `LLM_ERROR "response ids do not match request"`.
8. Per item: `has_integrity_problem` = any `⟦\d+⟧` not owned by this cell, or `validate_integrity(mapping, text)` false -> `INTEGRITY`; `restore(text, mapping, target_language=destination)`; `PlaceholderIntegrityError` -> `INTEGRITY`.
9. All-or-nothing: success returns the complete `{key: text}` map; any failure returns `{}`. Failure log: `batch translate failed: failure=<v> cells=N batches=N elapsed=...`.
`BatchFailure` = `CELL_TOO_LARGE, DIRECTION_UNKNOWN, LLM_ERROR, INTEGRITY, TIMEOUT, NO_KOREAN_TEXT` (`:80-86`).

### 3.4 `BatchCorrectionService.correct_cells(cells, mode)` (`correction/batch.py:68-148`)
Differences from 3.3: `mode = normalize_correction_mode(mode)`; per cell `mask` -> `detect(masked)`; not `"ko"` (or undetected) -> SKIPPED (not sent, not returned); renumber only Korean cells; size check -> `CELL_TOO_LARGE`; `item_id = f"c{len(items)}"` (index among SENT items); languages `"ko"/"ko"`. No items -> `NO_KOREAN_TEXT`. After the id check: blank corrected text -> `LLM_ERROR "empty correction result"`; integrity as 3.3; `restore_fragment(text, mapping, None)` (keeps dictionary terms as written); blank restored -> `LLM_ERROR`; `_merge_whitespace(original, restored)` (`:43-50`): equal ignoring outer whitespace -> original unchanged; else original's leading/trailing whitespace + `restored.strip()`.

### 3.5 Prompts (`llm/claude_cli.py`)
Helpers already used by single-text prompts: `_formatting_instruction(model)` = `"Keep the same line breaks. "` for `haiku`, else the strict text (`:54-63`); `_tone_instruction(tone)` (`:25-36`, nothing for `neutral`); `_correction_preamble(mode)` (`:149-156`); `_POLISH_MERGE_SPLIT_INSTRUCTION = "You may merge or split sentences. "` (`:138`).
- Batch translation (`:79-103`), single string concatenation:
  `Translate the text of every item below from its source language to its target language. Translate each item independently: never merge, split, reorder, add or drop items, and return each item's id unchanged. Keep line breaks and tabs inside each text as they are. Preserve any tokens that look like ⟦N⟧ exactly as-is within the same item -- do not translate, move to another item, or remove them. ` + formatting + tone + `Respond with only JSON matching this shape: {"translations": [{"id": str, "translation": str}]} ` + `\n\nItems:\n` + `json.dumps({"items": [{"id", "source", "target", "text"}...]}, ensure_ascii=False)`.
- Batch correction (`:184-198`): `_correction_preamble(mode)` + (polish ? merge/split sentence : "") + `Proofread the text of every item below. Correct each item independently: never merge, split, reorder, add or drop items, and return each item's id unchanged. Keep line breaks and tabs inside each text as they are. ` + formatting + `Respond with only JSON matching this shape: {"corrections": [{"id": str, "corrected": str}]} ` + `\n\nItems:\n` + `json.dumps({"items": [{"id", "text"}...]}, ensure_ascii=False)`.
- Response validation `_collect_batch_texts` (`:201-231`; list/text keys `translations`/`translation` or `corrections`/`corrected`), each raising `LLMProviderError`:
  - top-level not a dict or key not a list -> `Claude CLI batch response missing '<list_key>' list`
  - entry not a dict, or `id`/text not strings -> `Claude CLI batch response has a malformed entry`
  - id not in request -> `... has an unknown id`; repeated -> `... has a duplicate id`
  - source text non-blank but result blank -> `... has an empty translation` / `... has an empty correction`
  - ids missing -> `Claude CLI batch response is missing ids`
  The JSON is obtained through the same `_ask_json` path as single-text calls (CLI error/empty/malformed JSON -> `LLMProviderError`).

---

## 4. Range write (Alt+Alt / Ctrl+Alt+Alt on a cell selection)

### 4.1 `_replace_cells(interaction, max_range_cells, transform, no_text_status, failure_status)` (`sheets/service.py:81-150`)
1. `capture_selection()`: `SheetsCaptureError` with reason -> `UNSUPPORTED_RANGE(reason)`, without -> `CAPTURE_FAILED`; any other exception -> log `Sheets capture failed: <Type>` -> `CAPTURE_FAILED`.
2. `check_range_support(..., REPLACE, ...)` reason -> `UNSUPPORTED_RANGE(reason)`.
3. `interaction.can_edit() is not True` -> `UNSUPPORTED_RANGE(UNIDENTIFIED_TARGET)`. Windows: `True` iff `assume_editable` (= `range_replace_enabled`), else `None` (`windows_sheets_backend.py:102-103`). No view-only detection exists; a blocked paste is caught only by verification.
4. `cells = translatable_cells(grid)`; none -> `no_text_status` (`NO_TEXT` / `NO_KOREAN_TEXT`). No LLM call.
5. `transform([CellText((r,c), display_text)])` -> map or `SheetsOutcome` (returned as is). Exceptions here are NOT caught (they escape to the action thread).
6. Target check: `reread_selection()` (a full new capture incl. Ctrl+C); any exception -> `TARGET_CHANGED`; `!same_target(before, current)` -> `TARGET_CHANGED`.
7. `fragment = build_replacement_html(grid, translations)`; `ValueError` -> `failure_status`.
8. `write_selection(selection, build_cf_html(fragment), None)` (HTML only: a text companion could replace formulas with their displayed values); any exception -> log `Sheets write failed: <Type>` -> `WRITE_FAILED`. Never retried.
9. Verify: `reread_selection()`; exception -> `WRITE_UNVERIFIED`; `!written_as_expected(grid, translations, after.grid)` -> log `Sheets write could not be verified` -> `WRITE_UNVERIFIED`. No retry, re-paste or undo.
10. `SUCCESS` (log `Sheets replace ok: cells=N translated=N total=...`).
- `perform_sheets_replace(interaction, batch, max_range_cells, base, target, tone)` (`:64-78`): transform = `translate_cells`; failure -> `TRANSLATION_FAILED(failure)`.
- `perform_sheets_correction_replace(interaction, correction_batch, max_range_cells, mode)` (`:209-233`): transform = `correct_cells`; failure -> `_correction_failure` (`NO_KOREAN_TEXT` -> `NO_KOREAN_TEXT`; else `CORRECTION_FAILED`, reason kept only for `CELL_TOO_LARGE`); keep only cells whose corrected text != original; none -> `NO_CHANGES` (nothing written); `no_text_status=NO_KOREAN_TEXT`, `failure_status=CORRECTION_FAILED`.

### 4.2 Payload (`sheets/write_payload.py`)
The original copied fragment is spliced by offset, never re-serialized (`:1-7`).
- `build_replacement_html(grid, translations)` (`:59-90`):
  1. Each key must exist (`ValueError "no cell at ..."`) and be `TEXT` (`ValueError "... is not a text cell"`).
  2. If `compact_json` present and translations non-empty: every FORMULA cell whose `<td>` open tag lacks `data-sheets-formula=` gets ` data-sheets-formula="<html-escaped formula>"` inserted before `>` (modern HTML carries formulas only in the compact data).
  3. Each target `<td>` replaced by `_new_td`.
  4. Splice edits back to front by cell index. Empty translations -> byte-identical output.
- `_new_td(old_td, text)` (`:35-56`): normalize `\r\n`/`\r` -> `\n`; open tag end found quote-aware (`_open_tag_end`, error `"unterminated <td> tag"`); remove ` data-sheets-textstyleruns="..."`; if a `data-sheets-value` attribute exists: html-unescape + JSON-parse it (non-dict/invalid -> `{}`), set `"1": 2` and `"2": text`, re-dump compact (`separators=(",",":")`, non-ASCII kept), html-escape with quotes, put back in place as `data-sheets-value="..."`; else append ` data-sheets-value="<escaped json.dumps({"1":2,"2":text})>"` (default separators `, ` / `: `) before `>`; content = `html.escape(text, quote=False)` (`& < >`) with `\n` -> `<br>`; keep from the last `</` of the old td. Everything between open tag and close tag (inner `<b>`, `<div>`, ...) is dropped.
- Formula-prefix handling on the RANGE path: there is no `=`/`+` refusal; value type `"1": 2` forces a string so `=SUM(...)` stays text (test `translation_starting_with_equals_stays_text`). The `=`/`+` refusal exists only on the cell-edit path (5).
- `build_replacement_tsv` (`:93-106`): grid matrix with translations, fields containing tab/newline/quote quoted with `""` escaping, rows `\n`, cells `\t`. Unused in production (service passes `text=None`); tests exist.

### 4.3 `write_selection(selection, cf_html, text)` (`windows_sheets_backend.py:154-212`)
Inside `clipboard_section()`:
1. Re-verify right before pasting: `current_url_and_state()`; document id and gid must equal the captured ones, state `CELL_SELECTION`, captured address not `None`, `_read_range_address() == captured address`; else `RuntimeError("target changed before paste")` (service maps to WRITE_FAILED, not TARGET_CHANGED).
2. `guard = ClipboardGuard(clipboard); guard.begin()`.
3. If `grid.compact_json` is set: parse the NEW fragment (`parse_sheets_html(extract_cf_html_fragment(cf_html))`, no metadata); `replacements = {(r,c): new_display_text}` for EVERY original TEXT cell with non-blank display text (not only translated ones); `updated = replace_compact_strings(json.loads(compact_json), replacements)`; `custom = encode_custom_data({MIME: json.dumps(updated, ensure_ascii=False)})`. Consequence: any non-blank text cell in the range (even an unchanged one in correction mode) loses its character runs in the native payload; whitespace-only cells keep them (test `native_paste_keeps_untranslated_whitespace_cell_formatting`). Errors here propagate -> WRITE_FAILED, guard not finished (snapshot simply dropped, clipboard untouched).
4. `write_html_and_text(cf_html, text, custom_data=custom?)`: OpenClipboard, EmptyClipboard, `HTML Format` = UTF-8 bytes, custom format bytes if any, CF_UNICODETEXT if text not None (never, in prod), Close (`win32_multiformat_clipboard.py:121-131`). The original copy-id/server token data is never replayed.
5. `guard.mark_own_write()`; `_wait_until_alt_released()`; `before = sequence_number()`.
6. `SendKeys("{Ctrl}v")` (never Enter, never Ctrl+Shift+V), then immediately `SendKeys("{Ctrl}c")`: Chrome processes keys in order, so a clipboard change from this copy proves the paste was consumed.
7. `confirmed = wait_for_clipboard_change(clipboard, before, 3.0)` (`_PASTE_CONFIRM_TIMEOUT_SECONDS`); if confirmed `guard.mark_own_write()`.
8. `finally`: confirmed -> `guard.finish()` (restore user clipboard unless changed by someone else); not confirmed -> log `Sheets paste not confirmed; leaving the clipboard untouched instead of restoring` and DO NOT restore. No exception is raised for "not confirmed": the flow continues to verification, and the user's previous clipboard is lost (our payload stays).

### 4.4 Verification (`sheets/verify.py`)
- `content_fingerprint(grid)` (`:8-29`): SHA-256 over cells sorted by (row,col) of `repr((row, col, kind.value, display_text, has_rich_text, has_link, row_span, col_span, formula))` UTF-8 + `\x00`. Only compared within one process: any stable encoding of the same tuple is equivalent. Never logged.
- `same_target(before, after)` (`:32-43`): both addresses non-None and equal, same document id, same gid, same (rows, cols), same fingerprint.
- `written_as_expected(before, translations, after)` (`:46-70`): same dims; every old cell exists in `after`; translated cells must be TEXT with `display_text == translation` exactly; every other cell must keep kind, display_text, formula, has_rich_text, has_link.

---

## 5. Cell-editing path (Sheets route `CELL_EDITING` -> text flow)

### 5.1 Focused element for the editor (`windows_uia_backend.py:294-374`)
`get_focused_element()`: role in `("sheets_cell_editor", "sheets_formula_bar")` -> `_sheets_editor_element` instead of the sentinel/select-all probe of the text path. Under `clipboard_section()`:
1. `_copy_selection_text_and_html()`: `ClipboardGuard.begin()`; `before = seq`; `SendKeys("{Ctrl}c")` (no explicit Alt wait here; `resolve_focus` already waited); `wait_for_clipboard_change(before, 0.3, poll 0.02)` (`_SHEETS_COPY_TIMEOUT_SECONDS`, `_SHEETS_COPY_POLL_SECONDS`); false -> `(None, None)`; else `mark_own_write`, return `(read_text() or None, read_html())`; any exception -> `(None, None)`; `finally guard.finish()`. Selection detection uses the sequence number (an empty selection makes Chrome copy nothing), not a sentinel.
2. `editor_text = _read_editor_value(control)` (ValuePattern, one trailing `\n` removed; error -> `None`).
3. `editor_format = html_rich_text_state(html)` (5.4).
Returns `FocusedElement(app_hint=url, role, name, full_text="", selection_text, supports_direct_text_replacement=False, editor_full_text=editor_text, editor_has_rich_text=editor_format)`; log `Sheets editor capture: full_chars=<n|None> selection_chars=<n> rich=<state>`. Never sends Ctrl+A during capture (tests assert no keys besides the copy).

### 5.2 `GoogleSheetsCellEditStrategy(allow_rich_text)` (`interaction/strategies.py:194-228`; UI-ref cites older line numbers)
`app_name = "google_sheets"`; matches `is_google_sheets_url(app_hint) and role in {sheets_cell_editor, sheets_formula_bar}`. `identify_context`: `editor_full_text is None` -> None; `full.lstrip().startswith("=")` -> None (formula, even if only a substring is selected); `selection_text` truthy -> `EditingContext("google_sheets", "sheets_cell_edit", has_selection=True, text=selection)`; `full.strip() == ""` -> None; else `(..., has_selection=False, text=full)`. `allow_rich_text` and `editor_has_rich_text` are never consulted: rich cells are replaced as plain text.

### 5.3 Replace guards and keys
Stack (UI-ref 2.5): `Retrying(SheetsEditSafe(ChromeAdapter[..., GoogleSheetsCellEdit(allow_rich_text=False)]), allow_full_cell_replace=cfg.edit_full_cell_replace_enabled)`; read-only stack uses `allow_rich_text=True` and no SheetsEditSafe.
1. `SheetsEditSafeInteraction.replace` (UI-ref 2.7): `new_text.lstrip()` starts with `=` or `+` -> `translation_starts_with_formula_prefix`; no selection and full-cell replace disabled -> `full_cell_replace_not_enabled`; re-capture through the inner adapter (another editor copy) and compare the WHOLE `EditingContext` (app, field_id, has_selection, text) -> mismatch/None -> `target_changed`.
2. `ChromeAdapter.replace` then re-probes again (third copy) comparing only app + field_id (UI-ref 2.5) and pastes via the text-only `ClipboardFallback` (EmptyClipboard; NOT under `clipboard_section`): selection -> `{Ctrl}v`; full cell -> `{Ctrl}a`, sleep 0.2, `{Ctrl}v` (`windows_uia_backend.py:499-511`), each after the <= 0.5 s Alt-release wait. Ctrl+A inside the editor stays in the cell (user-confirmed 2026-10-13, `config/model.py:43`). Enter is never sent: the edit stays open for the user to commit.
3. User messages for these codes and for capture failure on Sheets: UI-ref 5.3 / 5.5 (`app.py:155-187`).

### 5.4 Rich-text detectors
- `html_rich_text_state(clipboard_html)` (`clipboard_html.py:253-272`): empty -> `None`. Contains `google-sheets-html-origin` -> `True` iff it contains `data-sheets-textstyleruns`, `data-sheets-hyperlink` or `<a `, else `False`. Otherwise (editor copy): if it starts with `Version:` extract the fragment (failure -> `None`); `_EditorFormatParser` (`:224-250`): counts `<span data-sheets-root="1">` roots; any non-`br` start tag inside the root, or any attribute starting `data-sheets-hyperlink`/`data-sheets-textstyleruns` inside it -> rich; result `None` unless exactly one root that was closed, else `rich`. Value is only logged/stored.
- `editor_rich_text_state(text_range)` (`sheets/editor_format.py`): UIA TextAttributeIds `40005, 40006, 40007, 40008, 40001, 40014, 40016, 40017, 40030, 40026` must all be plain str/int/float/bool (mixed sentinel -> `None`); link attribute `40035` falsy -> `False`, scalar -> `True`; errors -> `None`. Not called from production code (tests only).

---

## 6. Clipboard save/restore as used by Sheets
Mechanics are in UI-ref 2.3 (`Win32MultiFormatClipboard`, `ClipboardGuard`, `wait_for_clipboard_change`, `clipboard_section`). Usage map:
| step | lock | guard | wait | restore rule |
|---|---|---|---|---|
| range copy (`_copy_selection_data`) | yes (via `capture_selection`) | begin -> copy -> mark -> finish | change within 1.0 s | normal guard rule |
| range paste (`write_selection`) | yes | begin -> write payload -> mark -> Ctrl+V, Ctrl+C -> mark if confirmed | change within 3.0 s | only if confirmed; else leave payload |
| editor copy (`_copy_selection_text_and_html`) | yes | begin -> copy -> mark -> finish | change within 0.3 s, poll 0.02 | normal guard rule |
| editor paste (text path) | no | text-only `ClipboardFallback` (UI-ref 2.2) | none | text only |
A single Alt+Alt range run therefore takes the lock 4 times (capture, pre-write reread, write, verify reread) and sends 4 Ctrl+C plus 1 Ctrl+V; each capture also re-resolves focus. The lock is re-entrant (RLock); Alt and Ctrl hotkey families run on different threads and serialize on it.

---

## 7. Table view (Ctrl+Ctrl / Alt+Ctrl+Ctrl on a cell selection)

### 7.1 Building the result (`sheets/service.py:153-261`)
- `capture_for_table(interaction, max_range_cells)`: same capture error mapping as 4.1 step 1; `check_range_support(..., EXPLAIN, ...)` -> `UNSUPPORTED_RANGE(reason)`; else the selection. No `can_edit` check; links, rich text and OTHER cells allowed.
- `translate_for_table(selection, batch, base, target, tone)`: no translatable cells -> `NO_TEXT`; batch failure -> `TRANSLATION_FAILED(failure)`; else `SheetsTableResult(location, rows, cols, cells)` with one `TableCell` for EVERY grid cell sorted by (row, col): `original = display_text` (numbers/dates/formulas show their display value), `translation = map.get((r,c))` (`None` for non-translated cells).
- `correct_for_table(selection, correction_batch, mode)`: no translatable cells -> `NO_KOREAN_TEXT`; failure -> `_correction_failure`; else same shape; non-Korean cells get `translation=None`; unchanged Korean cells get translation == original (shown twice).
- Controller: `run_sheets` (UI-ref 3.4): capture first, loading message only after a successful capture (`Translating...` or `한국어 교정 중...`/`한국어 수정 중...`); an exception in the transform -> `SheetsOutcome(failure_status)` (`TRANSLATION_FAILED` default, `CORRECTION_FAILED` for Alt+Ctrl+Ctrl).

### 7.2 Rendering (`ui/explanation.py:74-104, 212-224, 345-405`)
`display_sheet_table(result)`:
1. `_clear_cards()` (also removes any previous table, hides caption, re-shows the scroll area); clear and hide status.
2. Caption: `sheet = sheet_name or ("gid {gid}" if gid else "")`; parts = non-empty of `[sheet, range_address]` joined by ` · `; visible only if any part. Caption style `color text_secondary`, bold. In practice `gid 0 · A1:C3` (sheet_name is always None).
3. `QTableWidget(rows, cols)`: no edit triggers, no selection, background `card_background`. Headers from `range_origin(address)`: columns `column_letters(first_col + c)`, rows `str(first_row + r + 1)` (unknown address -> `A..`, `1..`). Every column width `220` (`_SHEET_COLUMN_WIDTH`).
4. Cells with `original == ""` and `translation is None` get no widget (stay empty). Others: `SheetCellWidget`: VBox margins `(6,4,6,4)`, spacing 2; original label bold `text_primary`; translation label (only if not None) bold `accent`; both `PlainText` format (a cell containing `<b>` is shown literally), word wrap, mouse-selectable.
5. Row height = `max(28, max over its cells of (layout.totalHeightForWidth(220) if heightForWidth else 0, widget.sizeHint().height))`.
6. Hide the scroll area; insert the table at the scroll area's position in the panel layout (between caption and footer); keep a reference.
7. Size: `maxW = min(700, avail.w - 40)`, `maxH = min(800, avail.h - 80)` (primary screen); `width = clamp(verticalHeader.width + sum(colW) + 60, 480, maxW)`, `height = clamp(horizontalHeader.height + sum(rowH) + 130, 240, maxH)`; `resize`. The table scrolls itself beyond that.
`show_loading`, `show_error` and `display_result` all call `_clear_cards()`, so any later status or sentence result removes the table. `display_sheet_table` is called without `bring_to_front` (the loading step raised the window).

---

## 8. End-to-end Sheets flows
Messages: all go to the explanation window as `Error: <msg>`; exact strings are in UI-ref 3.5 (SheetsOutcome) and UI-ref 5.3/5.5 (cell-edit codes, capture failure, range-disabled). "outcome msg" below = `SheetsOutcome.user_message()`.

### 8.1 Alt+Alt (`app.py:400-452`)
1. `_precheck_replace_route` with `sheets_backend` (`app.py:382-395`): `SHEETS_UNSUPPORTED` -> WRONG_INPUT_STATE msg, stop; `SHEETS_RANGE` and `!range_replace_enabled` -> range-disabled msg, stop. `on_sheets = route != TEXT_INTERACTION or "docs.google.com/spreadsheets" in url`.
2. Badge at cursor (`Translating...`).
3. `SHEETS_RANGE` -> `perform_sheets_replace(sheets_backend, batch_translation_service, max_range_cells, base, target, tone)`; badge hidden in `finally`; status != SUCCESS -> outcome msg; SUCCESS silent.
   | outcome | when |
   |---|---|
   | `UNSUPPORTED_RANGE(reason)` | 2.1 steps 3-4, 2.7, or `can_edit` not True (`UNIDENTIFIED_TARGET`) |
   | `CAPTURE_FAILED` | not a selection at capture time, no copy, no HTML, parse/compact rejection, unexpected error |
   | `NO_TEXT` | no non-blank TEXT cell |
   | `TRANSLATION_FAILED` | any BatchFailure (CELL_TOO_LARGE has its own text); also a `build_replacement_html` ValueError |
   | `TARGET_CHANGED` | pre-write reread failed or differs |
   | `WRITE_FAILED` | paste-time re-verify mismatch, compact/encode or clipboard errors in `write_selection` |
   | `WRITE_UNVERIFIED` | post-write reread failed or content mismatch |
4. Otherwise (`CELL_EDITING` or non-Sheets) -> `perform_alt_alt_translate(adapter, ...)` with the 5.3 guards; on Sheets: capture failure -> `편집 중인 셀을 번역할 수 없습니다(수식·서식 또는 셀 상태를 확인할 수 없음).`; translation failure silent; replace error codes mapped (UI-ref 5.3).

### 8.2 Ctrl+Alt+Alt (`app.py:454-526`)
Base-language check and `wait_for_modifiers_released` (2.0 s) come BEFORE routing (UI-ref 5.5). Then the same precheck with the correction range-disabled text; badge with `correction_loading_message(mode)`; `SHEETS_RANGE` -> `perform_sheets_correction_replace(sheets_backend, batch_correction_service, max_range_cells, mode)`; outcomes as 8.1 except `NO_KOREAN_TEXT` (no translatable or no Korean cell), `NO_CHANGES` (all corrected == original, nothing written), `CORRECTION_FAILED` (batch failure or payload ValueError). Non-range -> `perform_correction_replace` with the correction message variants.

### 8.3 Ctrl+Ctrl (`app.py:569-601`)
Refresh controller state; route with `explain_sheets_backend` (empty editor with an address name = selection). `SHEETS_UNSUPPORTED` -> `run_sheets(capture=-> WRONG_INPUT_STATE)` (message shown, no loading). `SHEETS_RANGE` -> `run_sheets(capture_for_table(explain_sheets_backend, max_range_cells), translate_for_table(sel, batch_translation_service, base, target, tone))`: outcome -> outcome msg; success -> table (7.2). Otherwise -> `controller.run()`: in a cell editor the read-only strategy list ends with `GoogleSheetsCellEdit(allow_rich_text=True)` (formula cells and empty cells without a selection -> capture None -> `텍스트를 선택한 후 Ctrl+Ctrl을 실행해주세요.`), result shown as sentence cards.

### 8.4 Alt+Ctrl+Ctrl (`app.py:528-567`)
Base check, modifier wait (fail -> `show_correction_no_selection`), then route with `explain_sheets_backend`: UNSUPPORTED -> WRONG_INPUT_STATE msg; RANGE -> `run_sheets(capture_for_table, correct_for_table(sel, batch_correction_service, mode), failure_status=CORRECTION_FAILED, loading_message=correction_loading_message(mode))`; else `run_correction(correct_sentences)` (cell editor text via the read-only strategy). No badge.

---

## 9. Unit tests to port (`C:/python_workspace/ClaudeLangTrans_v5/tests/unit/`)
| file | count | notable cases |
|---|---|---|
| `sheets/test_routing.py` | 6 | route table; role->state incl. empty editor as selection only when value is empty; gid in `#`/query/missing; non-Sheets URL |
| `sheets/test_support.py` | 14 | address grammar; unknown hidden blocks both modes; grid smaller than address -> HIDDEN_OR_FILTERED, larger -> UNIDENTIFIED; rich text allowed, link blocks REPLACE only; merged blocks both; OTHER blocks REPLACE only; too large |
| `sheets/test_clipboard_html.py` | 14 | mixed types keep positions; trailing empty cells; newline/tab/quote stay in one cell; rich/link flags; merged spans occupy columns; cell_spans point at each td; non-Sheets HTML rejected; CF_HTML round trip with Korean; `html_rich_text_state` true/false/unknown; editor span plain vs rich |
| `sheets/test_custom_clipboard.py` | 11 | custom MIME round trip; compact replacement keeps formulas/empties/styles; invalid data rejected; compact types override display-text guessing; HTML/compact disagreement rejected; single-cell span with compact; quote-prefixed text stays text; unknown value type rejected |
| `sheets/test_rich_to_plain.py` | 4 | rich cell translated to plain; dropping runs leaves other rich cells/formulas intact; legacy textstyleruns attr removed with nested markup; missing run payload rejected |
| `sheets/test_write_payload.py` | 11 | no translations = byte-identical; only target content changes; untouched cells and target style byte-identical; value attr becomes string; `=`-leading translation stays text; `\n " < > &` round trip; formula/number/empty/unknown targets rejected; TSV quoting; CR removal |
| `sheets/test_verify.py` | 10 | fingerprint stability; same_target false on sheet/range/content change or unknown address; written_as_expected detects untranslated target, changed formula, modified untouched cell, target turned non-text, shape change |
| `sheets/test_clipboard_guard.py` | 11 | restore rules; restore failure swallowed; sections never overlap across threads; re-entrant; wait settles / times out / never stable |
| `sheets/test_service.py` | 33 | single write after all batches; nothing written on failure; reread differs/fails -> TARGET_CHANGED; other tab -> TARGET_CHANGED; link range aborts before LLM; no text skips LLM; unknown edit permission aborts; write exception -> WRITE_FAILED without retry; mismatch/reread failure after write -> WRITE_UNVERIFIED; table keeps layout and display values; correction writes only changed Korean cells; NO_CHANGES skips write; distinct user messages |
| `sheets/test_windows_sheets_backend.py` | 8 | Name Box via unnamed AutomationId control; missing Name Box -> None; empty editor selection opt-in; editor capture never sends Ctrl+A and copies only once; native paste keeps whitespace cell runs |
| `sheets/test_editor_format.py` | 3 | uniform UIA format plain; mixed -> None; link blocks |
| `sheets/test_diagnostic_collection.py` | 1 | manual diagnostics script keeps CF_HTML offsets (tooling; optional) |
| `interaction/test_sheets_edit.py` | 10 | `=`/`+` refusal; other apps untouched; full-cell blocked by default / allowed when enabled; selection never needs flag; text, context-gone, selection-state changes abort |
| `interaction/test_sheets_strategy.py` | 19 | editor roles only; formula with/without selection rejected; empty/unknown rejected; rich formatting still allows plain replace; classifier Sheets roles, combobox observed cases, unrelated combobox, unnamed Name Box |
| `translation/test_batch.py` | 17 | per-cell direction; dictionary terms per destination; multi-call batching; second batch failure -> nothing; missing cell / cross-cell placeholder swap fail whole job; undetectable cell fails pre-LLM; cell too large; timeout between batches; job-unique placeholders |
| `correction/test_batch.py` | 7 | only Korean cells sent/returned; NO_KOREAN_TEXT; terms kept; moved token -> INTEGRITY; whitespace kept; blank -> failure; LLM error and too large |
| `llm/test_batch_translate.py` | 7 | prompt embeds JSON items + placeholder rule; haiku soft formatting; tone; response violations raise; default provider raises; mock tagging |
| `config/test_sheets_config.py` | 6 | env overrides; defaults enabled; enabled only by exact `"1"`; invalid values -> defaults; `to_limits` |
| `ui/test_sheet_table.py` | 18 | layout, read-only, headers from address (fallback to positions), caption, plain-text cells, empty cells, multiline, switching back hides table, size bounds; `run_sheets` ordering and failure statuses |
| `ui/test_ctrl_ctrl_controller.py` | (2 Sheets) | `run_sheets` loading message given / default `Translating...` |

---

## 10. Things the Rust port must not forget
1. Read the Name Box BEFORE copying; reject whole row/column, multi-range and oversized selections without ever copying data. An unknown address still copies, then fails as `UNIDENTIFIED_TARGET`.
2. Routing runs `resolve_focus` (Alt wait + stray-toolbar recovery) and every capture runs it again; non-Sheets pages must stay cheap (foreground + URL only).
3. Use two backend instances: only the read-only (Ctrl family) one treats an empty, address-named cell editor as a range selection; only the replace one may report `can_edit == true` (iff `range_replace_enabled`).
4. CF_HTML offsets are BYTE offsets into UTF-8; build with 10-digit zero-padded fields and the exact prefix/suffix.
5. Prefer the compact custom MIME data when present; fail closed on any unknown flag bit (`& ~0x4CB`), value type other than 2/4, unconsumed list, or HTML/compact mismatch. Numbers/dates in a modern copy therefore make the capture fail (CAPTURE_FAILED) - preserve this unless deliberately changed.
6. Single-cell copies arrive as a `data-sheets-root` span; wrap them into the synthetic one-cell table only when compact metadata says 1x1, and build the paste from that synthetic HTML.
7. Write payload = splice the original HTML by offset; never re-serialize. Force `"1": 2` string values; strip `data-sheets-textstyleruns` and inner markup of targets only; re-add `data-sheets-formula` to formula cells when compact data exists; native payload strings replaced for every non-blank text cell; never replay copy-id tokens; never write CF_UNICODETEXT.
8. Paste = `Ctrl+V` then `Ctrl+C` and wait (3.0 s) for the clipboard to change before restoring; if unconfirmed, do not restore. Never Enter / Ctrl+Shift+V.
9. Three safety gates around the single write: pre-write full reread + `same_target` (TARGET_CHANGED), paste-time document/gid/state/address re-check (WRITE_FAILED), post-write reread + `written_as_expected` (WRITE_UNVERIFIED). No retry, re-paste or undo anywhere.
10. Batch layer is all-or-nothing, item ids `c{n}` (translation: index over all translatable cells; correction: index over sent Korean cells), job-wide unique `⟦n⟧` placeholders, foreign-placeholder check, exact id-set match, timeout checked between batches, sizes counted in characters (code points), greedy order-preserving batching with `max_cells`/`max_chars`.
11. Batch exceptions that are not `LLMProviderError` escape `perform_sheets_replace` uncaught in Python (only the controller path catches them); the port should map them to the failure status rather than crash the action thread.
12. Correction range: only Korean cells sent; only changed cells written; `NO_CHANGES` when nothing differs; outer whitespace of each cell preserved.
13. Cell-edit path: reject formulas (`=` after leading whitespace) at capture, refuse `=`/`+` results at replace, compare the full editing context on recapture, require the full-cell flag when there is no selection, Ctrl+A + 0.2 s + Ctrl+V for full-cell replace, editor copy detected by sequence number with a 0.3 s window, one trailing `\n` removed from the UIA value.
14. Clipboard: every Sheets copy/paste/restore under one process-wide re-entrant lock; multi-format snapshot + sequence-number guard; the editor PASTE uses the text-only fallback outside the lock (a known asymmetry).
15. `sheet_name` is never populated: caption shows `gid N · <address>`. Table: every grid cell shown, only translated cells get a second accent line, empty cells blank, plain-text labels, column width 220, min row height 28, size clamps `+60`/`+130` within 480x240..700x800 (screen-limited).
16. User text mapping lives in app.py and `SheetsOutcome.user_message()`; replace hotkeys notify every non-SUCCESS range outcome; SUCCESS is silent; a `build_replacement_html` error reports the translate/correct failure status, not WRITE_FAILED.
17. Never log cell text, fingerprints or clipboard content; parser error phrases are fixed and safe to log.
