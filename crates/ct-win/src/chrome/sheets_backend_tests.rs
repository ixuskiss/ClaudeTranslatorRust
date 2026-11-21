use super::*;
use crate::clipboard::ClipboardError;
use crate::format_clipboard::ClipboardSnapshot;
use crate::hotkey::Modifier;
use crate::keys::KeyError;
use ct_sheets::{
    CUSTOM_MIME, SheetGrid, SheetLocation, UnsupportedReason, build_cf_html,
    build_replacement_html, encode_custom_data, parse_sheets_html,
};
use serde_json::json;
use std::sync::Mutex;

const URL: &str = "https://docs.google.com/spreadsheets/d/doc/edit#gid=0";
const SEL: Option<SheetsInputState> = Some(SheetsInputState::CellSelection);
const MAX: usize = 100;

fn rejected(r: Result<(), CaptureError>) -> Option<Option<UnsupportedReason>> {
    r.err().map(|e| e.reason)
}

// ---- capture_decision -------------------------------------------------------

#[test]
fn whole_row_selection_is_rejected_before_copy() {
    for address in ["3:3", "A:A", "A1:B"] {
        assert_eq!(
            rejected(capture_decision(Some(URL), SEL, Some(address), MAX)),
            Some(Some(UnsupportedReason::WholeRowOrColumn)),
            "{address}"
        );
    }
    assert_eq!(
        rejected(capture_decision(Some(URL), SEL, Some("A1:B2,C3"), MAX)),
        Some(Some(UnsupportedReason::MultipleRanges))
    );
}

#[test]
fn oversized_address_is_rejected_before_copy() {
    assert_eq!(
        rejected(capture_decision(Some(URL), SEL, Some("A1:J11"), MAX)),
        Some(Some(UnsupportedReason::RangeTooLarge))
    );
    // exactly at the limit still copies
    assert!(capture_decision(Some(URL), SEL, Some("A1:J10"), MAX).is_ok());
}

#[test]
fn unknown_address_still_copies() {
    assert!(capture_decision(Some(URL), SEL, None, MAX).is_ok());
    assert!(capture_decision(Some(URL), SEL, Some("garbage!!"), MAX).is_ok());
    assert!(capture_decision(Some(URL), SEL, Some("B2"), MAX).is_ok());
}

#[test]
fn capture_requires_sheets_document_and_cell_selection() {
    let editing = Some(SheetsInputState::CellEditing);
    let other = Some(SheetsInputState::Other);
    for (url, state) in [
        (Some(URL), editing),
        (Some(URL), other),
        (Some(URL), None),
        (None, SEL),
        (Some("https://docs.google.com/document/d/x/edit"), SEL),
        (Some("https://docs.google.com/spreadsheets/u/0/"), SEL),
    ] {
        assert_eq!(
            rejected(capture_decision(url, state, Some("A1"), MAX)),
            Some(None),
            "{url:?} {state:?}"
        );
    }
}

// ---- paste_target_matches ---------------------------------------------------

fn tiny_grid() -> SheetGrid {
    parse_sheets_html(
        "<google-sheets-html-origin><table><tr><td>a</td></tr></table>",
        None,
    )
    .unwrap()
}

fn selection_at(doc: &str, gid: Option<&str>, address: Option<&str>) -> SheetSelection {
    SheetSelection {
        location: SheetLocation {
            document_id: doc.into(),
            gid: gid.map(String::from),
            range_address: address.map(String::from),
        },
        grid: tiny_grid(),
        hidden_or_filtered: Some(false),
    }
}

#[test]
fn paste_target_requires_same_document_gid_and_address() {
    let captured = selection_at("doc", Some("0"), Some("A1"));
    assert!(paste_target_matches(&captured, Some(URL), SEL, Some("A1")));
    // other document
    let other_doc = "https://docs.google.com/spreadsheets/d/zzz/edit#gid=0";
    assert!(!paste_target_matches(
        &captured,
        Some(other_doc),
        SEL,
        Some("A1")
    ));
    // other tab
    let other_gid = "https://docs.google.com/spreadsheets/d/doc/edit#gid=5";
    assert!(!paste_target_matches(
        &captured,
        Some(other_gid),
        SEL,
        Some("A1")
    ));
    // missing gid when captured had one
    let no_gid = "https://docs.google.com/spreadsheets/d/doc/edit";
    assert!(!paste_target_matches(
        &captured,
        Some(no_gid),
        SEL,
        Some("A1")
    ));
    // other address / unreadable address
    assert!(!paste_target_matches(&captured, Some(URL), SEL, Some("A2")));
    assert!(!paste_target_matches(&captured, Some(URL), SEL, None));
    // not a cell selection
    assert!(!paste_target_matches(
        &captured,
        Some(URL),
        Some(SheetsInputState::CellEditing),
        Some("A1")
    ));
    assert!(!paste_target_matches(
        &captured,
        Some(URL),
        None,
        Some("A1")
    ));
    assert!(!paste_target_matches(&captured, None, SEL, Some("A1")));
    // captured without an address never matches
    let unknown = selection_at("doc", Some("0"), None);
    assert!(!paste_target_matches(&unknown, Some(URL), SEL, None));
    // both without gid is fine
    let no_gid_capture = selection_at("doc", None, Some("A1"));
    assert!(paste_target_matches(
        &no_gid_capture,
        Some(no_gid),
        SEL,
        Some("A1")
    ));
}

// ---- focus state / name box (v5 test_windows_sheets_backend.py) ------------

#[test]
fn empty_editor_is_a_range_selection_when_opted_in() {
    assert_eq!(
        focus_input_state(Role::SheetsCellEditor, Some("\n\n"), true),
        SheetsInputState::CellSelection
    );
}

#[test]
fn empty_editor_stays_editing_by_default() {
    assert_eq!(
        focus_input_state(Role::SheetsCellEditor, Some("\n\n"), false),
        SheetsInputState::CellEditing
    );
}

#[test]
fn editor_with_text_stays_editing_when_opted_in() {
    assert_eq!(
        focus_input_state(Role::SheetsCellEditor, Some("안녕하세요.\n"), true),
        SheetsInputState::CellEditing
    );
    assert_eq!(
        focus_input_state(Role::SheetsCellEditor, None, true),
        SheetsInputState::CellEditing
    );
    assert_eq!(
        focus_input_state(Role::SheetsGrid, None, false),
        SheetsInputState::CellSelection
    );
    assert_eq!(
        focus_input_state(Role::SheetsNameBox, None, true),
        SheetsInputState::Other
    );
}

#[test]
fn range_address_uses_first_non_blank_name_box_value_trimmed() {
    assert_eq!(
        first_address([Some(" A1:E1 ".to_string()), Some("B2".into())]),
        Some("A1:E1".into())
    );
    assert_eq!(
        first_address([None, Some("  ".into()), Some("C3".into())]),
        Some("C3".into())
    );
}

#[test]
fn missing_name_box_is_unknown() {
    assert_eq!(first_address([None, None, None]), None);
    assert_eq!(
        first_address([Some(String::new()), Some(" \n".into())]),
        None
    );
}

#[test]
fn name_box_lookups_stop_at_first_hit() {
    let calls = Mutex::new(0);
    let lookups = (0..3).map(|_| {
        *calls.lock().unwrap() += 1;
        Some("A1".to_string())
    });
    assert_eq!(first_address(lookups), Some("A1".into()));
    assert_eq!(*calls.lock().unwrap(), 1);
}

// ---- trimming helpers -------------------------------------------------------

#[test]
fn custom_data_is_trimmed_to_declared_length() {
    let encoded = encode_custom_data(&[("k".into(), "v".into())]);
    let mut padded = encoded.clone();
    padded.extend_from_slice(&[0u8; 13]);
    assert_eq!(trim_custom_data(&padded), encoded.as_slice());
    assert_eq!(trim_custom_data(&encoded), encoded.as_slice());
    // declared length longer than the buffer or a too-short buffer: left as is
    let bogus = [0xFF, 0, 0, 0, 1, 2];
    assert_eq!(trim_custom_data(&bogus), &bogus[..]);
    assert_eq!(trim_custom_data(&[1, 2]), &[1, 2][..]);
    assert_eq!(trim_custom_data(&[]), &[] as &[u8]);
}

#[test]
fn html_bytes_drop_trailing_nuls_only() {
    assert_eq!(trim_trailing_nuls(b"abc\0\0\0"), b"abc");
    assert_eq!(trim_trailing_nuls(b"a\0b"), b"a\0b");
    assert_eq!(trim_trailing_nuls(b"\0\0"), b"");
    assert_eq!(trim_trailing_nuls(b""), b"");
}

// ---- capture / write sequences over fakes ------------------------------------

const FAST: Timeouts = Timeouts {
    copy: Duration::from_millis(150),
    paste_confirm: Duration::from_millis(150),
};

struct Probe {
    url: Option<String>,
    state: Option<SheetsInputState>,
    address: Option<String>,
    address_reads: Mutex<usize>,
}

impl Probe {
    fn new(url: Option<&str>, state: Option<SheetsInputState>, address: Option<&str>) -> Probe {
        Probe {
            url: url.map(String::from),
            state,
            address: address.map(String::from),
            address_reads: Mutex::new(0),
        }
    }
}

impl SheetsProbe for Probe {
    fn url_and_state(&self) -> (Option<String>, Option<SheetsInputState>) {
        (self.url.clone(), self.state)
    }
    fn range_address(&self) -> Option<String> {
        *self.address_reads.lock().unwrap() += 1;
        self.address.clone()
    }
}

/// Multi-format clipboard + keys. Ctrl+C writes `copy` (padded like GlobalSize) when set;
/// `ctrl_c_changes` controls whether a copy bumps the sequence number at all.
type Formats = Vec<(String, Vec<u8>)>;

struct Fake {
    seq: Mutex<u32>,
    formats: Mutex<Formats>,
    sent: Mutex<Vec<Chord>>,
    restores: Mutex<usize>,
    writes: Mutex<Vec<Formats>>,
    copy: Option<Formats>,
    ctrl_c_changes: bool,
}

impl Fake {
    fn new(copy: Option<Formats>, ctrl_c_changes: bool) -> Fake {
        Fake {
            seq: Mutex::new(10),
            formats: Mutex::new(vec![("user".into(), b"mine".to_vec())]),
            sent: Mutex::new(Vec::new()),
            restores: Mutex::new(0),
            writes: Mutex::new(Vec::new()),
            copy,
            ctrl_c_changes,
        }
    }
    fn sent(&self) -> Vec<Chord> {
        self.sent.lock().unwrap().clone()
    }
    fn restores(&self) -> usize {
        *self.restores.lock().unwrap()
    }
}

impl FormatClipboard for Fake {
    fn sequence_number(&self) -> u32 {
        *self.seq.lock().unwrap()
    }
    fn snapshot(&self) -> Option<ClipboardSnapshot> {
        Some(ClipboardSnapshot::new(vec![(1, b"mine".to_vec())]))
    }
    fn restore(&self, _: &ClipboardSnapshot) -> Result<(), ClipboardError> {
        *self.restores.lock().unwrap() += 1;
        *self.formats.lock().unwrap() = vec![("user".into(), b"mine".to_vec())];
        *self.seq.lock().unwrap() += 1;
        Ok(())
    }
    fn read_text(&self) -> Option<String> {
        unreachable!("range path never reads CF_UNICODETEXT")
    }
    fn read_registered(&self, name: &str) -> Option<Vec<u8>> {
        let formats = self.formats.lock().unwrap();
        formats
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.clone())
    }
    fn write_registered(&self, formats: &[(&str, &[u8])]) -> Result<(), ClipboardError> {
        let owned: Formats = formats
            .iter()
            .map(|(n, b)| (n.to_string(), b.to_vec()))
            .collect();
        self.writes.lock().unwrap().push(owned.clone());
        *self.formats.lock().unwrap() = owned;
        *self.seq.lock().unwrap() += 1;
        Ok(())
    }
}

impl KeySender for Fake {
    fn send(&self, chord: Chord) -> Result<(), KeyError> {
        self.sent.lock().unwrap().push(chord);
        if chord == Chord::CtrlC && self.ctrl_c_changes {
            if let Some(copy) = &self.copy {
                *self.formats.lock().unwrap() = copy.clone();
            }
            *self.seq.lock().unwrap() += 1;
        }
        Ok(())
    }
    fn is_down(&self, _: Modifier) -> bool {
        false
    }
}

const TWO_CELLS: &str =
    "<google-sheets-html-origin><table><tr><td>hello</td><td>world</td></tr></table>";

fn padded(mut bytes: Vec<u8>, pad: usize) -> Vec<u8> {
    bytes.extend(std::iter::repeat_n(0u8, pad));
    bytes
}

fn sheets_copy(fragment: &str, custom: Option<Vec<u8>>) -> Formats {
    let mut v = vec![(
        HTML_FORMAT.to_string(),
        padded(build_cf_html(fragment).into_bytes(), 7),
    )];
    if let Some(c) = custom {
        v.push((CUSTOM_FORMAT.to_string(), padded(c, 9)));
    }
    v
}

#[test]
fn rejected_selection_never_copies() {
    let probe = Probe::new(Some(URL), SEL, Some("A:A"));
    let f = Fake::new(Some(sheets_copy(TWO_CELLS, None)), true);
    let err = capture_with(&probe, &f, &f, MAX, FAST).unwrap_err();
    assert_eq!(err.reason, Some(UnsupportedReason::WholeRowOrColumn));
    assert!(f.sent().is_empty());

    let probe = Probe::new(Some(URL), Some(SheetsInputState::CellEditing), None);
    let err = capture_with(&probe, &f, &f, MAX, FAST).unwrap_err();
    assert_eq!(err.reason, None);
    assert!(f.sent().is_empty());
}

#[test]
fn capture_reads_padded_html_and_custom_data_and_restores_clipboard() {
    let compact = json!({"2": [195, 195], "3": {"1": [2, 2], "4": ["hello", "world"]}, "15": {"1": 1, "2": 2}});
    let custom = encode_custom_data(&[(CUSTOM_MIME.into(), compact.to_string())]);
    let probe = Probe::new(Some(URL), SEL, Some("A1:B1"));
    let f = Fake::new(Some(sheets_copy(TWO_CELLS, Some(custom))), true);
    let sel = capture_with(&probe, &f, &f, MAX, FAST).unwrap();
    assert_eq!(sel.location.document_id, "doc");
    assert_eq!(sel.location.gid.as_deref(), Some("0"));
    assert_eq!(sel.location.range_address.as_deref(), Some("A1:B1"));
    assert_eq!((sel.grid.rows, sel.grid.cols), (1, 2));
    assert!(sel.grid.compact_json.is_some());
    assert_eq!(sel.hidden_or_filtered, Some(false));
    assert_eq!(f.sent(), vec![Chord::CtrlC]);
    assert_eq!(f.restores(), 1);
}

#[test]
fn unknown_address_copies_and_hidden_state_is_unknown() {
    let probe = Probe::new(Some(URL), SEL, None);
    let f = Fake::new(Some(sheets_copy(TWO_CELLS, None)), true);
    let sel = capture_with(&probe, &f, &f, MAX, FAST).unwrap();
    assert_eq!(sel.location.range_address, None);
    assert_eq!(sel.hidden_or_filtered, None);

    // address smaller than the grid: hidden state unknown too
    let probe = Probe::new(Some(URL), SEL, Some("A1"));
    let sel = capture_with(&probe, &f, &f, MAX, FAST).unwrap();
    assert_eq!(sel.hidden_or_filtered, None);
}

#[test]
fn copy_without_clipboard_change_fails_without_restore() {
    let probe = Probe::new(Some(URL), SEL, Some("A1:B1"));
    let f = Fake::new(None, false);
    let err = capture_with(&probe, &f, &f, MAX, FAST).unwrap_err();
    assert_eq!(err.reason, None);
    assert_eq!(f.sent(), vec![Chord::CtrlC]);
    assert_eq!(f.restores(), 0);
}

#[test]
fn copy_without_html_or_unparseable_html_fails() {
    let probe = Probe::new(Some(URL), SEL, Some("A1:B1"));
    let f = Fake::new(Some(vec![("other".into(), b"x".to_vec())]), true);
    assert_eq!(
        capture_with(&probe, &f, &f, MAX, FAST).unwrap_err().reason,
        None
    );
    assert_eq!(f.restores(), 1);

    let f = Fake::new(
        Some(sheets_copy("<table><tr><td>x</td></tr></table>", None)),
        true,
    );
    assert_eq!(
        capture_with(&probe, &f, &f, MAX, FAST).unwrap_err().reason,
        None
    );
}

/// Legacy copy without compact data: cell kinds come from `data-sheets-value`.
const LEGACY_TWO_CELLS: &str = "<google-sheets-html-origin><table><tr>\
    <td data-sheets-value='{\"1\":2,\"2\":\"hello\"}'>hello</td>\
    <td data-sheets-value='{\"1\":2,\"2\":\"world\"}'>world</td></tr></table>";

fn captured_two_cells(compact: Option<serde_json::Value>) -> SheetSelection {
    let html = if compact.is_some() {
        TWO_CELLS
    } else {
        LEGACY_TWO_CELLS
    };
    let grid = parse_sheets_html(html, compact.as_ref()).unwrap();
    SheetSelection {
        location: SheetLocation {
            document_id: "doc".into(),
            gid: Some("0".into()),
            range_address: Some("A1:B1".into()),
        },
        grid,
        hidden_or_filtered: Some(false),
    }
}

#[test]
fn write_changed_target_sends_no_keys_and_writes_nothing() {
    let sel = captured_two_cells(None);
    let probe = Probe::new(Some(URL), SEL, Some("A1:C1"));
    let f = Fake::new(None, true);
    let cf = build_cf_html(&build_replacement_html(&sel.grid, &[((0, 0), "x".into())]).unwrap());
    assert!(write_with(&probe, &f, &f, &sel, &cf, FAST).is_err());
    assert!(f.sent().is_empty());
    assert!(f.writes.lock().unwrap().is_empty());
}

#[test]
fn confirmed_paste_sends_ctrl_v_then_ctrl_c_and_restores() {
    let sel = captured_two_cells(None);
    let probe = Probe::new(Some(URL), SEL, Some("A1:B1"));
    let f = Fake::new(None, true);
    let cf = build_cf_html(&build_replacement_html(&sel.grid, &[((0, 0), "x".into())]).unwrap());
    write_with(&probe, &f, &f, &sel, &cf, FAST).unwrap();
    assert_eq!(f.sent(), vec![Chord::CtrlV, Chord::CtrlC]);
    let writes = f.writes.lock().unwrap().clone();
    assert_eq!(writes.len(), 1);
    // no compact data: HTML only, never CF_UNICODETEXT
    assert_eq!(writes[0].len(), 1);
    assert_eq!(writes[0][0].0, HTML_FORMAT);
    assert_eq!(writes[0][0].1, cf.as_bytes());
    assert_eq!(f.restores(), 1);
}

#[test]
fn unconfirmed_paste_leaves_payload_and_returns_ok() {
    let sel = captured_two_cells(None);
    let probe = Probe::new(Some(URL), SEL, Some("A1:B1"));
    let f = Fake::new(None, false);
    let cf = build_cf_html(&build_replacement_html(&sel.grid, &[((0, 0), "x".into())]).unwrap());
    write_with(&probe, &f, &f, &sel, &cf, FAST).unwrap();
    assert_eq!(f.sent(), vec![Chord::CtrlV, Chord::CtrlC]);
    assert_eq!(f.restores(), 0);
    assert_eq!(f.read_registered(HTML_FORMAT).unwrap(), cf.as_bytes());
}

#[test]
fn native_paste_keeps_untranslated_whitespace_cell_formatting() {
    let data = json!({"2": [195, 1219], "3": {"1": [2, 2], "4": ["hello", " "]},
        "15": {"1": 1, "2": 2}, "17": [{"1": [{"1": 0, "2": {"5": 1}}]}]});
    let grid = parse_sheets_html(
        "<google-sheets-html-origin><table><tr><td>hello</td><td><b> </b></td></tr></table>",
        Some(&data),
    )
    .unwrap();
    let sel = SheetSelection {
        location: SheetLocation {
            document_id: "doc".into(),
            gid: Some("0".into()),
            range_address: Some("A1:B1".into()),
        },
        grid,
        hidden_or_filtered: Some(false),
    };
    let probe = Probe::new(Some(URL), SEL, Some("A1:B1"));
    let f = Fake::new(None, true);
    let cf = build_cf_html(&build_replacement_html(&sel.grid, &[((0, 0), "안녕".into())]).unwrap());
    write_with(&probe, &f, &f, &sel, &cf, FAST).unwrap();

    let writes = f.writes.lock().unwrap().clone();
    let names: Vec<&str> = writes[0].iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec![HTML_FORMAT, CUSTOM_FORMAT]);
    let native = compact_from_custom(Some(&writes[0][1].1)).unwrap().unwrap();
    assert_eq!(native["3"]["4"], json!(["안녕", " "]));
    assert_eq!(native["17"], data["17"]);
    assert_eq!(native["2"], data["2"]);
    let stored: serde_json::Value =
        serde_json::from_str(sel.grid.compact_json.as_deref().unwrap()).unwrap();
    assert_eq!(stored, data);
}

#[test]
fn unusable_cf_html_with_compact_data_fails_before_touching_clipboard() {
    let data = json!({"2": [195, 195], "3": {"1": [2, 2], "4": ["hello", "world"]}, "15": {"1": 1, "2": 2}});
    let sel = captured_two_cells(Some(data));
    let probe = Probe::new(Some(URL), SEL, Some("A1:B1"));
    let f = Fake::new(None, true);
    assert!(write_with(&probe, &f, &f, &sel, "not cf html", FAST).is_err());
    assert!(f.sent().is_empty());
    assert!(f.writes.lock().unwrap().is_empty());
    assert_eq!(f.restores(), 0);
}
