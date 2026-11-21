//! Range support checks.

use crate::model::{CellKey, CellKind, SheetCell, SheetGrid, SheetSelection};
use crate::outcome::UnsupportedReason;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetsMode {
    /// Alt+Alt, Ctrl+Alt+Alt.
    Replace,
    /// Ctrl+Ctrl, Alt+Ctrl+Ctrl.
    Explain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParsedAddress {
    /// `(rows, cols)`.
    Size(usize, usize),
    Unsupported(UnsupportedReason),
}

/// One corner of a range address.
enum Corner {
    /// `A1`: 0-based `(row, col)`.
    Cell(usize, usize),
    /// `A`
    Col,
    /// `3`
    Row,
}

/// Removes a leading `'quoted ''name'''!` or `name!` sheet prefix.
fn strip_sheet_prefix(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix('\'') {
        let mut chars = rest.char_indices();
        while let Some((i, c)) = chars.next() {
            if c != '\'' {
                continue;
            }
            match rest[i + 1..].chars().next() {
                Some('\'') => {
                    chars.next();
                }
                Some('!') => return &rest[i + 2..],
                _ => break,
            }
        }
    }
    match s.find('!') {
        Some(i) if i > 0 => &s[i + 1..],
        _ => s,
    }
}

/// Column letters (1-3 ASCII letters, case-insensitive) to a 0-based index.
fn column_index(letters: &str) -> Option<usize> {
    if letters.is_empty() || letters.len() > 3 || !letters.bytes().all(|b| b.is_ascii_alphabetic())
    {
        return None;
    }
    let n = letters.bytes().fold(0usize, |acc, b| {
        acc * 26 + usize::from(b.to_ascii_uppercase() - b'A' + 1)
    });
    Some(n - 1)
}

fn parse_corner(s: &str) -> Option<Corner> {
    let split = s.find(|c: char| c.is_ascii_digit()).unwrap_or(s.len());
    let (letters, digits) = s.split_at(split);
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let row = (!digits.is_empty()).then(|| digits.parse::<usize>().unwrap_or(usize::MAX));
    match (letters.is_empty(), row) {
        (true, Some(_)) => Some(Corner::Row),
        (true, None) => None,
        (false, None) => column_index(letters).map(|_| Corner::Col),
        (false, Some(r)) => column_index(letters).map(|c| Corner::Cell(r.saturating_sub(1), c)),
    }
}

pub fn parse_range_address(address: &str) -> ParsedAddress {
    use ParsedAddress::Unsupported;
    let trimmed = address.trim();
    let body = strip_sheet_prefix(trimmed).trim();
    if body.is_empty() {
        return Unsupported(UnsupportedReason::UnidentifiedTarget);
    }
    if body.contains(',') {
        return Unsupported(UnsupportedReason::MultipleRanges);
    }
    let unidentified = Unsupported(UnsupportedReason::UnidentifiedTarget);
    let whole = Unsupported(UnsupportedReason::WholeRowOrColumn);
    let Some((a, b)) = body.split_once(':') else {
        return match parse_corner(body) {
            Some(Corner::Cell(..)) => ParsedAddress::Size(1, 1),
            _ => unidentified,
        };
    };
    match (parse_corner(a), parse_corner(b)) {
        (Some(Corner::Cell(r1, c1)), Some(Corner::Cell(r2, c2))) => {
            ParsedAddress::Size(r1.abs_diff(r2) + 1, c1.abs_diff(c2) + 1)
        }
        // A:A, 3:3, and open-ended A1:B / A:B1.
        (Some(Corner::Col), Some(Corner::Col))
        | (Some(Corner::Row), Some(Corner::Row))
        | (Some(Corner::Cell(..)), Some(Corner::Col))
        | (Some(Corner::Col), Some(Corner::Cell(..))) => whole,
        _ => unidentified,
    }
}

/// First applicable reason a selection cannot be processed in `mode`, or `None` if supported.
pub fn check_range_support(
    sel: &SheetSelection,
    mode: SheetsMode,
    max_range_cells: usize,
) -> Option<UnsupportedReason> {
    let Some(address) = sel.location.range_address.as_deref() else {
        return Some(UnsupportedReason::UnidentifiedTarget);
    };
    let (rows, cols) = match parse_range_address(address) {
        ParsedAddress::Unsupported(reason) => return Some(reason),
        ParsedAddress::Size(r, c) => (r, c),
    };
    let grid = &sel.grid;
    if (rows, cols) != (grid.rows, grid.cols) {
        // A smaller grid means rows were dropped from the copy.
        return Some(if grid.rows <= rows && grid.cols <= cols {
            UnsupportedReason::HiddenOrFiltered
        } else {
            UnsupportedReason::UnidentifiedTarget
        });
    }
    if sel.hidden_or_filtered != Some(false) {
        return Some(UnsupportedReason::HiddenOrFiltered);
    }
    if grid.cells.iter().any(|c| c.row_span > 1 || c.col_span > 1) {
        return Some(UnsupportedReason::MergedCells);
    }
    if grid.rows.saturating_mul(grid.cols) > max_range_cells {
        return Some(UnsupportedReason::RangeTooLarge);
    }
    if mode == SheetsMode::Replace {
        if grid.cells.iter().any(|c| c.kind == CellKind::Other) {
            return Some(UnsupportedReason::UnknownCellType);
        }
        if grid
            .cells
            .iter()
            .any(|c| c.kind == CellKind::Text && c.has_link)
        {
            return Some(UnsupportedReason::RichTextOrLink);
        }
    }
    None
}

/// Text cells with non-blank text, sorted by `(row, col)`.
pub fn translatable_cells(grid: &SheetGrid) -> Vec<(CellKey, String)> {
    let mut cells: Vec<&SheetCell> = grid
        .cells
        .iter()
        .filter(|c| c.kind == CellKind::Text && !c.display_text.trim().is_empty())
        .collect();
    cells.sort_by_key(|c| (c.row, c.col));
    cells
        .into_iter()
        .map(|c| ((c.row, c.col), c.display_text.clone()))
        .collect()
}

/// 0-based column index to letters: `A..Z, AA..`.
pub fn column_letters(index: usize) -> String {
    let mut n = index + 1;
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push(b'A' + (n % 26) as u8);
        n /= 26;
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
}

/// 0-based `(row, col)` of the first corner; `(0, 0)` when unknown.
pub fn range_origin(address: Option<&str>) -> (usize, usize) {
    let Some(address) = address else {
        return (0, 0);
    };
    let body = strip_sheet_prefix(address.trim());
    let first = body.split(':').next().unwrap_or("").trim();
    match parse_corner(first) {
        Some(Corner::Cell(r, c)) => (r, c),
        _ => (0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SheetLocation;

    fn cell(row: usize, col: usize, kind: CellKind, text: &str) -> SheetCell {
        SheetCell {
            row,
            col,
            kind,
            display_text: text.into(),
            has_rich_text: false,
            has_link: false,
            row_span: 1,
            col_span: 1,
            formula: None,
        }
    }

    fn text_cell(row: usize, col: usize) -> SheetCell {
        cell(row, col, CellKind::Text, "a")
    }

    fn grid(rows: usize, cols: usize, cells: Vec<SheetCell>) -> SheetGrid {
        SheetGrid {
            rows,
            cols,
            cells,
            html: String::new(),
            cell_spans: vec![],
            compact_json: None,
        }
    }

    fn selection(
        address: Option<&str>,
        hidden: Option<bool>,
        cells: Option<Vec<SheetCell>>,
        rows: usize,
        cols: usize,
    ) -> SheetSelection {
        let cells = cells.unwrap_or_else(|| {
            (0..rows)
                .flat_map(|r| (0..cols).map(move |c| text_cell(r, c)))
                .collect()
        });
        SheetSelection {
            location: SheetLocation {
                document_id: "doc".into(),
                gid: Some("0".into()),
                range_address: address.map(String::from),
            },
            grid: grid(rows, cols, cells),
            hidden_or_filtered: hidden,
        }
    }

    const MODES: [SheetsMode; 2] = [SheetsMode::Replace, SheetsMode::Explain];

    #[test]
    fn parse_range_address_table() {
        use ParsedAddress::*;
        use UnsupportedReason as U;
        let cases = [
            ("B2", Size(1, 1)),
            ("A1:C2", Size(2, 3)),
            ("C2:A1", Size(2, 3)),
            ("'시트 1'!A1:B2", Size(2, 2)),
            ("A:A", Unsupported(U::WholeRowOrColumn)),
            ("3:3", Unsupported(U::WholeRowOrColumn)),
            ("A1:A", Unsupported(U::WholeRowOrColumn)),
            ("A:B1", Unsupported(U::WholeRowOrColumn)),
            ("A1:B2,D4", Unsupported(U::MultipleRanges)),
            ("", Unsupported(U::UnidentifiedTarget)),
            ("not an address", Unsupported(U::UnidentifiedTarget)),
        ];
        for (addr, expected) in cases {
            assert_eq!(parse_range_address(addr), expected, "{addr}");
        }
    }

    #[test]
    fn parse_range_address_prefix_and_case() {
        use ParsedAddress::*;
        assert_eq!(parse_range_address("Sheet1!b2:c3"), Size(2, 2));
        assert_eq!(parse_range_address("'It''s'!A1"), Size(1, 1));
        assert_eq!(parse_range_address("  A1:B2  "), Size(2, 2));
        assert_eq!(
            parse_range_address("Sheet1!"),
            Unsupported(UnsupportedReason::UnidentifiedTarget)
        );
    }

    #[test]
    fn unknown_hidden_state_blocks_both_modes() {
        let sel = selection(Some("A1:B1"), None, None, 1, 2);
        for mode in MODES {
            assert_eq!(
                check_range_support(&sel, mode, 1000),
                Some(UnsupportedReason::HiddenOrFiltered)
            );
        }
    }

    #[test]
    fn known_hidden_rows_block_both_modes() {
        let sel = selection(Some("A1:B1"), Some(true), None, 1, 2);
        for mode in MODES {
            assert_eq!(
                check_range_support(&sel, mode, 1000),
                Some(UnsupportedReason::HiddenOrFiltered)
            );
        }
    }

    #[test]
    fn grid_smaller_than_address_means_rows_were_dropped() {
        let sel = selection(Some("A1:A3"), Some(false), None, 2, 1);
        assert_eq!(
            check_range_support(&sel, SheetsMode::Explain, 1000),
            Some(UnsupportedReason::HiddenOrFiltered)
        );
    }

    #[test]
    fn grid_larger_than_address_is_unidentified() {
        let sel = selection(Some("A1:A2"), Some(false), None, 3, 1);
        assert_eq!(
            check_range_support(&sel, SheetsMode::Explain, 1000),
            Some(UnsupportedReason::UnidentifiedTarget)
        );
    }

    #[test]
    fn missing_address_is_unidentified() {
        let sel = selection(None, Some(false), None, 1, 2);
        assert_eq!(
            check_range_support(&sel, SheetsMode::Replace, 1000),
            Some(UnsupportedReason::UnidentifiedTarget)
        );
    }

    #[test]
    fn whole_column_address_reports_its_own_reason() {
        let sel = selection(Some("A:A"), Some(false), None, 1, 2);
        assert_eq!(
            check_range_support(&sel, SheetsMode::Explain, 1000),
            Some(UnsupportedReason::WholeRowOrColumn)
        );
    }

    #[test]
    fn rich_text_cell_allows_plain_replacement_and_explain() {
        let mut rich = text_cell(0, 1);
        rich.has_rich_text = true;
        let sel = selection(
            Some("A1:B1"),
            Some(false),
            Some(vec![text_cell(0, 0), rich]),
            1,
            2,
        );
        for mode in MODES {
            assert_eq!(check_range_support(&sel, mode, 1000), None);
        }
    }

    #[test]
    fn link_cell_blocks_replace_but_not_explain() {
        let mut link = text_cell(0, 1);
        link.has_link = true;
        let sel = selection(
            Some("A1:B1"),
            Some(false),
            Some(vec![text_cell(0, 0), link]),
            1,
            2,
        );
        assert_eq!(
            check_range_support(&sel, SheetsMode::Replace, 1000),
            Some(UnsupportedReason::RichTextOrLink)
        );
        assert_eq!(check_range_support(&sel, SheetsMode::Explain, 1000), None);
    }

    #[test]
    fn merged_cell_blocks_both_modes() {
        let mut merged = text_cell(0, 0);
        merged.col_span = 2;
        let sel = selection(Some("A1:B1"), Some(false), Some(vec![merged]), 1, 2);
        for mode in MODES {
            assert_eq!(
                check_range_support(&sel, mode, 1000),
                Some(UnsupportedReason::MergedCells)
            );
        }
    }

    #[test]
    fn unknown_cell_type_blocks_replace_only() {
        let other = cell(0, 1, CellKind::Other, "a");
        let sel = selection(
            Some("A1:B1"),
            Some(false),
            Some(vec![text_cell(0, 0), other]),
            1,
            2,
        );
        assert_eq!(
            check_range_support(&sel, SheetsMode::Replace, 1000),
            Some(UnsupportedReason::UnknownCellType)
        );
        assert_eq!(check_range_support(&sel, SheetsMode::Explain, 1000), None);
    }

    #[test]
    fn range_over_max_cells_is_too_large() {
        let sel = selection(Some("A1:B2"), Some(false), None, 2, 2);
        assert_eq!(
            check_range_support(&sel, SheetsMode::Explain, 3),
            Some(UnsupportedReason::RangeTooLarge)
        );
        assert_eq!(check_range_support(&sel, SheetsMode::Explain, 4), None);
    }

    #[test]
    fn plain_text_range_is_supported_in_both_modes() {
        let sel = selection(Some("A1:B1"), Some(false), None, 1, 2);
        for mode in MODES {
            assert_eq!(check_range_support(&sel, mode, 1000), None);
        }
    }

    #[test]
    fn translatable_cells_skip_formula_number_date_boolean_empty_blank() {
        let cells = vec![
            cell(0, 0, CellKind::Text, "안녕"),
            cell(0, 1, CellKind::Formula, "결과"),
            cell(0, 2, CellKind::Number, "42"),
            cell(1, 0, CellKind::Date, "2026-10-13"),
            cell(1, 1, CellKind::Empty, ""),
            cell(1, 2, CellKind::Text, "   "),
            cell(2, 0, CellKind::Boolean, "TRUE"),
            cell(2, 1, CellKind::Text, "Hello"),
        ];
        assert_eq!(
            translatable_cells(&grid(3, 3, cells)),
            vec![((0, 0), "안녕".to_string()), ((2, 1), "Hello".to_string())]
        );
    }

    #[test]
    fn translatable_cells_sorted_by_row_col() {
        let cells = vec![
            cell(1, 0, CellKind::Text, "c"),
            cell(0, 1, CellKind::Text, "b"),
            cell(0, 0, CellKind::Text, "a"),
        ];
        let keys: Vec<CellKey> = translatable_cells(&grid(2, 2, cells))
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys, vec![(0, 0), (0, 1), (1, 0)]);
    }

    #[test]
    fn column_letters_after_z() {
        assert_eq!(column_letters(0), "A");
        assert_eq!(column_letters(25), "Z");
        assert_eq!(column_letters(26), "AA");
        assert_eq!(column_letters(701), "ZZ");
        assert_eq!(column_letters(702), "AAA");
    }

    #[test]
    fn range_origin_reads_first_corner() {
        assert_eq!(range_origin(Some("C5:D6")), (4, 2));
        assert_eq!(range_origin(Some("'시트 1'!B2")), (1, 1));
        assert_eq!(range_origin(Some("D6:C5")), (5, 3));
        assert_eq!(range_origin(Some("A:A")), (0, 0));
        assert_eq!(range_origin(Some("garbage")), (0, 0));
        assert_eq!(range_origin(None), (0, 0));
    }
}
