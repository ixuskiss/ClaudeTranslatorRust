//! Builds the clipboard payload that writes replacement text back into Sheets.
//!
//! The HTML payload is the *original* copied fragment with only the targeted text
//! cells spliced in by byte offset, never parsed and re-serialized, so every other
//! cell and the target cells' own styling attributes stay byte-for-byte as Sheets
//! produced them.

use std::collections::BTreeMap;
use std::ops::Range;

use serde_json::{Map, Value};

use crate::custom_clipboard::{CUSTOM_MIME, encode_custom_data, replace_compact_strings};
use crate::html::{decode_entities, parse_sheets_html};
use crate::model::{CellKey, CellKind, SheetGrid, SheetsParseError};

/// Failure while splicing the replacement HTML; carries a fixed phrase only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PayloadError(pub &'static str);

const UNTERMINATED: PayloadError = PayloadError("unterminated <td> tag");

/// Python `html.escape`: `&`, `<`, `>` and, with `quote`, `"` and `'`.
fn html_escape(text: &str, quote: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if quote => out.push_str("&quot;"),
            '\'' if quote => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Index just past the `>` closing the opening tag that begins at `start`,
/// ignoring any `>` inside quoted attribute values.
fn open_tag_end(source: &str, start: usize) -> Result<usize, PayloadError> {
    let mut quote: Option<u8> = None;
    for (offset, &byte) in source.as_bytes()[start..].iter().enumerate() {
        match quote {
            Some(q) if byte == q => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'>' => return Ok(start + offset + 1),
            None => {}
        }
    }
    Err(UNTERMINATED)
}

/// First ` name="..."` / ` name='...'` in `tag` (leading whitespace included in the
/// returned span, as the v5 regex did). Returns `(span, value_range)`.
fn find_attr(tag: &str, name: &str) -> Option<(Range<usize>, Range<usize>)> {
    let needle = format!("{name}=");
    let mut from = 0;
    while let Some(found) = tag[from..].find(&needle) {
        let at = from + found;
        from = at + 1;
        let Some(ws) = tag[..at].chars().next_back().filter(|c| c.is_whitespace()) else {
            continue;
        };
        let value_start = at + needle.len();
        let Some(&quote) = tag.as_bytes().get(value_start) else {
            continue;
        };
        if quote != b'"' && quote != b'\'' {
            continue;
        }
        if let Some(len) = tag[value_start + 1..].find(quote as char) {
            let close = value_start + 1 + len;
            return Some((at - ws.len_utf8()..close + 1, value_start + 1..close));
        }
    }
    None
}

fn new_td(old_td: &str, text: &str) -> Result<String, PayloadError> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let open_end = open_tag_end(old_td, 0)?;
    let mut open_tag = old_td[..open_end].to_string();
    while let Some((span, _)) = find_attr(&open_tag, "data-sheets-textstyleruns") {
        open_tag.replace_range(span, "");
    }
    let close_start = old_td
        .rfind("</")
        .filter(|&i| i >= open_end)
        .ok_or(UNTERMINATED)?;
    if let Some((span, value)) = find_attr(&open_tag, "data-sheets-value") {
        let mut map = match serde_json::from_str::<Value>(&decode_entities(&open_tag[value])) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        };
        // A string value: never re-interpreted as number/formula.
        map.insert("1".to_string(), Value::from(2));
        map.insert("2".to_string(), Value::String(text.clone()));
        let attr = html_escape(&Value::Object(map).to_string(), true);
        open_tag.replace_range(span, &format!(" data-sheets-value=\"{attr}\""));
    } else {
        open_tag.pop(); // the closing '>'
        let quoted = Value::String(text.clone()).to_string();
        let attr = html_escape(&format!("{{\"1\": 2, \"2\": {quoted}}}"), true);
        open_tag.push_str(&format!(" data-sheets-value=\"{attr}\">"));
    }
    let content = html_escape(&text, false).replace('\n', "<br>");
    Ok(format!("{open_tag}{content}{}", &old_td[close_start..]))
}

fn span_of(grid: &SheetGrid, index: usize) -> Result<(usize, usize), PayloadError> {
    grid.cell_spans
        .get(index)
        .copied()
        .filter(|&(start, end)| start <= end && grid.html.get(start..end).is_some())
        .ok_or(PayloadError("no such cell"))
}

pub fn build_replacement_html(
    grid: &SheetGrid,
    replacements: &[(CellKey, String)],
) -> Result<String, PayloadError> {
    let mut targets: BTreeMap<usize, &str> = BTreeMap::new();
    for (key, text) in replacements {
        let index = grid
            .cells
            .iter()
            .position(|c| (c.row, c.col) == *key)
            .ok_or(PayloadError("no such cell"))?;
        if grid.cells[index].kind != CellKind::Text {
            return Err(PayloadError("not a text cell"));
        }
        targets.insert(index, text);
    }

    let mut edits: BTreeMap<usize, String> = BTreeMap::new();
    if grid.compact_json.is_some() && !replacements.is_empty() {
        // Modern HTML carries formulas only in the compact data; put them back.
        for (index, cell) in grid.cells.iter().enumerate() {
            let Some(formula) = &cell.formula else {
                continue;
            };
            let (start, end) = span_of(grid, index)?;
            let original = &grid.html[start..end];
            let open_end = open_tag_end(original, 0)?;
            if !original[..open_end].contains("data-sheets-formula=") {
                let attr = html_escape(formula, true);
                edits.insert(
                    index,
                    format!(
                        "{} data-sheets-formula=\"{attr}\">{}",
                        &original[..open_end - 1],
                        &original[open_end..]
                    ),
                );
            }
        }
    }
    for (&index, text) in &targets {
        let (start, end) = span_of(grid, index)?;
        edits.insert(index, new_td(&grid.html[start..end], text)?);
    }

    let mut result = grid.html.clone();
    // Splice back to front so earlier offsets stay valid.
    for (&index, replacement) in edits.iter().rev() {
        let (start, end) = span_of(grid, index)?;
        result.replace_range(start..end, replacement);
    }
    Ok(result)
}

/// The native custom-clipboard payload for a paste of `new_fragment`, or `None`
/// when the capture carried no compact data. Strings are replaced for EVERY
/// original non-blank text cell (not only the translated ones), so such cells lose
/// their character runs; whitespace-only cells keep them.
pub fn native_payload(
    grid: &SheetGrid,
    new_fragment: &str,
) -> Result<Option<Vec<u8>>, SheetsParseError> {
    let Some(compact) = grid.compact_json.as_deref() else {
        return Ok(None);
    };
    let new_grid = parse_sheets_html(new_fragment, None)?;
    let mut replacements: Vec<(CellKey, String)> = Vec::new();
    for cell in &grid.cells {
        if cell.kind == CellKind::Text && !cell.display_text.trim().is_empty() {
            let Some(new) = new_grid.cell(cell.row, cell.col) else {
                return Err(SheetsParseError("replacement cell missing"));
            };
            replacements.push(((cell.row, cell.col), new.display_text.clone()));
        }
    }
    let data: Value =
        serde_json::from_str(compact).map_err(|_| SheetsParseError("invalid compact JSON"))?;
    let updated = replace_compact_strings(&data, &replacements)?;
    Ok(Some(encode_custom_data(&[(
        CUSTOM_MIME.to_string(),
        updated.to_string(),
    )])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom_clipboard::{CUSTOM_MIME, compact_from_custom, decode_custom_data};
    use crate::html::parse_sheets_html;
    use crate::model::{CellKind, SheetCell};
    use serde_json::{Value, json};

    const MIXED: &str = include_str!("../tests/fixtures/mixed_2x3.html");
    const EMPTY_MIDDLE: &str = include_str!("../tests/fixtures/empty_middle_and_last.html");

    fn grid_of(html: &str) -> SheetGrid {
        parse_sheets_html(html, None).unwrap()
    }

    fn rep(items: &[(usize, usize, &str)]) -> Vec<(CellKey, String)> {
        items
            .iter()
            .map(|&(r, c, t)| ((r, c), t.to_string()))
            .collect()
    }

    fn rebuilt(grid: &SheetGrid, items: &[(usize, usize, &str)]) -> SheetGrid {
        parse_sheets_html(&build_replacement_html(grid, &rep(items)).unwrap(), None).unwrap()
    }

    fn compact_table() -> Value {
        json!({
            "1": 1, "2": [-3, 195, 203, 194],
            "3": {"1": [2, 2, 2, 2], "4": ["a", "b", "c\nd", "hello world"]},
            "4": [{"2": 4224, "10": 2, "15": "Arial"}], "5": [-5, 0],
            "8": ["=concat(\"hello\",\" world\")"], "9": [0],
            "12": [-5, 1], "13": [-5, 1], "15": {"1": 1, "2": 5}, "20": 5,
        })
    }

    /// A 1x1 text cell "a" whose kind comes from compact metadata (the HTML has no
    /// usable value attribute).
    fn one_text_cell(source: &str) -> SheetGrid {
        let data = json!({"2": [195], "3": {"1": [2], "4": ["a"]}, "15": {"1": 1, "2": 1}});
        parse_sheets_html(source, Some(&data)).unwrap()
    }

    fn rich_data() -> Value {
        json!({"2": [1219], "3": {"1": [2], "4": ["Title\nbody"]},
            "15": {"1": 1, "2": 1},
            "17": [{"1": [{"1": 0, "2": {"5": 1}}, {"1": 5}]}]})
    }

    #[test]
    fn build_with_no_replacements_is_byte_identical() {
        let grid = grid_of(MIXED);
        assert_eq!(build_replacement_html(&grid, &[]).unwrap(), MIXED);
    }

    #[test]
    fn only_target_cell_content_changes_and_reparse_matches() {
        let grid = grid_of(MIXED);
        let out = rebuilt(&grid, &[(0, 0, "Hello")]);
        assert_eq!(out.cell(0, 0).unwrap().display_text, "Hello");
        assert_eq!(out.cell(0, 0).unwrap().kind, CellKind::Text);
        let others = |g: &SheetGrid| -> Vec<SheetCell> {
            g.cells
                .iter()
                .filter(|c| (c.row, c.col) != (0, 0))
                .cloned()
                .collect()
        };
        assert_eq!(others(&out), others(&grid));
    }

    #[test]
    fn untouched_cells_and_target_cell_style_are_byte_identical() {
        let grid = grid_of(MIXED);
        let new_html = build_replacement_html(&grid, &rep(&[(1, 2, "Total")])).unwrap();
        let (start, end) = *grid.cell_spans.last().unwrap();
        let tail = &grid.html[end..];
        assert!(new_html.starts_with(&grid.html[..start]));
        assert!(new_html.ends_with(tail));
        let new_td = &new_html[start..new_html.len() - tail.len()];
        let old_td = &grid.html[start..end];
        let cut = |s: &str| s[..s.find(" data-sheets-value").unwrap()].to_string();
        assert_eq!(cut(old_td), cut(new_td));
        assert!(new_td.contains("background-color:#fff2cc"));
    }

    #[test]
    fn target_cell_value_attribute_is_a_string_value_with_new_text() {
        let grid = grid_of(MIXED);
        let new_html = build_replacement_html(&grid, &rep(&[(0, 0, "=SUM(A1)")])).unwrap();
        let (start, _) = grid.cell_spans[0];
        let open = &new_html[start..];
        let open = &open[..open.find('>').unwrap()];
        assert!(
            open.contains(
                r#"data-sheets-value="{&quot;1&quot;:2,&quot;2&quot;:&quot;=SUM(A1)&quot;}""#
            ),
            "{open}"
        );
    }

    #[test]
    fn translation_starting_with_equals_stays_text() {
        let out = rebuilt(&grid_of(MIXED), &[(0, 0, "=SUM(A1)")]);
        let cell = out.cell(0, 0).unwrap();
        assert_eq!(cell.kind, CellKind::Text);
        assert_eq!(cell.formula, None);
        assert_eq!(cell.display_text, "=SUM(A1)");
    }

    #[test]
    fn newline_quote_angle_bracket_ampersand_roundtrip() {
        let text = "a<b>\n\"q\" & c\tend";
        let out = rebuilt(&grid_of(MIXED), &[(0, 0, text)]);
        assert_eq!(out.cell(0, 0).unwrap().display_text, text);
        assert_eq!((out.rows, out.cols), (2, 3));
    }

    #[test]
    fn translation_for_formula_number_or_empty_cell_rejected() {
        let grid = grid_of(MIXED);
        for key in [(0, 1), (0, 2), (1, 0), (1, 1)] {
            assert_eq!(
                build_replacement_html(&grid, &[(key, "x".to_string())]),
                Err(PayloadError("not a text cell")),
                "{key:?}"
            );
        }
    }

    #[test]
    fn translation_for_unknown_position_rejected() {
        assert_eq!(
            build_replacement_html(&grid_of(MIXED), &rep(&[(9, 9, "x")])),
            Err(PayloadError("no such cell"))
        );
    }

    #[test]
    fn carriage_returns_in_translation_do_not_leave_stray_cr() {
        let out = rebuilt(&grid_of(MIXED), &[(0, 0, "a\r\nb\rc")]);
        assert_eq!(out.cell(0, 0).unwrap().display_text, "a\nb\nc");
    }

    #[test]
    fn existing_attribute_is_redumped_compact_and_new_one_uses_default_separators() {
        let grid = grid_of(MIXED);
        let out = build_replacement_html(&grid, &rep(&[(0, 0, "한 \"x\"")])).unwrap();
        assert!(
            out.contains(
                "data-sheets-value=\"{&quot;1&quot;:2,&quot;2&quot;:&quot;한 \\&quot;x\\&quot;&quot;}\">한 \"x\"</td>"
            ),
            "{out}"
        );
        // Cell without a value attribute: appended with ", " / ": " separators,
        // single quote escaped as &#x27;.
        let bare = "<google-sheets-html-origin><table><tr><td>a</td></tr></table>";
        let out = build_replacement_html(&one_text_cell(bare), &rep(&[(0, 0, "it's")])).unwrap();
        assert_eq!(
            out,
            "<google-sheets-html-origin><table><tr><td data-sheets-value=\"{&quot;1&quot;: 2, &quot;2&quot;: &quot;it&#x27;s&quot;}\">it's</td></tr></table>"
        );
    }

    #[test]
    fn multiline_becomes_br_and_content_is_escaped() {
        let bare = "<google-sheets-html-origin><table><tr><td><b>a</b></td></tr></table>";
        let out =
            build_replacement_html(&one_text_cell(bare), &rep(&[(0, 0, "x&y\n<z>")])).unwrap();
        assert!(
            out.ends_with(">x&amp;y<br>&lt;z&gt;</td></tr></table>"),
            "{out}"
        );
        assert!(!out.contains("<b>"));
    }

    #[test]
    fn value_attribute_with_invalid_or_non_object_json_is_rebuilt() {
        for attr in ["not json", "[1,2]"] {
            let src = format!(
                "<google-sheets-html-origin><table><tr><td data-sheets-value=\"{attr}\">a</td></tr></table>"
            );
            let out = build_replacement_html(&one_text_cell(&src), &rep(&[(0, 0, "b")])).unwrap();
            assert!(
                out.contains("data-sheets-value=\"{&quot;1&quot;:2,&quot;2&quot;:&quot;b&quot;}\""),
                "{out}"
            );
        }
    }

    #[test]
    fn gt_inside_quoted_attribute_does_not_end_open_tag() {
        let src = "<google-sheets-html-origin><table><tr><td title=\"a>b\" data-sheets-value='{\"1\":2,\"2\":\"a\"}'>a</td></tr></table>";
        let out = build_replacement_html(&grid_of(src), &rep(&[(0, 0, "z")])).unwrap();
        assert!(
            out.starts_with(
                "<google-sheets-html-origin><table><tr><td title=\"a>b\" data-sheets-value=\"{&quot;1&quot;:2,&quot;2&quot;:&quot;z&quot;}\">z</td>"
            ),
            "{out}"
        );
    }

    #[test]
    fn unterminated_open_tag_is_reported() {
        assert_eq!(
            new_td("<td title=\"a>b</td>", "x"),
            Err(PayloadError("unterminated <td> tag"))
        );
    }

    #[test]
    fn modern_paste_html_fallback_keeps_formula_and_literal_string_types() {
        let source = "<google-sheets-html-origin><table><tr><td>a</td><td>b</td>\
                      <td>c<br/>d</td><td>hello world</td><td></td></tr></table>";
        let grid = parse_sheets_html(source, Some(&compact_table())).unwrap();
        let output = build_replacement_html(&grid, &rep(&[(0, 0, "=literal")])).unwrap();
        // The formula cell gets its formula attribute back, escaped.
        assert!(
            output.contains(
                "<td data-sheets-formula=\"=concat(&quot;hello&quot;,&quot; world&quot;)\">hello world</td>"
            ),
            "{output}"
        );
        let parsed = parse_sheets_html(&output, None).unwrap();
        assert_eq!(parsed.cells[0].kind, CellKind::Text);
        assert_eq!(parsed.cells[0].display_text, "=literal");
        assert_eq!(
            parsed.cells[3].formula.as_deref(),
            Some("=concat(\"hello\",\" world\")")
        );
        // Without replacements nothing is touched even with compact data.
        assert_eq!(build_replacement_html(&grid, &[]).unwrap(), source);
    }

    #[test]
    fn existing_formula_attribute_is_not_duplicated() {
        let source = "<google-sheets-html-origin><table><tr><td>a</td><td>b</td>\
                      <td>c<br/>d</td><td data-sheets-formula=\"x\">hello world</td><td></td></tr></table>";
        let grid = parse_sheets_html(source, Some(&compact_table())).unwrap();
        let output = build_replacement_html(&grid, &rep(&[(0, 0, "z")])).unwrap();
        assert_eq!(output.matches("data-sheets-formula=").count(), 1);
    }

    #[test]
    fn e13_with_character_formatting_translates_to_plain_text() {
        let data = rich_data();
        let source = "<span data-sheets-root=\"1\"><b>Title</b><br/>body</span>";
        let grid = parse_sheets_html(source, Some(&data)).unwrap();
        let output = build_replacement_html(&grid, &rep(&[(0, 0, "제목\n본문")])).unwrap();
        let native =
            crate::replace_compact_strings(&data, &[((0, 0), "제목\n본문".to_string())]).unwrap();
        assert_eq!(native["2"], json!([195]));
        assert!(native.get("17").is_none());
        assert_eq!(data, rich_data());
        let after = parse_sheets_html(&output, Some(&native)).unwrap();
        assert_eq!(after.cells[0].display_text, "제목\n본문");
        assert!(!after.cells[0].has_rich_text);
        assert!(!output.contains("<b>"));
    }

    #[test]
    fn legacy_character_style_attribute_removed_with_nested_markup() {
        let source = "<google-sheets-html-origin><table><tr>\
            <td data-sheets-value='{\"1\":2,\"2\":\"hello\"}' data-sheets-textstyleruns=\"[]\">\
            <b>hello</b></td></tr></table>";
        let grid = grid_of(source);
        let output = build_replacement_html(&grid, &rep(&[(0, 0, "안녕")])).unwrap();
        assert!(!output.contains("textstyleruns"));
        let after = parse_sheets_html(&output, None).unwrap();
        assert_eq!(after.cells[0].display_text, "안녕");
        assert!(!after.cells[0].has_rich_text);
    }

    fn native_json(payload: &[u8]) -> Value {
        compact_from_custom(Some(payload)).unwrap().unwrap()
    }

    #[test]
    fn native_payload_none_without_compact() {
        let grid = grid_of(MIXED);
        assert_eq!(native_payload(&grid, MIXED), Ok(None));
    }

    #[test]
    fn native_paste_keeps_untranslated_whitespace_cell_formatting() {
        let data = json!({"2": [195, 1219], "3": {"1": [2, 2], "4": ["hello", " "]},
            "15": {"1": 1, "2": 2}, "17": [{"1": [{"1": 0, "2": {"5": 1}}]}]});
        let source = "<google-sheets-html-origin><table><tr><td>hello</td>\
                      <td><b> </b></td></tr></table>";
        let grid = parse_sheets_html(source, Some(&data)).unwrap();
        let new_html = build_replacement_html(&grid, &rep(&[(0, 0, "안녕")])).unwrap();
        let payload = native_payload(&grid, &new_html).unwrap().unwrap();
        let native = native_json(&payload);
        assert_eq!(native["3"]["4"], json!(["안녕", " "]));
        assert_eq!(native["17"], data["17"]);
        assert_eq!(native["2"], data["2"]);
        let original: Value = serde_json::from_str(grid.compact_json.as_deref().unwrap()).unwrap();
        assert_eq!(original, data);
        let map = decode_custom_data(&payload).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map[0].0, CUSTOM_MIME);
    }

    #[test]
    fn native_payload_replaces_every_nonblank_text_cell_and_drops_its_runs() {
        let data = json!({"2": [1219, 1219, 203], "3": {"1": [2, 2, 2], "4": ["a", "b", "c"]},
            "15": {"1": 1, "2": 3}, "8": ["=\"c\""], "9": [0],
            "17": [{"1": [{"1": 0, "2": {"5": 1}}]}, {"1": [{"1": 0, "2": {"6": 1}}]}]});
        let source =
            "<google-sheets-html-origin><table><tr><td>a</td><td>b</td><td>c</td></tr></table>";
        let grid = parse_sheets_html(source, Some(&data)).unwrap();
        // Only the first cell is translated; the second is rewritten with its own text.
        let new_html = build_replacement_html(&grid, &rep(&[(0, 0, "번역")])).unwrap();
        let payload = native_payload(&grid, &new_html).unwrap().unwrap();
        let native = native_json(&payload);
        assert_eq!(native["3"]["4"], json!(["번역", "b", "c"]));
        assert_eq!(native["2"], json!([195, 195, 203]));
        assert!(native.get("17").is_none());
        assert_eq!(native["8"], data["8"]);
    }

    #[test]
    fn native_payload_rejects_new_fragment_that_does_not_parse() {
        let data = rich_data();
        let grid = parse_sheets_html(
            "<span data-sheets-root=\"1\"><b>Title</b><br/>body</span>",
            Some(&data),
        )
        .unwrap();
        assert!(native_payload(&grid, "<p>nope</p>").is_err());
    }

    #[test]
    fn empty_middle_fixture_roundtrips_without_replacements() {
        let grid = grid_of(EMPTY_MIDDLE);
        assert_eq!(build_replacement_html(&grid, &[]).unwrap(), EMPTY_MIDDLE);
    }
}
