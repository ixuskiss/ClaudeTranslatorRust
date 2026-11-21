//! Parser for the HTML table Google Sheets puts on the clipboard, plus the rich-text detector.
//!
//! Dependency-free tokenizer (decision D5): start/end tags, self-closing tags, text, comments,
//! quoted attribute values that may contain `>`. Only numeric character references and
//! `&amp; &lt; &gt; &quot; &apos; &#39; &nbsp;` are decoded; unknown named references stay literal.

use std::collections::HashSet;

use serde_json::Value;

use crate::cf_html::extract_cf_html_fragment;
use crate::custom_clipboard::{CompactItem, compact_cells};
use crate::model::{CellKind, SheetCell, SheetGrid, SheetsParseError};

const ORIGIN_MARKER: &str = "google-sheets-html-origin";
const MAX_SPAN: usize = 10_000;
const MAX_SPAN_AREA: usize = 100_000;

enum Token<'a> {
    Start {
        name: String,
        attrs: Vec<(String, String)>,
        start: usize,
    },
    End {
        name: String,
        end: usize,
    },
    Text(&'a str),
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

fn find_from(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from > hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

fn find_ci(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
        .map(|i| i + from)
}

fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // Longest reference we recognise is short; limit the lookahead.
        let mut limit = rest.len().min(12);
        while !rest.is_char_boundary(limit) {
            limit -= 1;
        }
        let window = &rest[..limit];
        let decoded = window.find(';').and_then(|semi| {
            let name = &window[1..semi];
            let ch = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => '\u{a0}',
                _ => {
                    let digits = name.strip_prefix('#')?;
                    let code = match digits.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => digits.parse::<u32>().ok()?,
                    };
                    if !digits.bytes().all(|b| b.is_ascii_alphanumeric()) {
                        return None;
                    }
                    char::from_u32(code).unwrap_or('\u{fffd}')
                }
            };
            Some((ch, semi + 1))
        });
        match decoded {
            Some((ch, len)) => {
                out.push(ch);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parses the inside of a start tag from `pos` (just after the name). Returns attributes and
/// the index just past the closing `>`, or `None` when the tag is never closed.
fn parse_attrs(src: &str, mut pos: usize) -> Option<(Vec<(String, String)>, usize)> {
    let b = src.as_bytes();
    let mut attrs = Vec::new();
    loop {
        while pos < b.len() && (is_ws(b[pos]) || b[pos] == b'/') {
            pos += 1;
        }
        if pos >= b.len() {
            return None;
        }
        if b[pos] == b'>' {
            return Some((attrs, pos + 1));
        }
        let name_start = pos;
        while pos < b.len() && !is_ws(b[pos]) && !matches!(b[pos], b'=' | b'>' | b'/') {
            pos += 1;
        }
        if pos == name_start {
            // Stray '=' etc.
            pos += 1;
            continue;
        }
        let name = src[name_start..pos].to_ascii_lowercase();
        while pos < b.len() && is_ws(b[pos]) {
            pos += 1;
        }
        let mut value = String::new();
        if pos < b.len() && b[pos] == b'=' {
            pos += 1;
            while pos < b.len() && is_ws(b[pos]) {
                pos += 1;
            }
            if pos >= b.len() {
                return None;
            }
            if b[pos] == b'"' || b[pos] == b'\'' {
                let quote = b[pos];
                let vs = pos + 1;
                let ve = b[vs..].iter().position(|&c| c == quote)? + vs;
                value = decode_entities(&src[vs..ve]);
                pos = ve + 1;
            } else {
                let vs = pos;
                while pos < b.len() && !is_ws(b[pos]) && b[pos] != b'>' {
                    pos += 1;
                }
                value = decode_entities(&src[vs..pos]);
            }
        }
        attrs.push((name, value));
    }
}

fn tokenize(src: &str) -> Vec<Token<'_>> {
    let b = src.as_bytes();
    let mut tokens = Vec::new();
    let mut text_start = 0;
    let mut pos = 0;
    while pos < b.len() {
        if b[pos] != b'<' {
            pos += 1;
            continue;
        }
        let next = b.get(pos + 1).copied().unwrap_or(0);
        // Where the construct ends, and what it produced.
        let mut produced: Option<(Token<'_>, usize)> = None;
        let mut skipped_to: Option<usize> = None;
        if next.is_ascii_alphabetic() {
            let name_end = b[pos + 1..]
                .iter()
                .position(|&c| is_ws(c) || c == b'>' || c == b'/')
                .map_or(b.len(), |i| i + pos + 1);
            if let Some((attrs, end)) = parse_attrs(src, name_end) {
                let name = src[pos + 1..name_end].to_ascii_lowercase();
                produced = Some((
                    Token::Start {
                        name,
                        attrs,
                        start: pos,
                    },
                    end,
                ));
            }
        } else if next == b'/' && b.get(pos + 2).is_some_and(u8::is_ascii_alphabetic) {
            let name_end = b[pos + 2..]
                .iter()
                .position(|&c| is_ws(c) || c == b'>' || c == b'/')
                .map_or(b.len(), |i| i + pos + 2);
            if let Some(gt) = find_from(b, name_end, b">") {
                let name = src[pos + 2..name_end].to_ascii_lowercase();
                produced = Some((Token::End { name, end: gt + 1 }, gt + 1));
            }
        } else if src[pos..].starts_with("<!--") {
            skipped_to = Some(find_from(b, pos + 4, b"-->").map_or(b.len(), |i| i + 3));
        } else if next == b'!' || next == b'?' || next == b'/' {
            skipped_to = Some(find_from(b, pos, b">").map_or(b.len(), |i| i + 1));
        }
        if let Some((token, end)) = produced {
            if text_start < pos {
                tokens.push(Token::Text(&src[text_start..pos]));
            }
            let raw_text = match &token {
                Token::Start { name, .. } if name == "style" || name == "script" => {
                    Some(name.clone())
                }
                _ => None,
            };
            tokens.push(token);
            pos = end;
            text_start = pos;
            if let Some(name) = raw_text {
                let close = format!("</{name}");
                let stop = find_ci(b, pos, close.as_bytes()).unwrap_or(b.len());
                if text_start < stop {
                    tokens.push(Token::Text(&src[text_start..stop]));
                }
                pos = stop;
                text_start = stop;
            }
        } else if let Some(end) = skipped_to {
            if text_start < pos {
                tokens.push(Token::Text(&src[text_start..pos]));
            }
            pos = end;
            text_start = end;
        } else {
            pos += 1;
        }
    }
    if text_start < b.len() {
        tokens.push(Token::Text(&src[text_start..]));
    }
    tokens
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

struct Current {
    row: usize,
    col: usize,
    row_span: usize,
    col_span: usize,
    attrs: Vec<(String, String)>,
    text: String,
    rich: bool,
    link: bool,
    start: usize,
}

struct GridParse {
    tables: usize,
    rows: usize,
    max_cols: usize,
    cells: Vec<SheetCell>,
    spans: Vec<(usize, usize)>,
}

fn parse_span(attrs: &[(String, String)], name: &str) -> Result<usize, SheetsParseError> {
    let raw = attr(attrs, name).unwrap_or("");
    let raw = if raw.is_empty() { "1" } else { raw };
    let n: i64 = raw
        .trim()
        .parse()
        .map_err(|_| SheetsParseError("invalid span attribute"))?;
    let n = usize::try_from(n.max(1)).map_err(|_| SheetsParseError("invalid span attribute"))?;
    if n > MAX_SPAN {
        return Err(SheetsParseError("invalid span attribute"));
    }
    Ok(n)
}

/// A `<div>` carrying only an overflow-style `style` attribute is not character formatting.
fn is_plain_overflow_div(attrs: &[(String, String)]) -> bool {
    attrs
        .iter()
        .all(|(k, v)| k == "style" && !style_has_character_formatting(v))
}

fn style_has_character_formatting(style: &str) -> bool {
    let lower = style.to_ascii_lowercase();
    ["font", "color", "text-decoration", "vertical-align"]
        .iter()
        .any(|word| {
            lower.match_indices(word).any(|(i, _)| {
                let rest = lower[i + word.len()..].trim_start();
                rest.starts_with('-') || rest.starts_with(':')
            })
        })
}

fn grid_parse(source: &str) -> Result<GridParse, SheetsParseError> {
    let mut p = GridParse {
        tables: 0,
        rows: 0,
        max_cols: 0,
        cells: Vec::new(),
        spans: Vec::new(),
    };
    let mut row: Option<usize> = None;
    let mut occupied: HashSet<(usize, usize)> = HashSet::new();
    let mut current: Option<Current> = None;
    for token in tokenize(source) {
        match token {
            Token::Start { name, attrs, start } => match name.as_str() {
                "table" => p.tables += 1,
                "tr" => row = Some(row.map_or(0, |r| r + 1)),
                "td" | "th" if row.is_some() => {
                    let r = row.unwrap_or(0);
                    let mut col = 0;
                    while occupied.contains(&(r, col)) {
                        col += 1;
                    }
                    let row_span = parse_span(&attrs, "rowspan")?;
                    let col_span = parse_span(&attrs, "colspan")?;
                    if row_span * col_span > MAX_SPAN_AREA {
                        return Err(SheetsParseError("invalid span attribute"));
                    }
                    for rr in r..r + row_span {
                        for cc in col..col + col_span {
                            occupied.insert((rr, cc));
                        }
                    }
                    p.max_cols = p.max_cols.max(col + col_span);
                    let link = attrs
                        .iter()
                        .any(|(k, _)| k.starts_with("data-sheets-hyperlink"));
                    current = Some(Current {
                        row: r,
                        col,
                        row_span,
                        col_span,
                        attrs,
                        text: String::new(),
                        rich: false,
                        link,
                        start,
                    });
                }
                _ => {
                    if let Some(cur) = current.as_mut() {
                        match name.as_str() {
                            "br" => cur.text.push('\n'),
                            "a" => cur.link = true,
                            "div" if is_plain_overflow_div(&attrs) => {}
                            _ => cur.rich = true,
                        }
                    }
                }
            },
            Token::Text(raw) => {
                if let Some(cur) = current.as_mut() {
                    cur.text.push_str(&decode_entities(raw));
                }
            }
            Token::End { name, end } => {
                if (name == "td" || name == "th")
                    && let Some(cur) = current.take()
                {
                    p.spans.push((cur.start, end));
                    p.cells.push(build_cell(cur));
                }
            }
        }
    }
    p.rows = row.map_or(0, |r| r + 1);
    Ok(p)
}

fn load_json_object(raw: Option<&str>) -> Option<serde_json::Map<String, Value>> {
    match serde_json::from_str::<Value>(raw?).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn build_cell(cur: Current) -> SheetCell {
    let formula = attr(&cur.attrs, "data-sheets-formula").map(str::to_string);
    let value = load_json_object(attr(&cur.attrs, "data-sheets-value"));
    let kind = if formula.is_some() {
        CellKind::Formula
    } else if let Some(value) = value {
        match value.get("1").and_then(Value::as_i64) {
            Some(2) if cur.text.is_empty() => CellKind::Empty,
            Some(2) => CellKind::Text,
            Some(3) => {
                let format = load_json_object(attr(&cur.attrs, "data-sheets-numberformat"));
                let is_date = format
                    .and_then(|f| f.get("1").and_then(Value::as_i64))
                    .is_some_and(|t| matches!(t, 5..=7));
                if is_date {
                    CellKind::Date
                } else {
                    CellKind::Number
                }
            }
            Some(4) => CellKind::Boolean,
            _ => CellKind::Other,
        }
    } else if cur.text.trim().is_empty() {
        CellKind::Empty
    } else {
        CellKind::Other
    };
    let has_rich_text = cur.rich
        || cur
            .attrs
            .iter()
            .any(|(k, _)| k == "data-sheets-textstyleruns");
    SheetCell {
        row: cur.row,
        col: cur.col,
        kind,
        display_text: cur.text,
        has_rich_text,
        has_link: cur.link,
        row_span: cur.row_span,
        col_span: cur.col_span,
        formula,
    }
}

/// S-ref §2.6 step 2: a single copied cell is a styled span, not a table.
fn single_cell_table(fragment: &str) -> Option<String> {
    let mut rest = fragment;
    if rest.starts_with("<style") && rest[6..].chars().next().is_none_or(|c| !is_word(c)) {
        let end = rest.find("</style>")? + "</style>".len();
        rest = &rest[end..];
    }
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("<span")?;
    let gt = rest.find('>')?;
    let attrs = &rest[..gt];
    let content = rest[gt + 1..].trim_end().strip_suffix("</span>")?;
    let has_root = ["\"", "'"].iter().any(|q1| {
        ["\"", "'"].iter().any(|q2| {
            let needle = format!("data-sheets-root={q1}1{q2}");
            attrs.match_indices(&needle).any(|(i, _)| {
                // `\b` before `d`: the previous char must not be a word character.
                attrs[..i].chars().next_back().is_some_and(|c| !is_word(c))
            })
        })
    });
    has_root.then(|| format!("<{ORIGIN_MARKER}><table><tr><td{attrs}>{content}</td></tr></table>"))
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

pub fn parse_sheets_html(
    fragment: &str,
    compact: Option<&Value>,
) -> Result<SheetGrid, SheetsParseError> {
    let metadata = match compact {
        Some(data) => Some(
            compact_cells(data).map_err(|_| SheetsParseError("unsupported compact metadata"))?,
        ),
        None => None,
    };
    let mut html = fragment.to_string();
    if !html.contains(ORIGIN_MARKER)
        && metadata
            .as_ref()
            .is_some_and(|(rows, cols, _)| (*rows, *cols) == (1, 1))
        && let Some(synthetic) = single_cell_table(fragment)
    {
        html = synthetic;
    }
    if !html.contains(ORIGIN_MARKER) {
        return Err(SheetsParseError("not Google Sheets clipboard HTML"));
    }
    let mut parsed = grid_parse(&html)?;
    if parsed.tables != 1 {
        return Err(SheetsParseError("expected exactly one table"));
    }
    if parsed.cells.is_empty() {
        return Err(SheetsParseError("table has no cells"));
    }
    if let Some((rows, cols, items)) = metadata {
        if (parsed.rows, parsed.max_cols, parsed.cells.len()) != (rows, cols, items.len()) {
            return Err(SheetsParseError("HTML and compact dimensions differ"));
        }
        for (cell, item) in parsed.cells.iter_mut().zip(&items) {
            apply_compact(cell, item)?;
        }
    }
    Ok(SheetGrid {
        rows: parsed.rows,
        cols: parsed.max_cols,
        cells: parsed.cells,
        html,
        cell_spans: parsed.spans,
        compact_json: compact.map(Value::to_string),
    })
}

fn apply_compact(cell: &mut SheetCell, item: &CompactItem) -> Result<(), SheetsParseError> {
    if cell.display_text != item.text.as_deref().unwrap_or("") {
        return Err(SheetsParseError("HTML and compact values differ"));
    }
    cell.kind = if item.formula.is_some() {
        CellKind::Formula
    } else if item.string_index.is_some() {
        CellKind::Text
    } else {
        CellKind::Empty
    };
    cell.formula = item.formula.clone();
    cell.has_rich_text |= item.rich_index.is_some();
    Ok(())
}

/// Whether copied Sheets HTML shows per-character formatting or partial links (`Some(true)`),
/// plain cells (`Some(false)`), or cannot tell (`None`).
pub fn html_rich_text_state(clipboard_html: &str) -> Option<bool> {
    if clipboard_html.is_empty() {
        return None;
    }
    if clipboard_html.contains(ORIGIN_MARKER) {
        return Some(
            clipboard_html.contains("data-sheets-textstyleruns")
                || clipboard_html.contains("data-sheets-hyperlink")
                || clipboard_html.contains("<a "),
        );
    }
    let extracted;
    let fragment = if clipboard_html.starts_with("Version:") {
        extracted = extract_cf_html_fragment(clipboard_html).ok()?;
        extracted.as_str()
    } else {
        clipboard_html
    };
    let (mut roots, mut in_root, mut closed, mut rich) = (0, false, false, false);
    for token in tokenize(fragment) {
        match token {
            Token::Start { name, attrs, .. } => {
                if name == "span" && attr(&attrs, "data-sheets-root") == Some("1") {
                    roots += 1;
                    in_root = true;
                } else if in_root && name != "br" {
                    rich = true;
                }
                if in_root
                    && attrs.iter().any(|(k, _)| {
                        k.starts_with("data-sheets-hyperlink")
                            || k.starts_with("data-sheets-textstyleruns")
                    })
                {
                    rich = true;
                }
            }
            Token::End { name, .. } => {
                if in_root && name == "span" {
                    closed = true;
                }
            }
            Token::Text(_) => {}
        }
    }
    (roots == 1 && closed).then_some(rich)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cf_html::build_cf_html;
    use serde_json::json;

    const MIXED: &str = include_str!("../tests/fixtures/mixed_2x3.html");
    const EMPTY_MIDDLE: &str = include_str!("../tests/fixtures/empty_middle_and_last.html");
    const MULTILINE: &str = include_str!("../tests/fixtures/multiline_cell.html");
    const RICH_LINK: &str = include_str!("../tests/fixtures/rich_and_link.html");
    const MERGED: &str = include_str!("../tests/fixtures/merged.html");

    fn table() -> Value {
        json!({
            "1": 1, "2": [-3, 195, 203, 194],
            "3": {"1": [2, 2, 2, 2], "4": ["a", "b", "c\nd", "hello world"]},
            "4": [{"2": 4224, "10": 2, "15": "Arial"}], "5": [-5, 0],
            "8": ["=concat(\"hello\",\" world\")"], "9": [0],
            "12": [-5, 1], "13": [-5, 1], "15": {"1": 1, "2": 5}, "20": 5,
        })
    }

    fn quote_prefixed_table() -> Value {
        json!({
            "1": 1, "2": [-2, 195],
            "3": {"1": [4, 2], "4": ["aaa", "aaa"]},
            "4": [{"2": 4224, "10": 2, "15": "Arial"}], "5": [-2, 0],
            "12": [-2, 1], "13": [-2, 1], "15": {"1": 2, "2": 1}, "20": 2,
        })
    }

    const MODERN: &str = "<google-sheets-html-origin><table><tr><td>a</td><td>b</td>\
        <td>c<br/>d</td><td>hello world</td><td></td></tr></table>";

    fn err(phrase: &'static str) -> Result<SheetGrid, SheetsParseError> {
        Err(SheetsParseError(phrase))
    }

    #[test]
    fn parse_mixed_types_keeps_positions() {
        let grid = parse_sheets_html(MIXED, None).unwrap();
        assert_eq!((grid.rows, grid.cols), (2, 3));
        let kinds: Vec<_> = grid.cells.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            [
                CellKind::Text,
                CellKind::Formula,
                CellKind::Number,
                CellKind::Date,
                CellKind::Empty,
                CellKind::Text
            ]
        );
    }

    #[test]
    fn formula_cell_keeps_formula_text() {
        let grid = parse_sheets_html(MIXED, None).unwrap();
        assert_eq!(
            grid.cell(0, 1).unwrap().formula.as_deref(),
            Some("=R[0]C[-1]&\"!\"")
        );
    }

    #[test]
    fn trailing_empty_cells_do_not_shift_grid() {
        let grid = parse_sheets_html(EMPTY_MIDDLE, None).unwrap();
        assert_eq!(grid.cell(2, 2).unwrap().kind, CellKind::Empty);
        assert_eq!((grid.rows, grid.cols), (3, 3));
        assert_eq!(grid.cell(1, 1).unwrap().display_text, "b");
    }

    #[test]
    fn newline_tab_quote_stay_inside_one_cell() {
        let grid = parse_sheets_html(MULTILINE, None).unwrap();
        assert_eq!(
            grid.cell(0, 0).unwrap().display_text,
            "첫 줄\n둘째\t\"인용\""
        );
    }

    #[test]
    fn rich_text_and_link_flags() {
        let grid = parse_sheets_html(RICH_LINK, None).unwrap();
        assert!(grid.cell(0, 0).unwrap().has_rich_text);
        assert!(grid.cell(0, 1).unwrap().has_link);
    }

    #[test]
    fn merged_cell_has_span_and_occupies_columns() {
        let grid = parse_sheets_html(MERGED, None).unwrap();
        assert_eq!(grid.cell(0, 0).unwrap().col_span, 2);
        assert_eq!((grid.rows, grid.cols), (2, 3));
        assert_eq!(grid.cell(0, 2).unwrap().display_text, "c");
    }

    #[test]
    fn cell_spans_point_at_each_td() {
        let grid = parse_sheets_html(MIXED, None).unwrap();
        let (s, e) = grid.cell_spans[0];
        assert!(grid.html[s..e].starts_with("<td") && grid.html[s..e].ends_with("</td>"));
        assert!(grid.html[s..e].contains("안녕하세요"));
    }

    #[test]
    fn cell_spans_are_byte_offsets_of_each_td() {
        let grid = parse_sheets_html(MIXED, None).unwrap();
        assert_eq!(grid.cell_spans.len(), grid.cells.len());
        for ((s, e), cell) in grid.cell_spans.iter().zip(&grid.cells) {
            let td = &grid.html[*s..*e];
            assert!(td.starts_with("<td ") && td.ends_with("</td>"), "{td}");
            assert_eq!(td.matches("<td").count(), 1);
            if !cell.display_text.is_empty() {
                assert!(td.contains(&cell.display_text));
            }
        }
    }

    #[test]
    fn non_sheets_html_rejected() {
        assert_eq!(
            parse_sheets_html("<table><tr><td>x</td></tr></table>", None),
            err("not Google Sheets clipboard HTML")
        );
    }

    #[test]
    fn cf_html_roundtrip_through_parser() {
        let wrapped = build_cf_html(MIXED);
        let fragment = extract_cf_html_fragment(&wrapped).unwrap();
        assert_eq!(fragment, MIXED);
        assert!(parse_sheets_html(&fragment, None).is_ok());
    }

    #[test]
    fn html_rich_text_state_true_when_style_runs_or_links_present() {
        assert_eq!(html_rich_text_state(RICH_LINK), Some(true));
    }

    #[test]
    fn html_rich_text_state_false_for_plain_sheets_html() {
        assert_eq!(html_rich_text_state(EMPTY_MIDDLE), Some(false));
    }

    #[test]
    fn html_rich_text_state_unknown_when_not_sheets_html() {
        assert_eq!(html_rich_text_state(""), None);
        assert_eq!(html_rich_text_state("<b>bold</b> not from sheets"), None);
    }

    #[test]
    fn observed_plain_editor_span_has_uniform_formatting() {
        let fragment = "<span style=\"font-size:10pt;font-family:Arial;font-style:normal;\" \
            data-sheets-root=\"1\">안녕하세요.</span>";
        assert_eq!(html_rich_text_state(&build_cf_html(fragment)), Some(false));
    }

    #[test]
    fn editor_span_with_character_formatting_or_link_is_rich() {
        for content in [
            "<b>bold</b> plain",
            "<span style=\"color:red\">red</span> plain",
            "<a href=\"https://example.com\">link</a>",
        ] {
            let html = format!("<span data-sheets-root=\"1\">{content}</span>");
            assert_eq!(html_rich_text_state(&html), Some(true), "{content}");
        }
    }

    #[test]
    fn modern_html_uses_compact_types_instead_of_guessing_display_text() {
        let data = table();
        let grid = parse_sheets_html(MODERN, Some(&data)).unwrap();
        let kinds: Vec<_> = grid.cells.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            [
                CellKind::Text,
                CellKind::Text,
                CellKind::Text,
                CellKind::Formula,
                CellKind::Empty
            ]
        );
        assert_eq!(
            grid.cells[3].formula.as_deref(),
            Some("=concat(\"hello\",\" world\")")
        );
        assert_eq!(grid.html, MODERN);
        let back: Value = serde_json::from_str(grid.compact_json.as_deref().unwrap()).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn compact_html_disagreement_is_rejected() {
        let html = "<google-sheets-html-origin><table><tr><td>different</td></tr></table>";
        assert_eq!(
            parse_sheets_html(html, Some(&table())),
            err("HTML and compact dimensions differ")
        );
        let same_shape = "<google-sheets-html-origin><table><tr><td>x</td><td>b</td>\
            <td>c<br/>d</td><td>hello world</td><td></td></tr></table>";
        assert_eq!(
            parse_sheets_html(same_shape, Some(&table())),
            err("HTML and compact values differ")
        );
    }

    #[test]
    fn single_cell_span_is_supported_with_compact_metadata() {
        let data = json!({"2": [195], "3": {"1": [2], "4": ["hello"]}, "15": {"1": 1, "2": 1}});
        let grid = parse_sheets_html(
            "<span style=\"font-size:10pt\" data-sheets-root=\"1\">hello</span>",
            Some(&data),
        )
        .unwrap();
        assert_eq!(grid.cells[0].kind, CellKind::Text);
        assert_eq!(grid.cells[0].display_text, "hello");
        assert!(grid.html.contains("style=\"font-size:10pt\""));
    }

    #[test]
    fn single_cell_span_becomes_synthetic_table() {
        let data = json!({"2": [195], "3": {"1": [2], "4": ["hello"]}, "15": {"1": 1, "2": 1}});
        let fragment =
            "<style>td{}</style>\n<span style=\"x\" data-sheets-root=\"1\">hello</span>\n";
        let grid = parse_sheets_html(fragment, Some(&data)).unwrap();
        assert_eq!(
            grid.html,
            "<google-sheets-html-origin><table><tr><td style=\"x\" \
             data-sheets-root=\"1\">hello</td></tr></table>"
        );
        // Without compact metadata the span is not a cell selection.
        assert_eq!(
            parse_sheets_html(fragment, None),
            err("not Google Sheets clipboard HTML")
        );
    }

    #[test]
    fn modern_nested_character_formatting_is_detected() {
        let source = "<google-sheets-html-origin><table><tr><td><b>a</b></td><td>b</td>\
            <td>c<br/>d</td><td>hello world</td><td></td></tr></table>";
        let grid = parse_sheets_html(source, Some(&table())).unwrap();
        assert!(grid.cells[0].has_rich_text);
        assert!(!grid.cells[1].has_rich_text);
    }

    #[test]
    fn quote_prefixed_text_cell_is_text() {
        let grid = parse_sheets_html(
            "<google-sheets-html-origin><table><tr><td>aaa</td></tr><tr><td>aaa</td></tr></table>",
            Some(&quote_prefixed_table()),
        )
        .unwrap();
        let got: Vec<_> = grid
            .cells
            .iter()
            .map(|c| (c.kind, c.display_text.as_str()))
            .collect();
        assert_eq!(got, [(CellKind::Text, "aaa"), (CellKind::Text, "aaa")]);
    }

    #[test]
    fn compact_number_type_rejects_capture() {
        let mut data = table();
        data["3"]["1"] = json!([3, 2, 2, 2]);
        assert_eq!(
            parse_sheets_html(MODERN, Some(&data)),
            err("unsupported compact metadata")
        );
    }

    #[test]
    fn unknown_named_entity_kept_literally() {
        let html = "<google-sheets-html-origin><table><tr>\
            <td>&copy; &amp; &lt;b&gt; &#65;&#x42; &nbsp;|</td></tr></table>";
        let grid = parse_sheets_html(html, None).unwrap();
        assert_eq!(grid.cells[0].display_text, "&copy; & <b> AB \u{a0}|");
    }

    #[test]
    fn plain_overflow_div_is_not_rich() {
        let wrap = |inner: &str| {
            format!("<google-sheets-html-origin><table><tr><td>{inner}</td></tr></table>")
        };
        let plain = wrap("<div style=\"overflow:hidden;max-height:10px\">x</div>");
        assert!(!parse_sheets_html(&plain, None).unwrap().cells[0].has_rich_text);
        for inner in [
            "<div style=\"font-weight:bold\">x</div>",
            "<div style=\"color : red\">x</div>",
            "<div class=\"k\">x</div>",
            "<span>x</span>",
        ] {
            let grid = parse_sheets_html(&wrap(inner), None).unwrap();
            assert!(grid.cells[0].has_rich_text, "{inner}");
        }
    }

    #[test]
    fn quoted_gt_in_attribute_and_comments_are_handled() {
        let html = "<google-sheets-html-origin><table><tr><!-- <td>no</td> -->\
            <td title=\"a>b\">x</td></tr></table>";
        let grid = parse_sheets_html(html, None).unwrap();
        assert_eq!(grid.cells.len(), 1);
        assert_eq!(grid.cells[0].display_text, "x");
    }

    #[test]
    fn table_count_and_empty_table_errors() {
        let two = "<google-sheets-html-origin><table><tr><td>a</td></tr></table><table></table>";
        assert_eq!(
            parse_sheets_html(two, None),
            err("expected exactly one table")
        );
        let none = "<google-sheets-html-origin><table><tr></tr></table>";
        assert_eq!(parse_sheets_html(none, None), err("table has no cells"));
    }
}
