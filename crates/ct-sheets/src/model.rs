//! Data model of a captured Sheets range.

/// `(row, col)` inside the selected range, 0-based.
// TODO(plan4): a later task replaces this with a re-export from ct-core.
pub type CellKey = (usize, usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Text,
    Formula,
    Number,
    Date,
    Boolean,
    Empty,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetCell {
    pub row: usize,
    pub col: usize,
    pub kind: CellKind,
    pub display_text: String,
    pub has_rich_text: bool,
    pub has_link: bool,
    pub row_span: usize,
    pub col_span: usize,
    pub formula: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetGrid {
    pub rows: usize,
    pub cols: usize,
    /// Parse order (document order of `<td>`).
    pub cells: Vec<SheetCell>,
    /// The HTML fragment that was actually parsed.
    pub html: String,
    /// Byte offsets `(start, end)` of each `<td ...>...</td>` in `html`, same order as `cells`.
    pub cell_spans: Vec<(usize, usize)>,
    pub compact_json: Option<String>,
}

impl SheetGrid {
    pub fn cell(&self, row: usize, col: usize) -> Option<&SheetCell> {
        self.cells.iter().find(|c| c.row == row && c.col == col)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetLocation {
    pub document_id: String,
    pub gid: Option<String>,
    pub range_address: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetSelection {
    pub location: SheetLocation,
    pub grid: SheetGrid,
    /// `None` = could not be determined; never treated as "not hidden".
    pub hidden_or_filtered: Option<bool>,
}
