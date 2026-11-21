//! Route selection for Sheets.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetsInputState {
    CellSelection,
    CellEditing,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    SheetsRange,
    TextInteraction,
    SheetsUnsupported,
}

/// Role of the focused UIA element on a Sheets page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetsRole {
    Grid,
    CellEditor,
    FormulaBar,
    NameBox,
    Other,
}

/// Source of the foreground URL and Sheets input state (implemented by the Windows backend).
pub trait SheetsRouting: Send + Sync {
    fn url_and_state(&self) -> (Option<String>, Option<SheetsInputState>);
}

pub fn is_google_sheets_url(url: &str) -> bool {
    url.contains("docs.google.com/spreadsheets")
}

pub fn choose_route(url: Option<&str>, state: Option<SheetsInputState>) -> Route {
    match url {
        Some(u) if is_google_sheets_url(u) => match state {
            Some(SheetsInputState::CellSelection) => Route::SheetsRange,
            Some(SheetsInputState::CellEditing) => Route::TextInteraction,
            _ => Route::SheetsUnsupported,
        },
        _ => Route::TextInteraction,
    }
}

/// An empty cell editor (only newlines) means a range is selected, not edited.
/// A space-only value stays editing; an unreadable value (`None`) keeps editing.
pub fn input_state_from_role(role: SheetsRole, editor_value: Option<&str>) -> SheetsInputState {
    match role {
        SheetsRole::Grid => SheetsInputState::CellSelection,
        SheetsRole::CellEditor if editor_value.is_some_and(|v| v.trim_matches('\n').is_empty()) => {
            SheetsInputState::CellSelection
        }
        SheetsRole::CellEditor | SheetsRole::FormulaBar => SheetsInputState::CellEditing,
        SheetsRole::NameBox | SheetsRole::Other => SheetsInputState::Other,
    }
}

/// `(document_id, gid)` of a Sheets URL; `(None, None)` for anything else.
pub fn parse_sheets_url(url: &str) -> (Option<String>, Option<String>) {
    if !is_google_sheets_url(url) {
        return (None, None);
    }
    const MARKER: &str = "/spreadsheets/d/";
    let Some(pos) = url.find(MARKER) else {
        return (None, None);
    };
    let rest = &url[pos + MARKER.len()..];
    let id: &str = rest.split(['/', '?', '#']).next().unwrap_or("");
    if id.is_empty() {
        return (None, None);
    }
    (Some(id.to_string()), find_gid(url))
}

fn find_gid(url: &str) -> Option<String> {
    let bytes = url.as_bytes();
    let mut from = 0;
    while let Some(off) = url[from..].find("gid=") {
        let at = from + off;
        if at > 0 && matches!(bytes[at - 1], b'#' | b'&' | b'?') {
            let digits: String = url[at + 4..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if !digits.is_empty() {
                return Some(digits);
            }
        }
        from = at + 4;
    }
    None
}

pub fn route_of(r: &dyn SheetsRouting) -> (Option<String>, Route) {
    let (url, state) = r.url_and_state();
    let route = choose_route(url.as_deref(), state);
    (url, route)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHEETS: &str =
        "https://docs.google.com/spreadsheets/d/1AbC-dEf_9/edit?usp=sharing#gid=123456";

    #[test]
    fn choose_route_table() {
        use Route::*;
        use SheetsInputState as S;
        let cases: [(Option<&str>, Option<S>, Route); 10] = [
            (
                Some("https://mail.google.com/mail/u/0/#inbox"),
                None,
                TextInteraction,
            ),
            (
                Some("https://chat.google.com/room/abc"),
                None,
                TextInteraction,
            ),
            (
                Some("https://docs.google.com/document/d/x/edit"),
                None,
                TextInteraction,
            ),
            (
                Some("https://docs.google.com/presentation/d/x/edit"),
                Some(S::CellSelection),
                TextInteraction,
            ),
            (Some(""), None, TextInteraction),
            (None, None, TextInteraction),
            (Some(SHEETS), Some(S::CellSelection), SheetsRange),
            (Some(SHEETS), Some(S::CellEditing), TextInteraction),
            (Some(SHEETS), Some(S::Other), SheetsUnsupported),
            (Some(SHEETS), None, SheetsUnsupported),
        ];
        for (url, state, route) in cases {
            assert_eq!(choose_route(url, state), route, "{url:?} {state:?}");
        }
    }

    #[test]
    fn input_state_from_role_table() {
        use SheetsInputState as S;
        use SheetsRole as R;
        assert_eq!(input_state_from_role(R::Grid, None), S::CellSelection);
        assert_eq!(input_state_from_role(R::CellEditor, None), S::CellEditing);
        assert_eq!(input_state_from_role(R::FormulaBar, None), S::CellEditing);
        assert_eq!(input_state_from_role(R::NameBox, None), S::Other);
        assert_eq!(input_state_from_role(R::Other, None), S::Other);
    }

    #[test]
    fn empty_editor_is_selection() {
        use SheetsInputState as S;
        use SheetsRole as R;
        let cases = [
            (R::CellEditor, Some("\n"), S::CellSelection),
            (R::CellEditor, Some(""), S::CellSelection),
            (R::CellEditor, Some("안녕하세요."), S::CellEditing),
            (R::CellEditor, Some(" "), S::CellEditing),
            (R::CellEditor, None, S::CellEditing),
            (R::FormulaBar, Some(""), S::CellEditing),
            (R::Grid, Some(""), S::CellSelection),
            (R::Other, Some(""), S::Other),
        ];
        for (role, value, state) in cases {
            assert_eq!(
                input_state_from_role(role, value),
                state,
                "{role:?} {value:?}"
            );
        }
    }

    #[test]
    fn parse_sheets_url_reads_document_and_gid() {
        assert_eq!(
            parse_sheets_url(SHEETS),
            (Some("1AbC-dEf_9".into()), Some("123456".into()))
        );
    }

    #[test]
    fn parse_sheets_url_gid_in_query_and_missing_gid() {
        assert_eq!(
            parse_sheets_url("https://docs.google.com/spreadsheets/d/abc/edit?gid=7"),
            (Some("abc".into()), Some("7".into()))
        );
        assert_eq!(
            parse_sheets_url("https://docs.google.com/spreadsheets/d/abc/edit"),
            (Some("abc".into()), None)
        );
    }

    #[test]
    fn parse_sheets_url_rejects_non_sheets() {
        for url in ["", "https://docs.google.com/document/d/abc/edit", "garbage"] {
            assert_eq!(parse_sheets_url(url), (None, None), "{url}");
        }
    }

    struct Fake(Option<String>, Option<SheetsInputState>);
    impl SheetsRouting for Fake {
        fn url_and_state(&self) -> (Option<String>, Option<SheetsInputState>) {
            (self.0.clone(), self.1)
        }
    }

    #[test]
    fn route_of_maps_url_and_state() {
        let r = route_of(&Fake(
            Some(SHEETS.into()),
            Some(SheetsInputState::CellSelection),
        ));
        assert_eq!(r, (Some(SHEETS.to_string()), Route::SheetsRange));
        let r = route_of(&Fake(
            Some(SHEETS.into()),
            Some(SheetsInputState::CellEditing),
        ));
        assert_eq!(r.1, Route::TextInteraction);
        let r = route_of(&Fake(Some(SHEETS.into()), None));
        assert_eq!(r.1, Route::SheetsUnsupported);
        assert_eq!(route_of(&Fake(None, None)), (None, Route::TextInteraction));
    }
}
