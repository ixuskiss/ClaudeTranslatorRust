#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

//! Google Sheets range model, routing and support checks (pure logic, no OS access).

pub mod cf_html;
pub mod custom_clipboard;
pub mod model;
pub mod outcome;
pub mod routing;
pub mod support;

pub use cf_html::{build_cf_html, extract_cf_html_fragment};
pub use custom_clipboard::{
    CUSTOM_FORMAT, CUSTOM_MIME, CompactItem, compact_cells, compact_from_custom,
    decode_custom_data, encode_custom_data, expand_runs, replace_compact_strings,
};
pub use model::{
    CellKey, CellKind, SheetCell, SheetGrid, SheetLocation, SheetSelection, SheetsParseError,
};
pub use outcome::{
    OutcomeReason, SheetsOutcome, SheetsStatus, SheetsTable, TableCell, UnsupportedReason,
};
pub use routing::{
    Route, SheetsInputState, SheetsRole, SheetsRouting, choose_route, input_state_from_role,
    is_google_sheets_url, parse_sheets_url, route_of,
};
pub use support::{
    ParsedAddress, SheetsMode, check_range_support, column_letters, parse_range_address,
    range_origin, translatable_cells,
};
