//! Post-write verification: same target before writing, expected content after.

use std::collections::HashMap;

use crate::model::{CellKey, CellKind, SheetCell, SheetGrid, SheetSelection};

/// Cells in `(row, col)` order; full `SheetCell` equality is the content
/// fingerprint (compared only within one process, never logged).
fn sorted_cells(grid: &SheetGrid) -> Vec<&SheetCell> {
    let mut cells: Vec<&SheetCell> = grid.cells.iter().collect();
    cells.sort_by_key(|c| (c.row, c.col));
    cells
}

/// True only when document, sheet tab, range and content are all provably
/// unchanged. An unknown range address on either side is a mismatch.
pub fn same_target(before: &SheetSelection, after: &SheetSelection) -> bool {
    let (Some(a), Some(b)) = (
        &before.location.range_address,
        &after.location.range_address,
    ) else {
        return false;
    };
    a == b
        && before.location.document_id == after.location.document_id
        && before.location.gid == after.location.gid
        && (before.grid.rows, before.grid.cols) == (after.grid.rows, after.grid.cols)
        && sorted_cells(&before.grid) == sorted_cells(&after.grid)
}

/// Targeted cells hold the replacement as text, every other cell is as it was.
pub fn written_as_expected(
    before: &SheetGrid,
    replacements: &[(CellKey, String)],
    after: &SheetGrid,
) -> bool {
    if (before.rows, before.cols) != (after.rows, after.cols) {
        return false;
    }
    let targets: HashMap<CellKey, &str> = replacements
        .iter()
        .map(|(key, text)| (*key, text.as_str()))
        .collect();
    before.cells.iter().all(|old| {
        let Some(new) = after.cell(old.row, old.col) else {
            return false;
        };
        match targets.get(&(old.row, old.col)) {
            Some(text) => new.kind == CellKind::Text && new.display_text == *text,
            None => {
                new.kind == old.kind
                    && new.display_text == old.display_text
                    && new.formula == old.formula
                    && new.has_rich_text == old.has_rich_text
                    && new.has_link == old.has_link
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CellKind, SheetLocation};

    fn cell(
        row: usize,
        col: usize,
        kind: CellKind,
        text: &str,
        formula: Option<&str>,
    ) -> SheetCell {
        SheetCell {
            row,
            col,
            kind,
            display_text: text.to_string(),
            has_rich_text: false,
            has_link: false,
            row_span: 1,
            col_span: 1,
            formula: formula.map(str::to_string),
        }
    }

    fn text(row: usize, col: usize, t: &str) -> SheetCell {
        cell(row, col, CellKind::Text, t, None)
    }

    fn formula(row: usize, col: usize, f: &str) -> SheetCell {
        cell(row, col, CellKind::Formula, "=x", Some(f))
    }

    fn grid(cells: Vec<SheetCell>, rows: usize, cols: usize) -> SheetGrid {
        SheetGrid {
            rows,
            cols,
            cells,
            html: String::new(),
            cell_spans: Vec::new(),
            compact_json: None,
        }
    }

    fn default_cells() -> Vec<SheetCell> {
        vec![text(0, 0, "안녕"), formula(0, 1, "=A1")]
    }

    fn selection(
        cells: Vec<SheetCell>,
        rows: usize,
        cols: usize,
        doc: &str,
        gid: &str,
        address: Option<&str>,
    ) -> SheetSelection {
        SheetSelection {
            location: SheetLocation {
                document_id: doc.to_string(),
                gid: Some(gid.to_string()),
                range_address: address.map(str::to_string),
            },
            grid: grid(cells, rows, cols),
            hidden_or_filtered: Some(false),
        }
    }

    fn base() -> SheetSelection {
        selection(default_cells(), 1, 2, "doc1", "0", Some("A1:B1"))
    }

    fn rep(row: usize, col: usize, t: &str) -> Vec<(CellKey, String)> {
        vec![((row, col), t.to_string())]
    }

    #[test]
    fn same_target_true_for_identical_selection() {
        assert!(same_target(&base(), &base()));
    }

    #[test]
    fn same_target_ignores_cell_order() {
        let mut reversed = base();
        reversed.grid.cells.reverse();
        assert!(same_target(&base(), &reversed));
    }

    #[test]
    fn same_target_false_when_sheet_or_range_or_content_differs() {
        let afters = [
            selection(default_cells(), 1, 2, "doc2", "0", Some("A1:B1")),
            selection(default_cells(), 1, 2, "doc1", "7", Some("A1:B1")),
            selection(default_cells(), 1, 2, "doc1", "0", Some("A2:B2")),
            selection(
                vec![text(0, 0, "안녕!"), formula(0, 1, "=A1")],
                1,
                2,
                "doc1",
                "0",
                Some("A1:B1"),
            ),
            selection(
                vec![text(0, 0, "안녕"), formula(0, 1, "=A2")],
                1,
                2,
                "doc1",
                "0",
                Some("A1:B1"),
            ),
            selection(vec![text(0, 0, "안녕")], 1, 1, "doc1", "0", Some("A1")),
        ];
        for after in &afters {
            assert!(!same_target(&base(), after));
        }
    }

    #[test]
    fn same_target_false_when_address_unknown() {
        let unknown = selection(default_cells(), 1, 2, "doc1", "0", None);
        assert!(!same_target(&unknown, &unknown));
        assert!(!same_target(&base(), &unknown));
    }

    #[test]
    fn written_as_expected_true_on_exact_result() {
        let before = grid(default_cells(), 1, 2);
        let after = grid(vec![text(0, 0, "Hello"), formula(0, 1, "=A1")], 1, 2);
        assert!(written_as_expected(&before, &rep(0, 0, "Hello"), &after));
    }

    #[test]
    fn written_as_expected_detects_target_not_translated() {
        let cells = vec![text(0, 0, "안녕"), text(0, 1, "잘가")];
        let before = grid(cells.clone(), 1, 2);
        let after = grid(cells, 1, 2);
        assert!(!written_as_expected(&before, &rep(0, 0, "Hello"), &after));
    }

    #[test]
    fn written_as_expected_detects_untouched_formula_changed() {
        let before = grid(default_cells(), 1, 2);
        let after = grid(vec![text(0, 0, "Hello"), text(0, 1, "=x")], 1, 2);
        assert!(!written_as_expected(&before, &rep(0, 0, "Hello"), &after));
    }

    #[test]
    fn written_as_expected_detects_untouched_cell_modified() {
        let before = grid(vec![text(0, 0, "안녕"), text(0, 1, "잘가")], 1, 2);
        let after = grid(vec![text(0, 0, "Hello"), text(0, 1, "Bye")], 1, 2);
        assert!(!written_as_expected(&before, &rep(0, 0, "Hello"), &after));
    }

    #[test]
    fn written_as_expected_detects_untouched_rich_or_link_changed() {
        let before = grid(vec![text(0, 0, "안녕"), text(0, 1, "잘가")], 1, 2);
        let mut rich = vec![text(0, 0, "Hello"), text(0, 1, "잘가")];
        rich[1].has_rich_text = true;
        assert!(!written_as_expected(
            &before,
            &rep(0, 0, "Hello"),
            &grid(rich, 1, 2)
        ));
        let mut link = vec![text(0, 0, "Hello"), text(0, 1, "잘가")];
        link[1].has_link = true;
        assert!(!written_as_expected(
            &before,
            &rep(0, 0, "Hello"),
            &grid(link, 1, 2)
        ));
    }

    #[test]
    fn written_as_expected_detects_target_turned_non_text() {
        let before = grid(vec![text(0, 0, "안녕")], 1, 1);
        let after = grid(
            vec![cell(0, 0, CellKind::Formula, "=1+1", Some("=1+1"))],
            1,
            1,
        );
        assert!(!written_as_expected(&before, &rep(0, 0, "=1+1"), &after));
    }

    #[test]
    fn written_as_expected_detects_shape_change() {
        let before = grid(vec![text(0, 0, "안녕")], 1, 1);
        let after = grid(vec![text(0, 0, "Hello")], 2, 1);
        assert!(!written_as_expected(&before, &rep(0, 0, "Hello"), &after));
    }

    #[test]
    fn written_as_expected_detects_missing_cell() {
        let before = grid(vec![text(0, 0, "a"), text(0, 1, "b")], 1, 2);
        let after = grid(vec![text(0, 0, "a")], 1, 2);
        assert!(!written_as_expected(&before, &[], &after));
    }
}
