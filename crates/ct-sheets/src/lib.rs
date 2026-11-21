#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

//! Google Sheets range model, routing and support checks (pure logic, no OS access).

pub mod cf_html;
pub mod custom_clipboard;
pub mod html;
pub mod model;
pub mod outcome;
pub mod payload;
pub mod routing;
pub mod service;
pub mod support;
pub mod verify;

pub use cf_html::{build_cf_html, extract_cf_html_fragment};
pub use custom_clipboard::{
    CUSTOM_FORMAT, CUSTOM_MIME, CompactItem, compact_cells, compact_from_custom,
    decode_custom_data, encode_custom_data, expand_runs, replace_compact_strings,
};
pub use html::{html_rich_text_state, parse_sheets_html};
pub use model::{
    CellKey, CellKind, SheetCell, SheetGrid, SheetLocation, SheetSelection, SheetsParseError,
};
pub use outcome::{
    OutcomeReason, SheetsOutcome, SheetsStatus, SheetsTable, TableCell, UnsupportedReason,
};
pub use payload::{PayloadError, build_replacement_html, native_payload};
pub use routing::{
    Route, SheetsInputState, SheetsRole, SheetsRouting, choose_route, input_state_from_role,
    is_google_sheets_url, parse_sheets_url, route_of,
};
pub use service::{
    CaptureError, SheetsInteraction, TableResults, Transform, capture_for_table,
    correction_failure, replace_cells, table_from, translation_failure,
};
pub use support::{
    ParsedAddress, SheetsMode, check_range_support, column_letters, parse_range_address,
    range_origin, translatable_cells,
};
pub use verify::{same_target, written_as_expected};
