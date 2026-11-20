# v5 Windows-integration + UI behavioral reference (for the Rust port)

Source: `C:/python_workspace/ClaudeLangTrans_v5/src/claudelangtrans/` (paths below are relative to it). Read-only reference. Line numbers = source lines. Not covered (already ported): LLM, dictionary, settings store, language detection, translation/correction services. Google Sheets internals are out of scope except routing (see 5.4).

Conventions: "UI thread" = Qt main thread. "action thread" = a fresh daemon thread per hotkey trigger. Python `uiautomation` = Windows UI Automation (COM) wrapper. All timeouts are seconds.

---

## 1. hotkey/

### 1.1 physical_keys.py
- `windows_modifier_is_down(key)` (`hotkey/physical_keys.py:15-20`): `GetAsyncKeyState(vk) & 0x8000`; `ctrl` -> VK_CONTROL `0x11`, `alt` -> VK_MENU `0x12` (either side). (`:12`)
- `wait_for_modifiers_released(is_down, timeout=2.0, poll=0.02)` (`:23-39`): loop { if neither ctrl nor alt down -> True; if now >= deadline -> False; sleep(poll) }. Checks first, so returns immediately if already up. On False the caller MUST NOT send any synthetic keys.
- Used only by Ctrl+Alt+Alt and Alt+Ctrl+Ctrl actions (app.py:466, 538) and as the router's stale-state probe (app.py:677).

### 1.2 detector.py (`DoublePressDetector`, `hotkey/detector.py:1-33`)
State: `_is_down: bool`, `_last_press_time: Option<f64>`; `interval` fixed at construction.
- `on_key_down(ts) -> bool`:
  1. if `_is_down` -> return false (OS auto-repeat; nothing else changes).
  2. `_is_down = true`.
  3. if `_last_press_time` is set and `ts - last <= interval` (INCLUSIVE `<=`) -> `_last_press_time = None` (pair consumed), return **true**.
  4. else `_last_press_time = ts`; return false.
- `on_key_up(ts)`: `_is_down = false`. Does NOT touch `_last_press_time` (the window is measured down-to-down; hold duration is irrelevant, so a long-held first press still pairs if second down is within interval of the FIRST DOWN).
- `reset_pending()`: `_last_press_time = None`; leaves `_is_down` alone.
- Triple tap: tap1 arm, tap2 fires (consumed), tap3 arms a fresh window (needs tap4 to fire again).
- Timestamps = `time.monotonic()` taken in the hook callback (seconds). Defaults 0.2 s both keys; settings range 0.1..2.0.
- IMPORTANT: no "other key resets" rule exists. The hook forwards ONLY Alt and Ctrl events (see 1.7); typing a letter between two Alt taps does NOT reset the detector. The only resets are the opposite modifier's transitions (1.4).

### 1.3 guard.py / manager.py / pausable.py
- `OperationGuard` (`hotkey/guard.py:4-25`): mutex-protected bool. `try_acquire()` non-blocking: false if running; `release()` clears.
- `HotkeyActionRunner` (`hotkey/manager.py:7-48`): `handle_key_down(ts)`: `if !detector.on_key_down(ts) return; if !guard.try_acquire() return (trigger dropped, not queued); executor(task)` where task = `try { action() } finally { guard.release() }`. `handle_key_up(ts)` -> detector.on_key_up. `reset_pending()` -> detector.reset_pending. The action NEVER runs inline on the hook thread; executor = `threading.Thread(target=task, daemon=True).start()` (app.py:93-94).
- Pause (`hotkey/pausable.py:4-17`): wrapper `if is_paused() return; action()`. Evaluated on the action thread at run time (guard is acquired and released around the no-op). While paused: detection, router, F15 injection all still run (only the action is skipped). `is_paused = tray_app.paused` (app.py:644+). Pause state is in-memory only, resets to false each launch.
- Guard topology (app.py:635-668): ONE guard shared by {alt plain runner, alt chord runner} and ONE shared by {ctrl plain, ctrl chord}. Alt-family and Ctrl-family guards are independent -> Alt+Alt and Ctrl+Ctrl actions CAN run concurrently (this is why Sheets clipboard use is serialized by a process-wide re-entrant lock, `sheets/clipboard_guard.py:36-47`). Alt translate vs Alt correction never overlap; Ctrl explain vs Ctrl correction-view never overlap.
- 4 independent `DoublePressDetector`s (alt plain, alt chord, ctrl plain, ctrl chord); alt ones use `alt_alt_interval_seconds`, ctrl ones `ctrl_ctrl_interval_seconds`. Intervals are read ONCE at startup (app.py:639-640); saving Settings does NOT change them until restart.

### 1.4 chord_router.py (`ChordHotkeyRouter`, `hotkey/chord_router.py:10-81`)
Mapping: runners `plain[alt]=alt_runner (translate+replace, Alt+Alt)`, `plain[ctrl]=ctrl_runner (translate+explain window, Ctrl+Ctrl)`, `chord[alt]=alt_chord_runner (Ctrl held + Alt+Alt = correction REPLACE)`, `chord[ctrl]=ctrl_chord_runner (Alt held + Ctrl+Ctrl = correction VIEW)`. Family of key K = {plain[K], chord[K]}.
State: `active: map<key, runner that received its down>` (present while physically held, as far as the router knows). Ignores any key other than "alt"/"ctrl".

`on_key_event(key, direction, ts)`:
- DOWN:
  1. if `key in active` -> return (auto-repeat of held key).
  2. `other` = opposite modifier.
  3. `_drop_if_stale(other)`: if `other in active`: probe `is_physically_down(other)` (exceptions -> keep recorded state); if probe says UP -> `active.pop(other).handle_key_up(ts)` (synthetic up so its detector does not treat the next real press as auto-repeat).
  4. `_reset_family(other)`: `reset_pending()` on BOTH runners of the OTHER key (D7: other modifier must be unchanged across taps; any other-key down invalidates a half-done double press of the other key).
  5. runner = `chord[key]` if `other in active` else `plain[key]` (chosen at key-down time).
  6. `active[key] = runner; runner.handle_key_down(ts)`.
- UP: `runner = active.pop(key)`; if present: `_reset_family(other)` then `runner.handle_key_up(ts)`. (An up for a key not in active is ignored.)
Consequences (D7 cancellation rules):
  - Ctrl pressed or released between the two Alt taps -> Alt's pending cancelled (and vice versa). E.g. `Alt, Ctrl down, Ctrl up, Alt` = no trigger.
  - Ctrl held throughout `Alt Alt` -> `alt_chord_runner` fires (correction replace); the plain Alt runner never sees these taps. If Ctrl is held for only the second tap, Ctrl-down reset both Alt runners -> nothing fires.
  - Alt held throughout `Ctrl Ctrl` -> `ctrl_chord_runner` (correction view).
  - A key's own transitions never reset its own family (only the other's do).
  - Stale "other" (its key-up missed: secure desktop, elevated window, hook started while held) is detected only at the NEXT down of the opposite key, via GetAsyncKeyState.
- Router holds no timers/threads. It runs synchronously on the hook thread inside the hook callback (so keep it fast; actions are spawned to threads).

### 1.5 Action triggering summary
Trigger fires on the SECOND key-DOWN (not release): user's fingers still hold the key -> that is why actions wait for release before synthesizing keys (see 2.5 and 3.x).

### 1.6 Injected-event filtering
`LLKHF_INJECTED = 0x10` on `KBDLLHOOKSTRUCT.flags` (`hotkey/windows_keyboard_hook.py:107,266`): injected events (app's own Ctrl+C/A/V via keybd_event and the F15) are passed through to `CallNextHookEx` but never reach the router/F15 logic. Without this a translate would feed its own synthetic Ctrls into the Ctrl+Ctrl detector.

### 1.7 windows_keyboard_hook.py (`WindowsKeyboardHook`)
- Hook: `SetWindowsHookExW(WH_KEYBOARD_LL=13, cb, NULL, 0)` on a dedicated daemon thread that runs a `GetMessageW/TranslateMessage/DispatchMessageW` loop (hook silently dies without its own message loop; separate from the UI loop) (`:77-79, 246-260`). `start()` waits for a ready event up to 5 s (hook-install failure is NOT detected: NULL handle ignored); `stop()` posts `WM_QUIT (0x0012)` via `PostThreadMessageW(thread_id,...)` and joins 5 s; handle unhooked in `finally` (`:230-260`). Callback object kept alive for hook lifetime.
- The hook NEVER blocks a real key: always returns `CallNextHookEx(NULL, code, wparam, lparam)` (`:301`). (An earlier version blocked Alt-up -> apps thought Alt stuck. Never block.) Note: Python code does not catch exceptions from `on_key_event` before CallNextHookEx; Rust port must catch panics and always call next hook.
- Message sets: down = `WM_KEYDOWN 0x100 | WM_SYSKEYDOWN 0x104`; up = `WM_KEYUP 0x101 | WM_SYSKEYUP 0x105`. `code >= 0` only (else just CallNextHookEx).
- Classification (`:307-312`): VK_LMENU `0xA4`/VK_RMENU `0xA5` -> "alt"; VK_LCONTROL `0xA2`/VK_RCONTROL `0xA3` -> "ctrl"; everything else -> None (never forwarded to the router). Left+Right sides are one logical key; no side tracking (so holding both sides then releasing one emits an "up").
- Forwarding: for non-injected events with a logical key: `on_key_event(key, "down"|"up", time.monotonic())`. Every down event incl. auto-repeat is forwarded (router/detector filter repeats).
- F15 solo-Alt-tap suppression (prevents Chrome stealing focus to its toolbar "Customize and control" button on a solo Alt tap; that steal broke capture):
  - State: `_alt_is_down`, `_alt_combo_used`.
  - On any non-injected key DOWN where `logical != alt` and `_alt_is_down` -> `_alt_combo_used = true` (this includes unclassified keys, Tab/F4/numpad, AND Ctrl).
  - On alt DOWN when `!_alt_is_down`: `_alt_is_down = true; _alt_combo_used = false`.
  - On alt UP (any non-injected alt up, even if `_alt_is_down` was already false): `_alt_is_down = false`; if `!_alt_combo_used`: `SendInput(VK_F15 0x7E key-DOWN)` NOW (before passing the real Alt-up down the chain) and set `release_f15_after`; else log skip.
  - After `CallNextHookEx` returns: if `release_f15_after`, spawn a daemon thread: sleep **0.05 s** (`_F15_RELEASE_DELAY_SECONDS`) then `SendInput(VK_F15, KEYEVENTF_KEYUP 0x0002)`. F15 is therefore HELD across the real Alt-up (app sees `Alt down ... F15 down, Alt up, (50ms) F15 up`); thread keeps the hook callback fast (LL hook is disabled by Windows if callback blocks ~300 ms).
  - Consequences: Alt-held Ctrl+Ctrl sets combo_used (no F15 on that Alt-up). Ctrl-held Alt+Alt: Ctrl went down BEFORE Alt so combo_used stays false -> F15 injected on each Alt up (Ctrl+F15, harmless).
- SendInput details (`:121-196`): `INPUT{type=INPUT_KEYBOARD(1), union{MOUSEINPUT, KEYBDINPUT}}`; `cbSize` MUST be the real `sizeof(INPUT)` = **40 bytes on x64** (union sized by MOUSEINPUT; an 8-byte-short struct made every SendInput fail with ERROR_INVALID_PARAMETER(87) for months, silently). `wVk=VK_F15, wScan=0, dwFlags=0|KEYUP, time=0, dwExtraInfo=NULL`. Check return value == 1; on failure log warning with vk/flags/return/GetLastError (call `SetLastError(0)` before).
- Real-machine lesson on ordering: F15 pressed BEFORE Alt-up (not at Alt-down; at Alt-down it lands ahead of Alt in the queue and does nothing), and held (not down+up pair) across Alt-up. Status per `git log`: spike S1..S5 confirmed in the Rust spike (this repo); treat these rules as the validated design.

---

## 2. interaction/

### 2.1 base.py (types, `interaction/base.py`)
- `FocusedElement` (frozen, `:6-23`): `app_hint: str` (the Chrome omnibox text / URL, NOT guaranteed to include scheme), `role: str`, `name: str`, `full_text: str`, `selection_text: Option<str>`, `supports_direct_text_replacement: bool` (always false in the real backend), `editor_full_text: Option<str>=None`, `editor_has_rich_text: Option<bool>=None` (Sheets cell editor only; None = unknown).
- `EditingContext` (frozen, `:26-31`): `app: str, field_id: str, has_selection: bool, text: str`. Equality is by value (all 4 fields) - Sheets re-capture check relies on that.
- `CapturedText` (`:35-37`): `context, text` (text == context.text).
- `ReplaceOutcome` (`:40-43`): `success: bool, error: Option<str>=None`.
- `UIABackend` protocol (`:46-97`): `get_focused_element()`, `foreground_is_docs_or_slides()`, `get_cursor_position()`, `set_selection_text`, `set_full_text` (unused: raise NotImplemented), `paste_clipboard_into_focus()` (plain Ctrl+V over existing selection), `paste_replacing_full_text_into_focus()` (re-Ctrl+A immediately before paste; must not assume capture-time select-all survived).
- `TextInteraction`: `capture() -> Option<CapturedText>`, `replace(captured, new_text) -> ReplaceOutcome`.
- Hard rule (docs in base.py:77-95, chrome.py:12-17): NEVER synthesize Enter / Ctrl+Enter / Send. Only Ctrl+C, Ctrl+A, Ctrl+V, Esc exist in the code.

### 2.2 clipboard.py (`ClipboardFallback`, `interaction/clipboard.py`)
Text-only preserve/restore used around the paste:
- `run_with_temporary_content(text, action)`: `previous = get_text()` (CF_UNICODETEXT or None; any exception -> None); `set_text(text)` with up to **3** attempts, no delay between (re-raise last error -> becomes `ReplaceOutcome(false, str(err))`); `try { return action() } finally { if previous is not None: set_text(previous) (failure only logged) }`. If previous was None (empty / non-text clipboard) nothing is restored: the new text stays on the clipboard.
- `Win32ClipboardBackend` (`interaction/windows_uia_backend.py:119-139`): `get_text`: OpenClipboard (single attempt, no retry), IsClipboardFormatAvailable(CF_UNICODETEXT) else None, GetClipboardData; `set_text`: OpenClipboard, EmptyClipboard, SetClipboardData(CF_UNICODETEXT). (EmptyClipboard destroys every other format: images/HTML/files are lost on the text-only path.)
- Restore happens right after the paste action returns; correctness depends on the paste having been consumed first (see 2.5 note on the implicit 0.5 s).

### 2.3 win32_multiformat_clipboard.py (`Win32MultiFormatClipboard`; used only by the Sheets cell-editor copy) 
- `_open()`: `OpenClipboard` up to **5** attempts, **0.02 s** between; then raise "could not open the clipboard" (`:20-21,37-49`).
- `sequence_number()` = `GetClipboardSequenceNumber()`.
- `snapshot()`: open; `EnumClipboardFormats` loop; skip handle formats `{2,3,9,14,0x80,0x81,0x82,0x83,0x8E}` (bitmap, metafile, palette, enhmetafile, owner-display, DSP*); `GetClipboardData(fmt)` failure -> skip format; str -> encode `utf-16-le + 00 00`; keep only bytes; (CF_HDROP etc. returning non-bytes are dropped -> file copies are not preserved). Close in finally.
- `restore(snapshot)`: open; EmptyClipboard; for each (fmt,data): CF_UNICODETEXT -> decode utf-16-le and strip trailing NULs then set; others set bytes; close in finally.
- Readers: `read_html` (registered format "HTML Format", decoded utf-8 errors=replace), `read_text` (CF_UNICODETEXT), `read_custom_data`; `write_html_and_text` (Empty, HTML bytes utf-8, optional custom format, optional CF_UNICODETEXT) - these belong to Sheets internals.
- "User copied meanwhile" rule = `ClipboardGuard` (`sheets/clipboard_guard.py:79-112`): `begin()`: snapshot + `begin_seq = own_seq = seq`. `mark_own_write()`: `own_seq = seq` right after our own copy. `finish()` (never raises; returns bool): if no snapshot -> false; clear snapshot; if `seq != own_seq` -> someone else touched the clipboard -> DO NOT restore, return false; if `own_seq == begin_seq` (we never wrote) -> true, no restore; else `restore(snapshot)` (errors logged, false).
- `wait_for_clipboard_change(before, timeout, stable=0.05, poll=0.02)` (`:50-76`): returns true once seq != before AND has been unchanged for >= `stable`; false on deadline. Always bounded.
- `clipboard_section()` = process-wide re-entrant mutex around Sheets copy/paste/restore sequences (Alt and Ctrl families run on different threads). The plain Chrome text path (2.5) does NOT take it.

### 2.4 retrying.py (`RetryingTextInteraction`, `interaction/retrying.py:144-190`)
- Outermost wrapper for both adapters. `capture()`: up to **5** attempts, each a fresh inner capture; return first non-None; sleep **0.08 s** between attempts (none after the last); all None -> None. Reason: UIA/Chrome needs to settle after the hotkey burst. Never logs captured text (only app/field).
- `replace()` is NOT retried (pass-through).

### 2.5 chrome.py (`ChromeAdapter`) + strategies.py + backend
`ChromeAdapter(uia_backend, strategies, clipboard_fallback)`:
- `capture()` (`interaction/chrome.py:35-48`): `focused = backend.get_focused_element()`; None -> None. FIRST strategy whose `matches(focused)` wins (list order); none -> None. `ctx = strategy.identify_context(focused)`; None -> None (does NOT fall through to the next strategy). Else `CapturedText(ctx, ctx.text)`.
- `replace(captured, new_text)` (`:50-89`): re-runs get_focused_element (a FULL probe again, including Ctrl+C / Ctrl+A+Ctrl+C key sequences) and re-validates BEFORE touching anything. Failure outcomes (exact strings, `ReplaceOutcome(false, err)`):
  - focused None: `Nothing is focused anymore; aborting to protect original text`
  - no strategy matches: `Focus moved to an unsupported app; aborting replace`
  - context None or `app`/`field_id` differ from captured: `Editing context changed since capture; aborting replace to protect original text` (only app + field_id are compared, NOT text/has_selection)
  Then: if `!focused.supports_direct_text_replacement` (always, in prod): `paste_action = captured.context.has_selection ? paste_clipboard_into_focus : paste_replacing_full_text_into_focus`; `clipboard_fallback.run_with_temporary_content(new_text, paste_action)`. (The direct set_selection_text/set_full_text branches are dead in prod.) Any exception -> `ReplaceOutcome(false, str(exc))`. Success -> `ReplaceOutcome(true)`.
  These error strings are NEVER shown to the user for non-Sheets apps (app.py:447-452 only maps Sheets codes) - only logged.

Wrapper stacks built in app.py:
- Alt+Alt / Ctrl+Alt+Alt: `Retrying( SheetsEditSafe( ChromeAdapter[GoogleChat, Gmail, GoogleDocs, GoogleSlides, GoogleSheetsCellEdit(allow_rich_text=false)] ), allow_full_cell_replace=cfg.sheets.edit_full_cell_replace_enabled )` (app.py:323-339). Strategy ORDER matters.
- Ctrl+Ctrl / Alt+Ctrl+Ctrl (read-only): `Retrying( ChromeAdapter[GoogleChatReadOnlySelection, GmailReadOnlySelection, GoogleDocs, GoogleSlides, GoogleSheetsCellEdit(allow_rich_text=true)] )` (app.py:344-356). (`allow_rich_text` is stored but never consulted.)

#### 2.5.1 Strategies (`interaction/strategies.py`)
Helper `_selection_or_full(f)`: `if f.selection_text (truthy; non-empty, incl. whitespace-only) -> (true, selection_text) else (false, full_text)`.
Gmail-field-role set `{subject, body, cc, bcc, to}` (`:116`).

| Strategy | `app_name` | matches(focused) | identify_context -> EditingContext (field_id) |
|---|---|---|---|
| GoogleChat (replace) `:131-146` | `google_chat` | `is_google_chat_url(url) && role not in GMAIL_ROLES` | role must be `compose` else None; `field_id="compose"`, selection or full text |
| Gmail `:149-165` | `gmail` | `"mail.google.com" in url` | role must be `subject` or `body` (to/cc/bcc/anything else -> None); `field_id=role`; selection or full text |
| GoogleChatReadOnlySelection `:168-186` | `google_chat` | same as GoogleChat | needs `selection_text` truthy else None; `field_id="selection"`, has_selection=true (ANY role, incl. history) |
| GmailReadOnlySelection `:189-206` | `gmail` | same as Gmail | selection required; `field_id="selection"` (any role, incl. received mail) |
| GoogleDocs `:209-225` | `google_docs` | `"docs.google.com/document" in url` | selection required (canvas; no safe no-selection path); `field_id="selection"` |
| GoogleSlides `:228-277` | `google_slides` | `"docs.google.com/presentation" in url` | if selection -> (`field_id="textbox"`, has_selection=true, selection text); else if `role in (textbox, slide_canvas) && full_text` non-empty -> (`"textbox"`, has_selection=false, full_text); else None. field_id MUST be `"textbox"` in both branches (else replace re-validation sees a changed field after translation latency) |
| GoogleSheetsCellEdit `:283-317` | `google_sheets` | `is_google_sheets_url(url) && role in {sheets_cell_editor, sheets_formula_bar}` | `editor_full_text` None -> None; `full.lstrip().startswith("=")` -> None (formula, even when only a substring selected); selection truthy -> (`"sheets_cell_edit"`, true, selection); `full.strip()==""` -> None; else (`"sheets_cell_edit"`, false, full) |

URL predicates (`interaction/windows_role_classifier.py:18-53`; plain substring tests on the omnibox text): `is_google_chat_url = "chat.google.com" in url or ("mail.google.com" in url and "#chat" in url)`; `is_docs_or_slides_url = "docs.google.com/document" in url or "docs.google.com/presentation" in url`; `is_google_sheets_url = "docs.google.com/spreadsheets" in url`. No scheme/host parsing. Do NOT detect Gmail compose-overlay by `compose=` in URL (Chat's own compose also carries it).

#### 2.5.2 Role classifier (`classify_focused_role(url, name, control_type="", *, automation_id="")`, `:56-148`) - evaluated top-down, first hit wins
- `lname = name.lower()`. 
- if `"mail.google.com" in url` (checked BEFORE the chat rule so a Gmail compose overlay under a stale `#chat` hash is Gmail):
  - `"subject" in lname or "제목" in name` -> `subject`
  - `"message body" in lname or lname == "body" or "본문" in name` -> `body`
  - `("bcc" in lname and "recipient" in lname) or "숨은참조" in name` -> `bcc`
  - `("cc" in lname and "recipient" in lname) or ("참조" in name and "숨은참조" not in name)` -> `cc`
  - `("to" in lname and "recipient" in lname) or "받는사람" in name` -> `to`
- if `is_google_chat_url(url)`: `control_type == "EditControl" or ("message" in lname and "histor" not in lname)` -> `compose` else `message_history` (control type is the locale-independent primary signal).
- if `"mail.google.com" in url` -> `unknown`.
- if `"docs.google.com/document" in url`: `ButtonControl` -> `unknown` else `canvas`.
- if `"docs.google.com/presentation" in url`: `ButtonControl` -> `unknown`; `"text box" in lname or "shape" in lname` -> `textbox`; else `slide_canvas`.
- Sheets (summary only): `ComboBoxControl` with `automation_id == "waffle-rich-text-editor"`: empty name -> `sheets_grid`; name fullmatch `[A-Z]+[1-9][0-9]*` -> `sheets_cell_editor`; else `unknown`; other combo -> `unknown`. `ButtonControl` -> `unknown`. `EditControl`: name box (`automation_id=="t-name-box"` or "name box"/"이름 상자") -> `sheets_name_box`; "formula"/"수식" -> `sheets_formula_bar`; contains any hint in `("address","search","find","omnibox","rename","title","comment","filter","주소","검색","찾기","제목","댓글","필터","이름 바꾸기")` -> `unknown`; else `sheets_cell_editor`. Types in `("", PaneControl, DocumentControl, GroupControl, CustomControl)` -> `sheets_grid`; else `unknown`.
- else `unknown`.

### 2.6 windows_uia_backend.py (`WindowsUiaBackend`) - step by step
Constants (`:69-86,107-116`): `_CHROME_CLASS_PREFIX="Chrome_WidgetWin"`, `_CHROME_RENDER_WIDGET_CLASS="Chrome_RenderWidgetHostHWND"`, `_RENDER_WIDGET_SEARCH_DEPTH=10`, omnibox class `OmniboxViewViews` depth 20, `_POST_KEY_DELAY=0.05`, `_SELECT_ALL_SETTLE_DELAY=0.2`, `_ESCAPE_RECOVERY_DELAY=0.1`, `_ALT_RELEASE_POLL=0.02`, `_ALT_RELEASE_TIMEOUT=0.5`, `_SHEETS_COPY_TIMEOUT=0.3`, `_SHEETS_COPY_POLL=0.02`, sentinel `"\0claudelangtrans_no_selection_probe\0"`, `_SUPPORTED_APP_URL_HINTS=("mail.google.com","docs.google.com")`.

COM: every public entry (`get_focused_element`, `foreground_is_docs_or_slides`, `resolve_focus`, Sheets `current_url_and_state`) calls `_ensure_com_initialized_for_this_thread()` -> `InitializeUIAutomationInCurrentThread()` once per thread (thread-local flag) (`:50-67`). A thread that never called CoInitializeEx cannot use UIA objects even if another thread did; action threads are fresh each trigger. UIA elements must not cross threads. `get_cursor_position` uses no COM.

UIA surface actually used (NO TextPattern; NO text read through UIA for Chrome content):
- `GetForegroundControl()`, `.ClassName` (must start with `Chrome_WidgetWin` -> Chrome/Edge), 
- omnibox: `window.EditControl(ClassName="OmniboxViewViews", searchDepth=20)`; `Exists(0,0)`; `GetValuePattern().Value` (fallback `.Name`) else `""`; any exception -> `""`. The omnibox text is the URL source (may lack `https://`); empty when omnibox hidden/fullscreen -> no strategy matches.
- `GetFocusedControl()`, `.Name`, `.ControlTypeName` (e.g. `ButtonControl`, `EditControl`), `.AutomationId`.
- `window.PaneControl(ClassName="Chrome_RenderWidgetHostHWND", searchDepth=10)`; `Exists(0,0)`; `.SetFocus()` (UIA SetFocus; sends no key).
- Sheets editor only: `control.GetValuePattern().Value` (strip ONE trailing `\n`).
All text content comes via clipboard round-trip; `supports_direct_text_replacement` is always false.

`_wait_until_alt_released(timeout=0.5)`: `while GetAsyncKeyState(VK_MENU)&0x8000 and now<deadline: sleep(0.02)`. Returns nothing: after the timeout the key is sent ANYWAY (unlike wait_for_modifiers_released). Reason: synthetic Esc/key while Alt is down becomes Alt+Esc = window switch (observed focus theft).

`get_focused_element()`:
1. COM init. `_resolve_focused_control()`:
   a. foreground None or class not `Chrome_WidgetWin*` -> return None (capture fails).
   b. `_wait_until_alt_released()`.
   c. `url = omnibox text`; `ctrl = GetFocusedControl()`; None -> `(url, None, "", "")` -> element None.
   d. `name, control_type`, `role = classify(url, name, control_type)`.
   e. Stray toolbar focus recovery: only if `control_type == "ButtonControl"` AND `role in (message_history, unknown)` AND (chat url OR url contains `mail.google.com`/`docs.google.com`) (narrow on purpose; a click in chat history must keep failing safely). Then: if `"docs.google.com" in url` try `_try_focus_docs_content` (UIA SetFocus on render-widget pane; success -> NO Esc, because Esc clears the Docs/Slides text selection). If not recovered (Chat/Gmail, or pane not found): `_wait_until_alt_released(); SendKeys("{Esc}"); sleep(0.1)`. Then re-`GetFocusedControl()`; if non-None re-read name/control_type and re-classify (without automation_id).
   f. If Sheets URL and control non-None: re-classify with `automation_id=control.AutomationId`.
   g. return `(url, ctrl, name, role)`; if ctrl None -> element None.
2. If `role in (sheets_cell_editor, sheets_formula_bar)` -> `_sheets_editor_element` (below) and return.
3. Selection probe `_capture_selection_via_clipboard()` (`:431-460`):
   - `previous = get_text()` (exception -> None); `set_text(SENTINEL)`; `SendKeys("{Ctrl}c")`; `sleep(0.05)`; `after = get_text()`; any exception in that try -> `after=None`; `finally: if previous is not None: set_text(previous)` (failure logged).
   - result: `after` if truthy and `!= SENTINEL` else None. (Sentinel, not before/after diff: copying text identical to the existing clipboard must still count.) Empty-string selection = None. If `previous` was None the copied text/sentinel stays on the clipboard.
4. No-selection fallback `_capture_full_text_via_select_all_and_clipboard()` (`:462-485`): runs only if `selection_text is None AND role in (compose, subject, body, textbox, slide_canvas)` (NOT Docs `canvas`, NOT sheets): `previous = get_text()`; `SendKeys("{Ctrl}a")`; `sleep(0.2)` (multi-paragraph fields need settle time; 0.05 only copied the first paragraph); `SendKeys("{Ctrl}c")`; `sleep(0.05)`; `full = get_text() or ""` (exception -> ""); `finally restore previous text`. Leaves the field selected afterward.
   LATENT QUIRK: unlike the selection probe, NO sentinel is seeded here, so if Ctrl+C copies nothing (empty field / bare Slides canvas whose Ctrl+A copies nothing) `get_text()` returns the user's PRE-EXISTING clipboard text as `full_text`. The Rust port should seed a sentinel (and treat "unchanged" as empty) rather than copy this.
5. Return `FocusedElement(app_hint=url, role, name, full_text, selection_text, supports_direct=false)`.

`foreground_is_docs_or_slides()` (`:165-173`): COM init; foreground Chrome? `is_docs_or_slides_url(omnibox url)`; no clipboard/selection work.
`get_cursor_position()` (`:175-193`): raw `GetCursorPos` (physical px when process is DPI aware; None on failure); no UIA, no Alt wait, no recovery. (Using the focused control's bounding rect was abandoned: Docs/Slides expose one giant canvas element; badge landed by the address bar.)
Paste primitives:
- `paste_clipboard_into_focus`: `_wait_until_alt_released(); SendKeys("{Ctrl}v")`.
- `paste_replacing_full_text_into_focus`: `_wait_until_alt_released(); SendKeys("{Ctrl}a"); sleep(0.2); SendKeys("{Ctrl}v")` (re-select; the capture-time selection may have collapsed during translation).

IMPLICIT TIMING (library default, not in app code): `uiautomation.SendKeys(text, interval=0.01, waitTime=OPERATION_WAIT_TIME=0.5)`. Each `SendKeys("{Ctrl}c")` = Ctrl down (VK_CONTROL with scancode, via legacy `keybd_event`, hence LLKHF_INJECTED), 10 ms, key down/up, 10 ms, Ctrl up, then **sleeps 0.5 s** at the end. So each key chord costs ~0.5 s before the explicit 0.05/0.2 waits; the paste is therefore consumed by Chrome before ClipboardFallback restores the old clipboard. A Rust port that sends keys faster MUST add equivalent settle waits (paste-consumed delay before clipboard restore; copy-complete wait). Worst-case timings: selection-only capture ~0.6 s; no-selection capture ~0.6+0.7+0.55 s; replace = another full probe + paste (+0.7 s for full-text). Capture is re-run (full probe) in `replace()` re-validation.

Sheets editor element (`_sheets_editor_element`, `:319-376`; brief): inside `clipboard_section()`: `ClipboardGuard(multi_clipboard)` begin -> `seq_before` -> `SendKeys("{Ctrl}c")` -> `wait_for_clipboard_change(0.3, poll 0.02)`; unchanged -> `(None, None)`; else `mark_own_write`, read text + HTML; then read editor value via ValuePattern; `html_rich_text_state(html)`; `finish()` always restores user's clipboard (unless user copied meanwhile). Returns `FocusedElement(full_text="", selection_text, editor_full_text, editor_has_rich_text, supports_direct=false)`. `sheets_select_all_enabled` ctor arg is stored, unused here.

### 2.7 sheets_edit.py (`SheetsEditSafeInteraction`, `interaction/sheets_edit.py`) - plug-in only
Wraps Alt+Alt adapter. `capture()` pass-through. `replace()` for `context.app == "google_sheets"` only: (1) `new_text.lstrip()` starts with `=` or `+` -> `ReplaceOutcome(false,"translation_starts_with_formula_prefix")`; (2) no selection and `!allow_full_cell_replace` -> `"full_cell_replace_not_enabled"`; (3) re-`capture()` via inner; None or `context != captured.context` -> `"target_changed"`; then delegate. Other apps / selections delegated untouched. Error codes are mapped to user text only in app.py (5.3).

---

## 3. Orchestration (replace/, explain/, correction, ctrl_ctrl_controller)

### 3.1 replace/service.py `perform_alt_alt_translate` (`replace/service.py:15-66`)
1. `captured = text_interaction.capture()` (Retrying-wrapped). None -> return sentinel `CAPTURE_FAILED`.
2. `translation_service.translate(captured.text, base, target, tone)`; `!success or translated_text is None` -> return `None` (translation failure: original untouched; NO user message anywhere).
3. `text_interaction.replace(captured, translated)` -> return `ReplaceOutcome`.
Only durations are logged (never text).

### 3.2 replace/correction.py `perform_correction_replace` (`:88-133`)
`CorrectionReplaceResult{capture_failed, outcome: Option<CorrectionOutcome>, replace: Option<ReplaceOutcome>}`.
1. capture None -> `capture_failed=true`.
2. `correction_service.correct(captured.text, base, mode)`; `status != SUCCESS or corrected_text None` -> `(capture_failed=false, outcome)`; replace NEVER called (UNCHANGED, NOT_KOREAN, LLM_FAILED, INTEGRITY_FAILED...).
3. else `replace(captured, corrected_text)` -> `(false, outcome, replace)`.

### 3.3 explain/service.py (controller usage)
`explain_captured_text(captured, dict_entries, detector, llm_provider, base, target, tone)` (`explain/service.py:190-232`): `mask` -> `detector.detect(masked)` -> `resolve_translation_direction`; no direction -> `None`; `llm_provider.explain(...)` -> `restore_sentence_pairs`; `PlaceholderIntegrityError` -> sentinel `DICTIONARY_INTEGRITY_FAILED`; `LLMProviderError` -> `None`. Split out from `perform_ctrl_ctrl_explain` so the controller can CAPTURE FIRST and show UI only afterward (showing/raising the loading window before capture steals focus from Chrome; capture then reads our own popup and fails). `DICTIONARY_INTEGRITY_MESSAGE = "사전 용어 처리에 실패했습니다. 다시 시도해주세요."` (`:158`). Display of result sentences; `replace()` is never reachable from this path.

### 3.4 ui/ctrl_ctrl_controller.py (`CtrlCtrlController`, QObject on the UI thread)
Two queued signals marshal to the UI thread: `_loading_requested(str)` -> `_show_loading(msg)`: `window.show_loading(msg); window.bring_to_front()`; `_result_ready(object)` -> `_show_result(result)`. `run*` methods execute on the action thread.
- State updated by app per trigger: `update_languages`, `update_display_mode`, `update_tone`, `update_dictionary_entries`, `update_llm_provider` (`:72-95`).
- `run()` (Ctrl+Ctrl translate, non-Sheets) (`:97-122`): capture -> None: emit `_NO_SELECTION` and return. Else emit loading `"Translating..."`; `explain_captured_text(...)` inside try (any exception -> log + result None); emit result.
- `run_correction(correct_fn, loading_message)` (`:124-141`): capture -> None: emit `_NO_SELECTION_CORRECTION`. Else emit loading(loading_message); `correct_fn(captured.text)`; exception -> `CorrectionOutcome(LLM_FAILED)`; emit outcome.
- `run_sheets(capture, translate, failure_status=TRANSLATION_FAILED, loading_message="Translating...")` (`:143-171`): same order (capture first; capture exception -> `SheetsOutcome(CAPTURE_FAILED)`; a `SheetsOutcome` from capture -> emit it, no loading; else loading, `translate(captured)`; exception -> `SheetsOutcome(failure_status)`).
- `show_correction_no_selection()` (`:173-177`): emits `_NO_SELECTION_CORRECTION` (used when modifiers still held).
- `_show_result(result)` dispatch order (`:183-210`) with EXACT strings (all errors are rendered by `window.show_error(m)` => status label `"Error: " + m`, followed by `bring_to_front()` unless noted):
  1. `CorrectionOutcome`: `SUCCESS` and `sentences` present -> `display_result(sentences, display_mode)` (NO bring_to_front); else `show_error(outcome.user_message())`.
  2. `_NO_SELECTION_CORRECTION` -> `텍스트를 선택한 후 Alt+Ctrl+Ctrl을 실행해주세요.`
  3. `SheetsOutcome` -> `show_error(outcome.user_message())` (see 3.5)
  4. `SheetsTableResult` -> `display_sheet_table(result)` (no bring_to_front)
  5. `_NO_SELECTION` -> `텍스트를 선택한 후 Ctrl+Ctrl을 실행해주세요.` (bring_to_front is essential: loading never ran so window may be hidden)
  6. `DICTIONARY_INTEGRITY_FAILED` -> `사전 용어 처리에 실패했습니다. 다시 시도해주세요.`
  7. `None` -> `Could not translate/explain the selected text`
  8. else (ExplanationResult) -> `display_result(result, display_mode)` (no bring_to_front: window was raised by the loading step; `display_result` does not call show(), so if the user closed the window during loading, the result is not visible).
- Loading messages: `"Translating..."`; correction view uses `correction_loading_message(mode)` = `한국어 교정 중...` (spelling) or `한국어 수정 중...` (polish) (`correction/modes.py:153-170`).

### 3.5 User-facing strings table (non-UI-widget)
CorrectionOutcome `user_message()` (`correction/service.py:30-60`): UNCHANGED `수정할 부분이 없습니다.`; BASE_NOT_KOREAN `한글 교정은 Base Language가 ko(한국어)일 때만 사용할 수 있습니다. 설정에서 Base Language를 ko로 바꿔주세요.`; NOT_KOREAN `선택한 문장의 주 언어가 한국어가 아니어서 교정하지 않았습니다.`; LLM_FAILED `교정에 실패하여 원본을 변경하지 않았습니다.`; INTEGRITY_FAILED = dictionary integrity text above; SUCCESS `""`.
SheetsOutcome (`sheets/outcome.py:24-66`) - only reached via routing: UNSUPPORTED_RANGE -> `이 선택 범위는 지원하지 않습니다: {detail}` (or `이 선택 범위는 지원하지 않습니다.` without detail) with details `선택 위치를 확인할 수 없습니다` | `떨어진 여러 범위는 선택할 수 없습니다` | `행·열 전체 선택은 지원하지 않습니다` | `병합된 셀이 포함되어 있습니다` | `숨겨진 행·열 또는 필터로 가려진 행이 포함되어 있습니다` | `글자별 서식이나 링크가 있는 셀이 포함되어 있습니다. 해당 셀을 제외하고 다시 선택해주세요` | `셀 유형을 확인할 수 없는 셀이 포함되어 있습니다` | `선택 범위가 너무 큽니다. 더 작은 범위를 선택해주세요`; NO_TEXT `번역할 텍스트가 없습니다.`; TRANSLATION_FAILED `번역에 실패하여 원본을 변경하지 않았습니다.` (with reason CELL_TOO_LARGE, also for CORRECTION_FAILED: `셀 내용이 너무 깁니다. 더 작은 텍스트를 선택해주세요.`); TARGET_CHANGED `번역 중 시트·선택 범위 또는 내용이 바뀌어 교체를 취소했습니다. 다시 실행해주세요.`; WRITE_FAILED `붙여넣기에 실패했습니다. 시트를 확인해주세요.`; WRITE_UNVERIFIED `교체 결과를 확인할 수 없습니다. 시트 내용을 직접 확인해주세요.`; CAPTURE_FAILED `선택한 셀을 읽을 수 없습니다.`; WRONG_INPUT_STATE `Sheets에서는 셀을 선택하거나 셀을 편집하는 중에 실행해주세요.`; NO_KOREAN_TEXT `선택한 셀에 한국어 텍스트가 없습니다.`; NO_CHANGES `수정할 부분이 없습니다.`; CORRECTION_FAILED `교정에 실패하여 원본을 변경하지 않았습니다.`; SUCCESS `""`.

---

## 4. ui/

### 4.1 theme.py (`ui/theme.py:32-47`) - fixed palette (ignores OS light/dark)
`background #FBEEEC`, `card_background #FFFFFF`, `divider #F0DEDC`, `text_primary #2B2B2B`, `text_secondary #8A8A8A`, `chip_background #F2E4E2`, `accent #2563EB`, `disabled_background #EFEFEF`, `disabled_text #B5B5B5`.

### 4.2 icon.py (`ui/icon.py`)
Placeholder tray icon: transparent square `size` (default 32), antialiased filled circle `#2563EB`, no pen, text `CT` white bold centered, font point size `max(1, int(size*0.4))`. Same drawing used by `packaging/generate_icon.py` for the .ico (35 lines; build-time).

### 4.3 tray_application.py (`ui/tray_application.py`)
- `QSystemTrayIcon`, icon = placeholder, tooltip `ClaudeLangTrans_v5` (`:427-429`). No left-click handler is defined; only the context menu.
- Menu, in order, no separators (`:437-461`):
  1. submenu `Target Language`: checkable exclusive group, items in order `en`, `vi`, `ja`, `ko` (NOTE: differs from Settings order), initial check = `settings.target_language`; click -> `settings_store.update(replace(current, target_language=lang))` and re-check group (does NOT call on_settings_saved/rebuild).
  2. `Personal Dictionary` -> opens `PersonalDictionaryDialog` (new instance per click, `WA_DeleteOnClose`; on `destroyed` re-merge company+team+user dictionaries and `translation_service.update_dictionary_entries(merged)`; also merged once at construction `:435`).
  3. `Settings` -> closes any previous dialog, creates a new `SettingsDialog(store, on_saved=app.on_settings_saved, model_catalog)`, `show()`.
  4. `Pause Translation` (checkable; text never changes; `triggered(checked)` sets `paused`). In-memory only.
  5. `Exit` -> `qt_app.quit`.
- Known staleness: tray target-language check marks are not refreshed when Settings changes target language (Rust port may improve).
- Tray shown via `tray_icon.show()` late in startup (5.1).

### 4.4 settings_dialog.py (`SettingsDialog`, QWidget top-level, title `ClaudeLangTrans - Settings`)
Window bg `#FBEEEC` on object `settingsRoot`. Layout: vertical stack of 5 white rounded cards (radius 14, margins 20/16/20/16, bold 14px title, 1px `#F0DEDC` dividers between rows) then the `Save` button. Row = bold label (+gray description) on the left, control on the right, row margins `(0,8,0,8)` (`:141-205,380-386`).
Cards and rows (labels verbatim):
1. Card `일반`:
   - `Base Language`: combobox `["ko","en","vi","ja"]`, current = settings.base_language.
   - `Target Language`: same list/order.
   - (no label) checkbox `Start ClaudeLangTrans when I log in` (custom-painted 16 px box, radius 3, gap 8, 2px accent check lines (3,8)-(6,12)-(13,3), because ancestor stylesheets kill the native indicator).
   - `Claude CLI 경로` + description `비워두면 자동 감지합니다.`: line edit prefilled with `claude_cli_path`; placeholder `자동 감지: {detected}` where detected = `resolve_claude_cli_path(setting)`; saved with `.strip()`.
   - `Claude Model`: combobox (see model logic); its description label is the fallback hint.
   - `Claude Effort`: combobox; description label is the unsupported hint.
   - `번역 어투`: combobox `[("일반 (기본값)","neutral"),("구어체","casual"),("정중하게 (회사 공식 메일 등)","formal"),("쉬운 단어와 문장","plain")]`; unknown stored value -> index 0.
2. Card `창 모드 번역 (Ctrl+Ctrl)`: `단축키 간격 (초)` = double spin box range 0.1..2.0 step 0.1 (Qt default 2 decimals), value `ctrl_ctrl_interval_seconds`; `표시 방식` combobox `[("원문 전체 · 번역 전체 보기 (기본값)","whole_text"),("문장별로 보기","per_sentence")]`.
3. Card `번역 후 교체 (Alt+Alt)`: `단축키 간격 (초)` spin 0.1..2.0 step 0.1, value `alt_alt_interval_seconds`.
4. Card `한글 교정 (Ctrl+Alt+Alt / Alt+Ctrl+Ctrl)`: `교정 범위` combobox `[("오탈자,띄어쓰기 교정","spelling"),("오탈자,띄어쓰기 교정,문장 다듬기","polish")]`; stored value normalized (`unknown -> spelling`).
5. Card `단축키 안내`: gray word-wrapped label, lines joined by `\n`, verbatim (`:84-91`):
   - `Alt+Alt — 번역 후 교체`
   - `Ctrl+Ctrl — 번역 결과를 창으로 보기 (원문 변경 없음)`
   - `Ctrl을 누른 채 Alt+Alt — 한글 교정 후 교체`
   - `Alt를 누른 채 Ctrl+Ctrl — 한글 교정 결과를 창으로 보기 (원문 변경 없음)`
   - `한글 교정은 Base Language가 ko이고 선택한 문장의 주 언어가 한국어일 때만 동작합니다.`
   - `Google Docs에서 Alt+Alt, Ctrl+Alt+Alt 기능을 사용하시려면 반드시 블록을 설정하셔야 합니다.`
Comboboxes are individually styled (white bg, `#2B2B2B` text, 1px `#F0DEDC` border, radius 6, padding 4/8; disabled bg `#EFEFEF` text `#B5B5B5`; popup list white with `chip_background` selection) because the popup does not inherit ancestor styles (`:114-132`).

Model/Effort logic (`:92-106,388-458`):
- Model list = `ModelCatalog.get()`; items: value `"default"` -> label `기본값 (자동)` with data `""`; others label `display_name`, data `value`. Saved model not in list -> append and select `(목록에 없음) {saved}` with data=saved (never silently swap).
- Effort combo rebuilt on every model change: first item `기본값 (자동)` (data `""`), then one item per level of the selected model: labels `low->Low, medium->Medium, high->High, xhigh->X-High, max->Max` (unknown level -> `level.capitalize()`). Levels for the model: lookup `"default"` if data is `""`; model not found -> `("low","medium","high","xhigh","max")`. Combo enabled iff levels non-empty; hint label `이 모델은 Effort를 지원하지 않습니다` shown iff disabled (e.g. haiku). Keeps previous effort if still offered else index 0 (`""`).
- Fallback hint label `기본 목록 표시 중 — Claude CLI에서 목록을 불러오면 자동으로 갱신됩니다` visible iff `catalog.source == "fallback"`.
- Haiku warning: signals connected AFTER initial population (no popup when loading a saved haiku or on catalog refresh). On every user-driven model index change: first re-populate effort, then if `model_combo.currentData() == "haiku"` -> `QMessageBox.warning(parent=dialog, title "모델 선택", text "길거나 복잡한 문장에서 성능이 저하될 수 있습니다.")` (modal OK).
- Catalog listener: catalog refreshes on a background thread -> signal -> UI thread `_on_catalog_changed`: keeps current model/effort selection, repopulates with signals blocked (no haiku popup), re-evaluates hint. Listener registered in ctor; a re-sync check runs if the catalog changed between read and registration; removed in `closeEvent`.
Save (`:460-478`): builds a full `UserSettings` from controls (`claude_model = model data`, `claude_effort = effort data`, `claude_cli_path` stripped, etc.), `store.update(...)`, calls `on_saved()` (app hot-swap, 5.5), then `close()`. No validation beyond control ranges. The dialog is not deleted on close (tray replaces it next open).

### 4.5 personal_dictionary_dialog.py (`PersonalDictionaryDialog`, title `ClaudeLangTrans - Personal Dictionary`)
- Widgets top to bottom: table (3 columns, headers `Term`, `Action`, `Translations`; rows = entries; Translations column = `", ".join(entry.translations.values())` in dict order), form rows `Term` (line edit) and `Action` (combobox items `KEEP`, `TRANSLATE`), four rows labeled `ko`, `en`, `vi`, `ja` (line edits; order ko,en,vi,ja), an error label (empty by default), buttons `Add`, `Update`, `Delete`.
- Row selection (`itemSelectionChanged`): none -> `_selected_term = None` (form NOT cleared); else look up entry by exact term text; fill Term, Action, and each language field (missing -> `""`).
- Form -> entry: `Action(combo text)`; translations = only non-empty language fields.
- Add: clear error; build entry and `store.add`; `ValueError`/`TypeError` -> error label = `str(exc)`; success -> refresh table. Update: if nothing selected -> `Select an entry to update first`; else `store.update(selected_term, entry)`; catches ValueError/TypeError/KeyError. Delete: nothing selected -> `Select an entry to delete first`; `store.delete`; KeyError -> label; success clears selection and refreshes.
- Store messages (from `dictionary/personal_store.py` / `model.py`): duplicate (case-insensitive) `Term '{term}' already exists in the personal dictionary` (ValueError); TRANSLATE with no translations `TRANSLATE entry for term '{term}' requires at least one translation`; not found `Term '{term}' not found in the personal dictionary` (KeyError, whose `str()` wraps the message in single quotes). No empty-term validation exists. Persisted immediately on each op; dialog does not clear the form after Add/Update/Delete.

### 4.6 explanation.py (`ExplanationWindow`, QWidget)
Behavior:
- Frameless (`FramelessWindowHint`), translucent window background (`WA_TranslucentBackground`); an inner panel `#explanationPanel` fills the window: bg `#FBEEEC`, radius 14. Title (not visible) `ClaudeLangTrans - Explanation`. Normal top-level (appears in taskbar). Closing (Esc, ✕) only hides; app keeps running (`setQuitOnLastWindowClosed(False)`).
- Panel layout: margins `(16,12,16,12)`, spacing 6, top->bottom: header row, status label (hidden when empty), table caption (Sheets, hidden), scroll area, footer (`:196-208`).
- Header: label `ClaudeLangTrans` (bold, `#8A8A8A`); stretch; flat 24x24 button `✕` (`#8A8A8A`, transparent, no border) -> `close()`. Footer: label `닫기 Esc` right-aligned `#8A8A8A`. Status label `color #2B2B2B`.
- Min size 480x240. Auto-size (only in `display_result`; loading/error keep the current size): `maxW = min(700, avail.width-40)`, `maxH = min(800, avail.height-80)` (primary screen availableGeometry); `width = clamp(content.sizeHint.w + 40, 480, maxW)`, `height = clamp(content.sizeHint.h + 80, 240, maxH)` -> `resize()`. Sheets table: `width = clamp(rowHeaderW + sum(colW) + 60, 480, maxW)`, `height = clamp(colHeaderH + sum(rowH) + 130, 240, maxH)`. Beyond max, the `QScrollArea` (widgetResizable, transparent, no border) scrolls vertically.
- `bring_to_front()` (`:226-245`): `show(); setWindowFlag(StaysOnTop,true); show(); raise_(); activateWindow(); setWindowFlag(StaysOnTop,false); show()`. Purpose: this is a background process so `SetForegroundWindow`-style activation is blocked by the foreground-lock policy (window would only flash in the taskbar); a TOPMOST z-order flip (SetWindowPos HWND_TOPMOST then back to non-topmost) reliably pops it to the front without leaving it always-on-top.
- Esc: `keyPressEvent` Key_Escape -> `close()` (only when the window has keyboard focus).
- Move: left-press anywhere (not in the 6 px edge zone) + move drags the window (`_drag_start_position = globalPos - pos`). Resize: left-press within `_RESIZE_MARGIN = 6` px of any edge -> `windowHandle().startSystemResize(edges)` (native). Hover cursor feedback (mouse tracking on window and panel, panel moves forwarded by event filter): `SizeHorCursor`/`SizeVerCursor`/corner cursors (`SizeFDiagCursor` for top-left & bottom-right, `SizeBDiagCursor` for the other corners)/arrow elsewhere.
Content/status API:
- `show_loading(msg="Translating...")`: clear cards/table; `status_label.setText(msg)`; show.
- `show_error(msg)`: clear; status = `"Error: " + msg`; show.
- `display_result(result, mode)`: clear; hide status; mode `per_sentence` -> one card per sentence; anything else (default `whole_text`, unknown values too) -> ONE card whose original = `"\n".join(all originals)` and translation = `"\n".join(all translations)`; then auto-size.
- Card (`SentenceCard`): white (`#FFFFFF`) frame radius 10, margins `(14,12,14,12)`; two word-wrapped labels stacked, no field labels: original bold `#2B2B2B`, translation bold `#2563EB`; text selectable by mouse. Cards spaced 10 px.
- Sheets table (`display_sheet_table`, out of scope beyond layout): read-only table; caption = `sheet_name or "gid {gid}"` and `range_address` joined by ` · ` (hidden if both empty); column width 220, min row height 28; cell widget = bold original + (if translated) bold accent translation, PlainText labels.

### 4.7 translating_badge.py (`TranslatingBadge`)
- QLabel with flags `Frameless | WindowStaysOnTop | ToolTip` (tooltip window class: never takes focus/activation - essential because it is shown BEFORE capture); text default `Translating...`; style `background #fff3cd; color #333; padding 4px 8px; border-radius 4px`.
- `show_at(point, message="Translating...")` / `hide()` are thread-safe (queued signals to the UI thread). On show: set text, `adjustSize()`, convert physical cursor point to logical by dividing by the primary screen's `devicePixelRatio` (rounded), position = cursor + `(16,16)` px, clamp to the primary screen `availableGeometry` (`x = max(left, min(x, right-w))`, same for y with bottom/top), `show()`. No auto-hide timer: hidden explicitly by the action's `finally`.
- Timing: shown by Alt+Alt / Ctrl+Alt+Alt actions right before capture (after Sheets-route precheck, only if cursor position was obtained), hidden in `finally` after the whole operation and before any result notification. Not used for Ctrl+Ctrl (that uses the explanation window's loading status after capture).

### 4.8 cross_thread_notifier.py (`CrossThreadNotifier(window)`)
`notify(message)` (any thread) -> queued signal -> UI thread: `window.show_error(message); window.bring_to_front()`. The window is the SAME `ExplanationWindow` instance used for Ctrl+Ctrl (so Alt+Alt notices are rendered as `Error: {message}` in that window).

---

## 5. app.py (`app.py`) - composition root

### 5.1 Startup sequence (`run()`, `app.py:240-716`)
1. `load_app_config_from_environment()` (loads `.env`, then env vars; `LLM_PROVIDER` default `claude_cli`; sheets flags `SHEETS_RANGE_REPLACE_ENABLED` / `SHEETS_EDIT_FULL_CELL_REPLACE_ENABLED` default "1"=on, `SHEETS_MAX_RANGE_CELLS` default 200).
2. `user_data_dir = %APPDATA%\ClaudeLangTrans_v5` (fallback `~/.claudelangtrans_v5`, dev only); `mkdir(parents, exist_ok)` (`config/paths.py`).
3. `_configure_logging`: level from env `LOG_LEVEL` (default INFO). If frozen (packaged exe): `RotatingFileHandler(user_data_dir/logs/claudelangtrans.log, maxBytes=2_000_000, backupCount=3, utf-8)` with format `%(asctime)s %(name)s %(levelname)s %(message)s`; else console. Never log captured/translated text (durations/status only).
4. `file_problems = []`; load `company.yaml`, `team.yaml`, personal store `user.yaml` (all append problems instead of raising); merge (company/team/user); `SettingsStore(settings.yaml, file_problems)`.
5. `_apply_autostart(settings.start_on_login)` (section 6).
6. `ModelCatalog(models_cache.json)`; `RuleBasedLanguageDetector`.
7. `build_llm_provider(app_config, cli_cwd=user_data_dir, cli_path_override=settings.claude_cli_path or None, model_override=settings.claude_model or None, effort_override=settings.claude_effort or None)`; then `_warm_up_in_background(provider, catalog)` = daemon thread running `refresh_catalog_and_warm_up(provider, catalog)` (boot CLI + refresh model list; failures non-fatal).
8. `TranslationService`, `WindowsUiaBackend(sheets_select_all_enabled=cfg.sheets.edit_full_cell_replace_enabled)`, `ClipboardFallback(Win32ClipboardBackend())`, two `WindowsSheetsBackend`s (`sheets_backend`: `assume_editable=cfg.range_replace_enabled`, used by Alt+Alt/Ctrl+Alt+Alt; `explain_sheets_backend`: `empty_editor_is_selection=True`, used by Ctrl+Ctrl/Alt+Ctrl+Ctrl), batch translation/correction services, `KoreanCorrectionService` (their providers read `llm_provider` through a closure so swaps apply).
9. Build the two adapters (2.5).
10. `QApplication`; `setQuitOnLastWindowClosed(False)`; `ExplanationWindow`; `CrossThreadNotifier(window)`; `TranslatingBadge`; `CtrlCtrlController(window, explain_adapter, dictionary, detector, provider, base, target, display_mode, tone)`.
11. Define action closures + `on_settings_saved`; build `TrayApplication` (`quit_callback=qt_app.quit`).
12. Build 2 guards, 4 runners, `ChordHotkeyRouter(is_physically_down=windows_modifier_is_down)`; `WindowsKeyboardHook(on_key_event)`; `start()`; connect `aboutToQuit` -> `hook.stop`, -> `close_current_llm_provider`.
13. SIGINT -> `qt_app.quit`; `QTimer` 200 ms no-op to let Python handle signals (dev-console only).
14. `tray_icon.show()`; then non-modal startup warnings (each a `QMessageBox(Warning, title "ClaudeLangTrans_v5", message)`, `setModal(false)`, `show()`, failures logged only), in order: (a) older-version autostart warning, (b) data-file-problems warning; `sys.exit(qt_app.exec())`.
- `main()` wrapper (`__init__.py:1-30`): any exception from `run()` is appended to `user_data_dir/logs/crash.log` (`\n--- unhandled exception ---\n` + traceback) then re-raised.
- No single-instance guard exists in v5 (two copies would double-fire; only older versions v3/v4 trigger a warning).
- Startup warning texts:
  - older (`startup/older_version_coexistence.py:130-152`): for each of HKCU Run values `ClaudeLangTrans` (label `v3`) and `ClaudeLangTrans_v4` (label `v4`) that exists (read-only lookup; per-value exceptions logged = absent): `이전 버전 ClaudeLangTrans({labels})가 Windows 시작 시 자동 실행되도록 설정되어 있습니다. 이전 버전과 v5를 동시에 실행하면 단축키가 두 번 동작할 수 있으니 {labels}를 종료하고 해당 버전 설정에서 자동 실행을 꺼주세요.` where `{labels}` = found labels joined `, `.
  - files (`config/file_problems.py:15-26`): None if no problems; else `설정·사전 파일 일부를 읽지 못해 해당 내용 없이 시작했습니다.\n\n` + lines `- {problem}` joined `\n` + `\n\n프로그램을 종료(트레이 → Exit)한 뒤 파일을 고치고 다시 시작하세요. 형식은 README의 '사전·설정 파일 샘플과 설정 방법'을 참고하세요.` (settings example problem: `settings.yaml: 파일 형식 오류로 읽지 못해 기본 설정을 사용합니다.`).

### 5.2 Threading model
- UI thread (Qt): all windows, tray, QMessageBoxes. Hook thread: LL hook + message loop; runs router synchronously (spawns action threads). Action threads: one fresh daemon thread per accepted trigger (guard-limited per key family). Background daemon threads: model-catalog refresh/warm-up (per provider build), F15 release timers (one short thread per solo Alt tap).
- Action threads touch UI only through signals: `CrossThreadNotifier.notify`, `TranslatingBadge.show_at/hide`, `CtrlCtrlController` signals. Action closures read `settings_store.get()` fresh each trigger (languages, tone, display mode, correction mode); dictionary via `translation_service.get_dictionary_entries()`.
- Uncaught exceptions in `alt_alt_action`/correction actions are not caught: the thread dies with a traceback; the guard is released (finally) and the badge hidden (finally). Ctrl+Ctrl controller methods catch exceptions from the LLM step (turn into `None` / `LLM_FAILED`).

### 5.3 Alt+Alt translate+replace flow (`alt_alt_action`, `app.py:400-452`)
1. log; `settings = settings_store.get()`.
2. `_precheck_replace_route("Alt+Alt", _SHEETS_RANGE_REPLACE_DISABLED_MESSAGE)` (see 5.4); None -> return.
3. `on_sheets = route != TEXT_INTERACTION or "docs.google.com/spreadsheets" in url`.
4. `pos = uia_backend.get_cursor_position()`; if not None `badge.show_at(pos)` (text `Translating...`).
5. `try`: route SHEETS_RANGE -> `perform_sheets_replace(...)`; else `perform_alt_alt_translate(adapter, translation_service, base, target, tone)`. `finally: badge.hide()`.
6. Notifications (all via `alt_alt_notifier.notify(...)` => `Error: ` window):
   - Sheets range outcome != SUCCESS -> `outcome.user_message()` (3.5).
   - `CAPTURE_FAILED`: if `on_sheets` -> `편집 중인 셀을 번역할 수 없습니다(수식·서식 또는 셀 상태를 확인할 수 없음).`; elif `uia_backend.foreground_is_docs_or_slides()` -> `텍스트를 선택한 후 Alt+Alt를 눌러주세요.`; else SILENT (Chat/Gmail without a compose focus: nothing shown).
   - Translation failed (`None`): SILENT (logged only; original untouched).
   - Replace outcome: logged; if `on_sheets and !success` and `error` in the map -> notify; non-Sheets failures SILENT:
     - `translation_starts_with_formula_prefix` -> `번역 결과가 =, +로 시작해 수식으로 해석될 수 있어 교체하지 않았습니다.`
     - `target_changed` -> `번역 중 편집 중인 셀의 내용이나 선택이 바뀌어 교체를 취소했습니다. 다시 실행해주세요.`
     - `full_cell_replace_not_enabled` -> `셀 전체 교체는 아직 검증되지 않아 꺼져 있습니다. 교체할 글자를 선택한 뒤 Alt+Alt를 눌러주세요.`
- Alt+Alt does NOT call `wait_for_modifiers_released` (the F15 trick + `_wait_until_alt_released(0.5)` inside the backend handle Alt); it can start capture while the second Alt is still down.

### 5.4 Sheets routing (one paragraph; internals out of scope)
Before any capture, each hotkey action calls `_sheets_route(backend)` -> `backend.current_url_and_state()` (cheap for non-Sheets pages: foreground Chrome + omnibox URL only; on a Sheets URL it resolves focus and maps role -> `CELL_SELECTION | CELL_EDITING | OTHER`). `choose_route(url,state)`: not a Sheets URL (or no URL) -> `TEXT_INTERACTION` (the Chrome text flow, unchanged); `CELL_SELECTION` -> `SHEETS_RANGE`; `CELL_EDITING` -> `TEXT_INTERACTION` (cell-edit strategy + `SheetsEditSafeInteraction`); anything else on Sheets -> `SHEETS_UNSUPPORTED`. Any exception in detection logs and falls back to `(None, TEXT_INTERACTION)`. Replace hotkeys' precheck: `SHEETS_UNSUPPORTED` -> notify `SheetsOutcome(WRONG_INPUT_STATE).user_message()` (`Sheets에서는 셀을 선택하거나 셀을 편집하는 중에 실행해주세요.`), return; `SHEETS_RANGE` while `range_replace_enabled` false -> notify the "range replace disabled" message (Alt+Alt: `Google Sheets 범위 교체(Alt+Alt)는 실제 Chrome에서 아직 검증되지 않아 기본으로 꺼져 있습니다. Ctrl+Ctrl로 번역을 확인하거나 셀 안의 글자를 선택한 뒤 Alt+Alt를 사용해주세요.`; Ctrl+Alt+Alt: `Google Sheets 범위 교체(Ctrl+Alt+Alt)는 실제 Chrome에서 아직 검증되지 않아 기본으로 꺼져 있습니다. Alt+Ctrl+Ctrl로 교정 결과를 확인하거나 셀 안의 글자를 선택한 뒤 Ctrl+Alt+Alt를 사용해주세요.`), return. Then range route -> `perform_sheets_replace` / `perform_sheets_correction_replace` (Sheets package; not specified here), else text flow. Default config has range replace + full-cell replace ENABLED (so the disabled messages are only for env overrides).

### 5.5 Ctrl+Alt+Alt correct+replace (`correction_replace_action`, `app.py:454-526`)
1. log; `settings`; `check_base_language(base)`: != `ko` -> notify `한글 교정은 Base Language가 ko(한국어)일 때만 사용할 수 있습니다. 설정에서 Base Language를 ko로 바꿔주세요.`; return.
2. `wait_for_modifiers_released(windows_modifier_is_down)` (2.0 s, both Ctrl and Alt must be physically up; the chord fires on the second Alt-DOWN with Ctrl+Alt still held and synthetic keys would combine with them). False -> notify `텍스트를 선택한 후 Ctrl+Alt+Alt를 눌러주세요.`; return.
3. `_precheck_replace_route("Ctrl+Alt+Alt", _SHEETS_CORRECTION_RANGE_REPLACE_DISABLED_MESSAGE)`; `on_sheets`; `mode = normalize_correction_mode(settings.korean_correction_mode)`.
4. cursor position -> `badge.show_at(pos, correction_loading_message(mode))` (`한국어 교정 중...` / `한국어 수정 중...`).
5. `try`: SHEETS_RANGE -> `perform_sheets_correction_replace(...)`; else `perform_correction_replace(adapter, correction_service, base, mode)`; `finally: badge.hide()`.
6. Notifications: sheets outcome != SUCCESS -> its message; capture failed (or `result is None`): on_sheets -> `편집 중인 셀을 교정할 수 없습니다(수식·서식 또는 셀 상태를 확인할 수 없음).` else ALWAYS `텍스트를 선택한 후 Ctrl+Alt+Alt를 눌러주세요.` (no docs/slides distinction here); correction status != SUCCESS -> `outcome.user_message()` (3.5); replace failure on Sheets -> map: `translation_starts_with_formula_prefix` -> `교정 결과가 =, +로 시작해 수식으로 해석될 수 있어 교체하지 않았습니다.`; `target_changed` -> `교정 중 편집 중인 셀의 내용이나 선택이 바뀌어 교체를 취소했습니다. 다시 실행해주세요.`; `full_cell_replace_not_enabled` -> `셀 전체 교체는 아직 검증되지 않아 꺼져 있습니다. 교체할 글자를 선택한 뒤 Ctrl+Alt+Alt를 눌러주세요.`; non-Sheets replace failure SILENT.
- The `_SHEETS_CORRECTION_*` constants (`app.py:171-186`) are the correction variants of the Alt+Alt Sheets strings so messages mention Ctrl+Alt+Alt / Alt+Ctrl+Ctrl and "교정" instead of "번역".

### 5.6 Alt+Ctrl+Ctrl correction view (`correction_view_action`, `:528-567`)
1. base check as above (notify goes to the explanation window via the notifier).
2. `wait_for_modifiers_released` False -> `ctrl_ctrl_controller.show_correction_no_selection()` (`텍스트를 선택한 후 Alt+Ctrl+Ctrl을 실행해주세요.`); return.
3. `mode`, `base_language` from settings; `controller.update_display_mode(settings.ctrl_ctrl_display_mode)` (languages/tone/dictionary not refreshed here).
4. Route via `explain_sheets_backend`: UNSUPPORTED -> `run_sheets(capture=WRONG_INPUT_STATE, ...)`; RANGE -> `run_sheets(capture_for_table(...), translate=correct_for_table(sel, batch_correction_service, mode), failure_status=CORRECTION_FAILED, loading_message=correction_loading_message(mode))`; else `run_correction(lambda text: correction_service.correct_sentences(text, base, mode), correction_loading_message(mode))`. No badge in this flow; window shows the loading status AFTER capture.

### 5.7 Ctrl+Ctrl translate+explain (`ctrl_ctrl_action`, `:569-601`)
Log; refresh controller (`update_languages`, `update_dictionary_entries(translation_service.get_dictionary_entries())`, `update_display_mode`, `update_tone`); route via `explain_sheets_backend`: UNSUPPORTED -> `run_sheets` with WRONG_INPUT_STATE; RANGE -> `run_sheets(capture_for_table, translate_for_table(selection, batch_translation_service, base, target, tone))`; else `controller.run()` (3.4). NOTE: no `wait_for_modifiers_released` here (Ctrl still held while synthetic Ctrl+C is sent: harmless), but the backend still waits for Alt release (0.5 s).

### 5.8 Settings hot-swap (`on_settings_saved`, `:603-622`; called by SettingsDialog.save after the store update)
`old = llm_provider`; `settings = store.get()`; `llm_provider = build_llm_provider(cfg, cli_cwd=data_dir, overrides from settings)`; `_warm_up_in_background`; `translation_service.update_llm_provider(new)`; `controller.update_llm_provider(new)`; `_apply_autostart(settings.start_on_login)`; `old.close()` if it has `close`. Batch/correction services see the new provider via the `llm_provider` closure. Other settings are picked up lazily (read per trigger): base/target language, tone, display mode, correction mode. NOT hot-swapped: hotkey intervals (needs restart), tray target-language checkmarks. Personal dictionary changes hot-swap through the dialog `destroyed` -> `_refresh_dictionary`.

### 5.9 Shutdown
Tray `Exit` -> `qt_app.quit()` (or console SIGINT). `aboutToQuit`: `keyboard_hook.stop()` (WM_QUIT + join 5 s), then `close_current_llm_provider()` (reads current provider; terminates the CLI subprocess). Closing Explanation/Settings/Dictionary windows never quits. Daemon threads die with the process. A held F15 from an in-flight 50 ms timer is not explicitly released at shutdown.

---

## 6. startup/autostart.py + windows_registry.py
- Key `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (`startup/windows_registry.py:76`), value name `ClaudeLangTrans_v5` (`:77`), type `REG_SZ`.
- `autostart_command(frozen, executable)` (`startup/autostart.py:35-41`): `None` when not frozen (running from source/`uv run`: registry untouched, log "Not a packaged exe; leaving the autostart registry untouched", returns false); else the executable path wrapped in double quotes: `"C:\path with spaces\app.exe"` (no arguments).
- `apply_autostart_setting(enabled, frozen, executable, registry_factory)`: `enabled` -> `SetValueEx(Run, name, REG_SZ, quoted_path)` (rewritten every startup and every Settings save, which also fixes a moved exe); disabled -> `DeleteValue` (missing value = no-op). Never raises: failures logged (`Could not update the autostart registry`), app continues. Called at startup (after settings load) and in `on_settings_saved`. The setting default is false.
- Read-only helper `run_value_exists(value_name)` (`windows_registry.py:80-88`): `OpenKey(HKCU, Run, KEY_READ)` + `QueryValueEx`; `FileNotFoundError` -> false. Used only by the older-version warning (values `ClaudeLangTrans`, `ClaudeLangTrans_v4`); v5 never writes or deletes those.
- Rust equivalent of "frozen" = release/packaged exe vs. dev/debug-run (decide explicitly; do not register `cargo run` binaries).

---

## 7. Things the Rust port must not forget
1. Hook: always `CallNextHookEx`, never block a key; ignore `LLKHF_INJECTED` events for routing AND for F15 logic; handle panics inside the callback; do work (threads) outside the callback; callback must return well under ~300 ms. Needs its own thread with a `GetMessage` loop; stop via `PostThreadMessage(WM_QUIT)`; unhook on exit. Detect and surface a NULL hook handle (Python ignores it).
2. `SendInput` `cbSize` must equal true `sizeof(INPUT)` (40 on x64); check the return value; never trust "injecting" logs.
3. F15 trick order: press F15 BEFORE passing a solo Alt-up, keep it held across the Alt-up, release after ~50 ms from another thread; skip when another key (incl. Ctrl) went down while Alt was held. Do not inject on Alt-down; do not use a down+up pair.
4. Double-press: per-key detectors with `<=` interval measured down-to-down, auto-repeat guard, `reset_pending` leaves is-down alone; non-modifier keys are NOT forwarded (no other-key reset); opposite-modifier down/up resets both runners of the other key; chord vs plain decided at key-down; stale-modifier probe via `GetAsyncKeyState` only at the next opposite-key down; shared guard per key family; Alt and Ctrl families run independently.
5. Trigger fires on the 2nd key-DOWN; before any synthetic key: Ctrl/Alt chords wait up to 2.0 s for both modifiers up (abort + message if not); Alt-only paths wait up to 0.5 s then continue anyway. Synthetic key while Alt is held becomes `Alt+key` (Alt+Esc switches windows).
6. Show UI AFTER capture for Ctrl+Ctrl (the popup steals focus from Chrome); the translate badge is shown before capture but must be a no-activate tooltip-style window. `bring_to_front` must use a TOPMOST then NOTOPMOST z-order flip; plain activation is blocked by the foreground-lock policy for background processes.
7. COM (UI Automation) must be initialized on every thread that touches UIA; do not share UIA elements across threads; action threads are fresh per trigger.
8. UIA is used only for foreground window, class `Chrome_WidgetWin*`, omnibox URL (`OmniboxViewViews` edit, ValuePattern), focused control name/type/AutomationId, render-widget `SetFocus`. Text comes from the clipboard (Ctrl+C), not TextPattern. Detect apps by substring on the omnibox text (may lack scheme); classify roles by ControlType first (locale-independent), then names (Korean + English).
9. Stray toolbar focus (solo Alt moves Chrome focus to a toolbar button): recover only when focused control is a `ButtonControl` AND role in {message_history, unknown} AND supported URL. Docs/Slides: UIA `SetFocus` on `Chrome_RenderWidgetHostHWND` (never Esc - it clears Docs/Slides selection). Chat/Gmail (or fallback): wait Alt released, send Esc, wait 0.1 s, re-read focus.
10. Selection probe uses a SENTINEL on the clipboard, not before/after diff; no-selection fallback = Ctrl+A, wait 0.2 s (multi-paragraph settle), Ctrl+C, wait 0.05 s. Fix the Python quirk: seed a sentinel in the full-text copy too (otherwise an empty field returns the user's old clipboard as "full text").
11. `SendKeys` in Python silently adds ~0.5 s after each chord; the ported code must provide equivalent settle behavior, especially: paste must be consumed before restoring the user's clipboard; copy must complete before reading.
12. Replace safety: re-validate focus (re-run the full capture probe) right before paste; compare only `app` + `field_id`; Slides `field_id` must be `"textbox"` for both branches; no-selection replace re-presses Ctrl+A right before Ctrl+V; never send Enter/Ctrl+Enter/Send; Gmail only subject/body; To/Cc/Bcc untouched; Chat only `compose` for replace; Docs requires a selection; read-only (Ctrl+Ctrl) strategies accept any genuine selection; first matching strategy wins and does not fall through.
13. Capture retry: 5 attempts, 0.08 s apart, fresh capture each time; replace is never retried. Chat-vs-Gmail ambiguity: Gmail field names win over a stale `#chat` URL hash; Chat identified by `chat.google.com` or Gmail + `#chat`.
14. Clipboard: text-only preserve/restore on the Chrome path (3 set attempts, restore only if previous text existed); multi-format snapshot (skip handle formats, UTF-16 + NUL for text) + sequence-number guard for Sheets (never overwrite a clipboard the user changed meanwhile); `OpenClipboard` retry 5 x 20 ms; serialize Sheets clipboard use across both hotkey families with one process-wide re-entrant lock.
15. Notifications policy is subtle: translate failure and non-Sheets replace failure are SILENT; capture failure shows a message only on Sheets (always), Docs/Slides (Alt+Alt) or always (Ctrl+Alt+Alt / Ctrl+Ctrl). All messages render as `Error: <msg>` in the explanation window.
16. Settings: intervals only applied at startup (port may improve but must not regress); save -> store update -> provider rebuild + warm-up + autostart sync + close old provider; haiku warning only on user-driven change; model/effort dropdown rules (4.4); catalog refresh arrives on a background thread -> marshal to UI thread.
17. Badge coordinates: `GetCursorPos` physical px; Qt divided by primary screen DPR and clamped to the primary screen. In Rust use per-monitor DPI properly (monitor from cursor point) but keep +16/+16 offset and clamping.
18. Startup: warnings are non-modal message boxes shown after the tray icon, older-version warning first; never let a YAML problem abort startup; logs go to `%APPDATA%\ClaudeLangTrans_v5\logs\` (rotating 2 MB x 3) when packaged; crash log `crash.log`. No single-instance guard in Python (consider adding one, noting the behavior change).
19. Explanation window: closing only hides; `display_result` does not re-show (a result arriving after the user closed the window stays hidden) - decide deliberately. Whole-text mode joins sentences with `\n`; unknown display mode = whole_text. Auto-size only on result display with the clamps in 4.6. Frameless needs custom drag/resize (6 px) and Esc.
20. Pause only suppresses the action (after detection, on the action thread); F15 injection and router state continue while paused.
