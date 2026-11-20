//! Personal dictionary window (v5 §4.5). `DictionaryForm` is the pure, unit-tested form
//! model; `DictionaryWindow` binds it to the Slint `DictionaryWin` on the UI thread.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ct_core::{Action, DictionaryEntry, DictionaryError, PersonalDictionaryStore};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::win32::{HWND_RETRIES, bring_to_front, hwnd_of, wait_for_hwnd};
use super::{DictRow, DictionaryWin};

pub const SELECT_TO_UPDATE: &str = "Select an entry to update first";
pub const SELECT_TO_DELETE: &str = "Select an entry to delete first";
/// Not in v5 (it allowed an empty term); a plan decision.
pub const EMPTY_TERM: &str = "Term을 입력하세요.";
const SAVE_FAILED: &str = "Could not save the personal dictionary";

/// Language fields in form (and table) order.
const LANGS: [&str; 4] = ["ko", "en", "vi", "ja"];
/// Action combo order.
const ACTIONS: [Action; 2] = [Action::Keep, Action::Translate];

/// The input row: Term, Action and the four translation fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictionaryForm {
    pub term: String,
    pub action: Action,
    pub ko: String,
    pub en: String,
    pub vi: String,
    pub ja: String,
}

impl DictionaryForm {
    /// Builds the entry: term and translations are trimmed, only non-empty translations
    /// are kept. Errors are the user-facing message.
    pub fn to_entry(&self) -> Result<DictionaryEntry, String> {
        let term = self.term.trim();
        if term.is_empty() {
            return Err(EMPTY_TERM.to_string());
        }
        let translations: BTreeMap<String, String> = [
            ("ko", &self.ko),
            ("en", &self.en),
            ("vi", &self.vi),
            ("ja", &self.ja),
        ]
        .into_iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .map(|(k, v)| (k.to_string(), v.trim().to_string()))
        .collect();
        DictionaryEntry::new(term.to_string(), self.action, translations)
            .map_err(|e| error_message(&e))
    }

    /// Missing languages become `""`.
    pub fn from_entry(e: &DictionaryEntry) -> Self {
        let get = |lang: &str| e.translations.get(lang).cloned().unwrap_or_default();
        DictionaryForm {
            term: e.term.clone(),
            action: e.action,
            ko: get("ko"),
            en: get("en"),
            vi: get("vi"),
            ja: get("ja"),
        }
    }
}

/// v5's store messages. `Io` carries no content (it may name a path).
pub fn error_message(e: &DictionaryError) -> String {
    match e {
        DictionaryError::Duplicate(t) => {
            format!("Term '{t}' already exists in the personal dictionary")
        }
        DictionaryError::TranslateWithoutTranslations(t) => {
            format!("TRANSLATE entry for term '{t}' requires at least one translation")
        }
        DictionaryError::NotFound(t) => format!("Term '{t}' not found in the personal dictionary"),
        DictionaryError::Io(_) => SAVE_FAILED.to_string(),
    }
}

/// Translations column: values joined by `, ` in ko, en, vi, ja order (then any other).
fn translations_text(e: &DictionaryEntry) -> String {
    let mut parts: Vec<&str> = LANGS
        .iter()
        .filter_map(|l| e.translations.get(*l).map(String::as_str))
        .collect();
    parts.extend(
        e.translations
            .iter()
            .filter(|(k, _)| !LANGS.contains(&k.as_str()))
            .map(|(_, v)| v.as_str()),
    );
    parts.join(", ")
}

// ---------------------------------------------------------------------------------------
// Slint window (UI thread only)
// ---------------------------------------------------------------------------------------

/// Called with the full entry list after every successful change.
type OnChanged = Box<dyn Fn(Vec<DictionaryEntry>)>;

struct Open {
    win: DictionaryWin,
    store: Arc<Mutex<PersonalDictionaryStore>>,
    on_changed: OnChanged,
    /// Term of the selected table row (`None`: nothing selected).
    selected: RefCell<Option<String>>,
}

thread_local! {
    /// The one open dictionary window (UI thread).
    static CURRENT: RefCell<Option<Rc<Open>>> = const { RefCell::new(None) };
}

fn current() -> Option<Rc<Open>> {
    CURRENT.with(|c| c.borrow().clone())
}

impl Open {
    fn entries(&self) -> Vec<DictionaryEntry> {
        self.store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries()
            .to_vec()
    }

    fn form(&self) -> DictionaryForm {
        let w = &self.win;
        DictionaryForm {
            term: w.get_term().to_string(),
            action: usize::try_from(w.get_action_index())
                .ok()
                .and_then(|i| ACTIONS.get(i).copied())
                .unwrap_or(Action::Keep),
            ko: w.get_ko().to_string(),
            en: w.get_en().to_string(),
            vi: w.get_vi().to_string(),
            ja: w.get_ja().to_string(),
        }
    }

    fn fill(&self, f: &DictionaryForm) {
        let w = &self.win;
        w.set_term(f.term.as_str().into());
        w.set_action_index(ACTIONS.iter().position(|a| *a == f.action).unwrap_or(0) as i32);
        w.set_ko(f.ko.as_str().into());
        w.set_en(f.en.as_str().into());
        w.set_vi(f.vi.as_str().into());
        w.set_ja(f.ja.as_str().into());
    }

    fn set_error(&self, text: &str) {
        self.win.set_error_text(text.into());
    }

    /// Rebuilds the table from the store; a selected term that vanished is deselected.
    fn refresh_table(&self) {
        let entries = self.entries();
        {
            let mut selected = self.selected.borrow_mut();
            if let Some(term) = selected.as_deref()
                && !entries.iter().any(|e| e.term == term)
            {
                *selected = None;
            }
        }
        let selected = self.selected.borrow();
        let rows: Vec<DictRow> = entries
            .iter()
            .map(|e| DictRow {
                term: e.term.as_str().into(),
                action: e.action.as_str().into(),
                translations: translations_text(e).into(),
                selected: selected.as_deref() == Some(e.term.as_str()),
            })
            .collect();
        self.win.set_rows(ModelRc::new(VecModel::from(rows)));
    }

    fn changed(&self) {
        self.refresh_table();
        (self.on_changed)(self.entries());
    }

    /// Selecting a row fills the form (deselecting never clears it, as in v5).
    fn on_row_selected(&self, row: i32) {
        let entry = usize::try_from(row)
            .ok()
            .and_then(|i| self.entries().into_iter().nth(i));
        if let Some(e) = entry {
            self.fill(&DictionaryForm::from_entry(&e));
            *self.selected.borrow_mut() = Some(e.term);
            self.set_error("");
            self.refresh_table();
        }
    }

    fn on_add(&self) {
        self.set_error("");
        let entry = match self.form().to_entry() {
            Ok(e) => e,
            Err(m) => return self.set_error(&m),
        };
        let result = self
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .add(entry);
        match result {
            Ok(()) => {
                *self.selected.borrow_mut() = None;
                self.changed();
            }
            Err(e) => self.fail("add", &e),
        }
    }

    fn on_update(&self) {
        self.set_error("");
        let Some(old) = self.selected.borrow().clone() else {
            return self.set_error(SELECT_TO_UPDATE);
        };
        let entry = match self.form().to_entry() {
            Ok(e) => e,
            Err(m) => return self.set_error(&m),
        };
        let term = entry.term.clone();
        let result = self
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .update(&old, entry);
        match result {
            Ok(()) => {
                // Unlike v5 (selection lost on refresh) the row stays selected.
                *self.selected.borrow_mut() = Some(term);
                self.changed();
            }
            Err(e) => self.fail("update", &e),
        }
    }

    fn on_delete(&self) {
        self.set_error("");
        let Some(term) = self.selected.borrow().clone() else {
            return self.set_error(SELECT_TO_DELETE);
        };
        let result = self
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .delete(&term);
        match result {
            Ok(()) => {
                *self.selected.borrow_mut() = None;
                self.changed();
            }
            Err(e) => self.fail("delete", &e),
        }
    }

    fn fail(&self, op: &str, e: &DictionaryError) {
        // Only the operation and error kind are logged, never the term.
        let kind = match e {
            DictionaryError::Duplicate(_) => "duplicate",
            DictionaryError::TranslateWithoutTranslations(_) => "no_translations",
            DictionaryError::NotFound(_) => "not_found",
            DictionaryError::Io(_) => "io",
        };
        tracing::warn!(op, kind, "dictionary: operation failed");
        self.set_error(&error_message(e));
    }
}

/// Hides and forgets the open window; it is dropped on a later event-loop turn, never
/// inside its own callback.
fn close_current() {
    let Some(open) = CURRENT.with(|c| c.borrow_mut().take()) else {
        return;
    };
    if let Err(e) = open.win.hide() {
        tracing::warn!(error = %e, "dictionary: hide failed");
    }
    slint::Timer::single_shot(Duration::ZERO, move || drop(open));
}

/// The personal dictionary window. Only one is open at a time; UI thread only.
pub struct DictionaryWindow;

impl DictionaryWindow {
    /// Opens the window, or brings the open one to the front. Every successful
    /// Add/Update/Delete is already persisted by `store`; afterwards `on_changed` is
    /// called (UI thread, store lock released) with the full current entry list, so the
    /// caller can replace `AppState.entries`. Must be called on the UI thread.
    pub fn open(
        store: Arc<Mutex<PersonalDictionaryStore>>,
        on_changed: impl Fn(Vec<DictionaryEntry>) + 'static,
    ) {
        if let Some(open) = current() {
            match hwnd_of(open.win.window()) {
                Ok(hwnd) => bring_to_front(hwnd),
                Err(e) => tracing::warn!(error = %e, "dictionary: re-open focus failed"),
            }
            return;
        }
        if let Err(e) = Self::create(store, Box::new(on_changed)) {
            tracing::error!(error = %e, "dictionary: open failed");
        }
    }

    fn create(
        store: Arc<Mutex<PersonalDictionaryStore>>,
        on_changed: OnChanged,
    ) -> Result<(), String> {
        let win = DictionaryWin::new().map_err(|e| e.to_string())?;
        win.set_action_items(ModelRc::new(VecModel::from(
            ACTIONS
                .iter()
                .map(|a| SharedString::from(a.as_str()))
                .collect::<Vec<_>>(),
        )));
        let open = Rc::new(Open {
            win,
            store,
            on_changed,
            selected: RefCell::new(None),
        });
        open.refresh_table();
        wire(&open);

        CURRENT.with(|c| *c.borrow_mut() = Some(open.clone()));
        if let Err(e) = open.win.show() {
            close_current();
            return Err(e.to_string());
        }
        let weak = open.win.as_weak();
        wait_for_hwnd(
            move || {
                let win = weak
                    .upgrade()
                    .ok_or_else(|| "dictionary window closed".to_string())?;
                hwnd_of(win.window())
            },
            HWND_RETRIES,
            |hwnd| match hwnd {
                Ok(hwnd) => bring_to_front(hwnd),
                Err(e) => tracing::warn!(error = %e, "dictionary: no HWND to focus"),
            },
        );
        Ok(())
    }
}

/// Handlers look the window up through `CURRENT`, so the component holds no strong
/// reference to its own state.
fn wire(open: &Open) {
    fn with(f: impl Fn(&Open)) {
        if let Some(open) = current() {
            f(&open);
        }
    }
    let w = &open.win;
    w.on_row_selected(|i| with(|o| o.on_row_selected(i)));
    w.on_add(|| with(Open::on_add));
    w.on_update(|| with(Open::on_update));
    w.on_delete(|| with(Open::on_delete));
    w.window().on_close_requested(|| {
        close_current();
        slint::CloseRequestResponse::HideWindow
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(term: &str, action: Action, tr: [&str; 4]) -> DictionaryForm {
        DictionaryForm {
            term: term.into(),
            action,
            ko: tr[0].into(),
            en: tr[1].into(),
            vi: tr[2].into(),
            ja: tr[3].into(),
        }
    }

    #[test]
    fn form_keeps_only_non_empty_translations() {
        let e = form(
            "logical model",
            Action::Translate,
            ["논리 모델", "", "", "論理"],
        )
        .to_entry()
        .unwrap();
        assert_eq!(e.term, "logical model");
        assert_eq!(e.translations.len(), 2);
        assert_eq!(e.translations["ko"], "논리 모델");
        assert_eq!(e.translations["ja"], "論理");
        let keep = form("DA", Action::Keep, ["", "", "", ""])
            .to_entry()
            .unwrap();
        assert!(keep.translations.is_empty());
    }

    #[test]
    fn translate_without_translations_is_error() {
        let err = form("logical model", Action::Translate, ["", "  ", "", ""])
            .to_entry()
            .unwrap_err();
        assert_eq!(
            err,
            "TRANSLATE entry for term 'logical model' requires at least one translation"
        );
    }

    #[test]
    fn empty_term_is_rejected() {
        for t in ["", "   "] {
            assert_eq!(
                form(t, Action::Keep, ["", "", "", ""])
                    .to_entry()
                    .unwrap_err(),
                EMPTY_TERM
            );
        }
        assert_eq!(EMPTY_TERM, "Term을 입력하세요.");
    }

    #[test]
    fn duplicate_message_matches_v5() {
        assert_eq!(
            error_message(&DictionaryError::Duplicate("DA".into())),
            "Term 'DA' already exists in the personal dictionary"
        );
    }

    #[test]
    fn not_found_and_io_messages() {
        assert_eq!(
            error_message(&DictionaryError::NotFound("DA".into())),
            "Term 'DA' not found in the personal dictionary"
        );
        let io = DictionaryError::Io(std::io::Error::other("C:\\secret\\path"));
        assert_eq!(error_message(&io), "Could not save the personal dictionary");
    }

    #[test]
    fn from_entry_fills_missing_languages_with_empty() {
        let e = form("x", Action::Translate, ["가", "", "", "ア"])
            .to_entry()
            .unwrap();
        assert_eq!(
            DictionaryForm::from_entry(&e),
            form("x", Action::Translate, ["가", "", "", "ア"])
        );
    }

    #[test]
    fn translations_column_joins_values_in_form_order() {
        let e = form("x", Action::Translate, ["가", "b", "c", "ア"])
            .to_entry()
            .unwrap();
        assert_eq!(translations_text(&e), "가, b, c, ア");
        let keep = form("DA", Action::Keep, ["", "", "", ""])
            .to_entry()
            .unwrap();
        assert_eq!(translations_text(&keep), "");
    }

    #[test]
    fn selection_messages_are_verbatim() {
        assert_eq!(SELECT_TO_UPDATE, "Select an entry to update first");
        assert_eq!(SELECT_TO_DELETE, "Select an entry to delete first");
    }
}
