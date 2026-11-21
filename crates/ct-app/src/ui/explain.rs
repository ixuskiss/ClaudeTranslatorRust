//! Ctrl+Ctrl explanation window (v5 §4.6). The view state is built purely (and unit
//! tested); the Slint window is created once on the UI thread and reused.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ct_core::{CorrectionSentence, DisplayMode, ExplanationResult, Lang};
use slint::winit_030::WinitWindowAccessor;
use slint::winit_030::winit::window::ResizeDirection;
use slint::{ComponentHandle, ModelRc, VecModel};
use windows::Win32::Foundation::HWND;

use super::win32::{HWND_RETRIES, bring_to_front, hwnd_of, wait_for_hwnd, work_area_of};
use super::{ExplainCard, ExplainLine, ExplainWin};
use crate::flows::{ExplainView, Notifier};

/// Pure view state rendered by the explanation window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExplainViewState {
    pub status: Option<String>,
    pub blocks: Vec<ExplainBlock>,
}

/// One white card: the original text and its translations.
#[derive(Clone, Debug, PartialEq)]
pub struct ExplainBlock {
    pub original: String,
    /// The language tag (`EN`, `JA`) is `Some` only when the block has 2+ translations.
    pub translations: Vec<(Option<&'static str>, String)>,
}

fn tag(lang: Lang) -> &'static str {
    match lang {
        Lang::Ko => "KO",
        Lang::En => "EN",
        Lang::Vi => "VI",
        Lang::Ja => "JA",
    }
}

fn block(original: String, translations: Vec<(Lang, String)>) -> ExplainBlock {
    let tagged = translations.len() >= 2;
    ExplainBlock {
        original,
        translations: translations
            .into_iter()
            .map(|(lang, text)| (tagged.then(|| tag(lang)), text))
            .collect(),
    }
}

/// Whole text: one block, originals joined by `\n` and each language's translations joined
/// by `\n`, languages in Target order (order of first appearance). Per sentence: one block
/// per sentence.
pub fn build_view_state(result: &ExplanationResult, mode: DisplayMode) -> ExplainViewState {
    let blocks = match mode {
        DisplayMode::PerSentence => result
            .sentences
            .iter()
            .map(|s| block(s.original.clone(), s.translations.clone()))
            .collect(),
        DisplayMode::WholeText if result.sentences.is_empty() => Vec::new(),
        DisplayMode::WholeText => {
            let original = result
                .sentences
                .iter()
                .map(|s| s.original.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let mut langs: Vec<Lang> = Vec::new();
            for (lang, _) in result.sentences.iter().flat_map(|s| &s.translations) {
                if !langs.contains(lang) {
                    langs.push(*lang);
                }
            }
            let columns = langs
                .into_iter()
                .map(|lang| {
                    let text = result
                        .sentences
                        .iter()
                        .filter_map(|s| {
                            s.translations
                                .iter()
                                .find(|(l, _)| *l == lang)
                                .map(|(_, t)| t.as_str())
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    (lang, text)
                })
                .collect();
            vec![block(original, columns)]
        }
    };
    ExplainViewState {
        status: None,
        blocks,
    }
}

/// Correction result: same grouping as `build_view_state`, corrected text without a tag.
pub fn build_correction_view_state(
    sentences: &[CorrectionSentence],
    mode: DisplayMode,
) -> ExplainViewState {
    let one = |original: String, corrected: String| ExplainBlock {
        original,
        translations: vec![(None, corrected)],
    };
    let blocks = match mode {
        DisplayMode::PerSentence => sentences
            .iter()
            .map(|s| one(s.original.clone(), s.corrected.clone()))
            .collect(),
        DisplayMode::WholeText if sentences.is_empty() => Vec::new(),
        DisplayMode::WholeText => {
            let join = |f: fn(&CorrectionSentence) -> &str| {
                sentences.iter().map(f).collect::<Vec<_>>().join("\n")
            };
            vec![one(join(|s| &s.original), join(|s| &s.corrected))]
        }
    };
    ExplainViewState {
        status: None,
        blocks,
    }
}

pub fn loading_state(text: &str) -> ExplainViewState {
    ExplainViewState {
        status: Some(text.to_string()),
        blocks: Vec::new(),
    }
}

/// Errors and notices render as `Error: {message}` (v5 §4.6 / §4.8).
pub fn error_state(message: &str) -> ExplainViewState {
    ExplainViewState {
        status: Some(format!("Error: {message}")),
        blocks: Vec::new(),
    }
}

/// How a new state is presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Present {
    /// Loading / error: keep the current size, show and bring to front.
    Front,
    /// Result: fit the size to the content, never re-show a hidden window (v5 §7-19).
    Fit,
}

/// The real window (or a test double) the host draws into.
pub trait ExplainSurface {
    fn apply(&self, state: &ExplainViewState);
    fn fit_to_content(&self);
    fn show_and_front(&self);
}

pub trait SurfaceFactory {
    type Surface: ExplainSurface;
    fn create(&mut self) -> Result<Self::Surface, String>;
}

/// Owns the single explanation window; creates it on first use and reuses it after.
pub struct ExplainHost<F: SurfaceFactory> {
    factory: F,
    surface: Option<F::Surface>,
}

impl<F: SurfaceFactory> ExplainHost<F> {
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            surface: None,
        }
    }

    pub fn present(&mut self, state: &ExplainViewState, how: Present) {
        if self.surface.is_none() {
            match self.factory.create() {
                Ok(surface) => self.surface = Some(surface),
                Err(e) => {
                    tracing::error!(error = %e, "explain window: create failed");
                    return;
                }
            }
        }
        let Some(surface) = self.surface.as_ref() else {
            return;
        };
        surface.apply(state);
        match how {
            Present::Front => surface.show_and_front(),
            Present::Fit => surface.fit_to_content(),
        }
    }
}

/// Auto-size limits (v5 §4.6), logical px.
const MIN_W: f32 = 480.0;
const MIN_H: f32 = 240.0;
const MAX_W: f32 = 700.0;
const MAX_H: f32 = 800.0;

/// `width = clamp(content.w + 40, 480, min(700, avail.w - 40))`,
/// `height = clamp(content.h + 80, 240, min(800, avail.h - 80))` (v5 §4.6).
pub fn fit_size(content: (f32, f32), avail: Option<(f32, f32)>) -> (f32, f32) {
    let (max_w, max_h) = match avail {
        Some((w, h)) => (
            MAX_W.min(w - 40.0).max(MIN_W),
            MAX_H.min(h - 80.0).max(MIN_H),
        ),
        None => (MAX_W, MAX_H),
    };
    (
        (content.0 + 40.0).clamp(MIN_W, max_w),
        (content.1 + 80.0).clamp(MIN_H, max_h),
    )
}

/// Delay before re-measuring the height once the new width is laid out.
const REFIT_DELAY: std::time::Duration = std::time::Duration::from_millis(30);

struct SlintSurface {
    win: ExplainWin,
    hwnd: Rc<Cell<Option<HWND>>>,
}

impl SlintSurface {
    /// Available work area of the window's monitor in logical px.
    fn avail(&self) -> Option<(f32, f32)> {
        let hwnd = self.hwnd.get()?;
        let (l, t, r, b) = work_area_of(hwnd)?;
        let scale = self.win.window().scale_factor();
        Some(((r - l) as f32 / scale, (b - t) as f32 / scale))
    }
}

impl ExplainSurface for SlintSurface {
    fn apply(&self, state: &ExplainViewState) {
        self.win
            .set_status(state.status.clone().unwrap_or_default().into());
        let cards: Vec<ExplainCard> = state
            .blocks
            .iter()
            .map(|b| ExplainCard {
                original: b.original.as_str().into(),
                translations: ModelRc::new(VecModel::from(
                    b.translations
                        .iter()
                        .map(|(tag, text)| ExplainLine {
                            tag: tag.unwrap_or_default().into(),
                            text: text.as_str().into(),
                        })
                        .collect::<Vec<_>>(),
                )),
            })
            .collect();
        tracing::debug!(blocks = cards.len(), "explain window: state applied");
        self.win.set_cards(ModelRc::new(VecModel::from(cards)));
    }

    fn fit_to_content(&self) {
        let avail = self.avail();
        let (w, h) = fit_size(
            (self.win.get_content_width(), self.win.get_content_height()),
            avail,
        );
        self.win.window().set_size(slint::LogicalSize::new(w, h));
        // Wrapped text height depends on the width just set: re-measure once laid out.
        let weak = self.win.as_weak();
        slint::Timer::single_shot(REFIT_DELAY, move || {
            if let Some(win) = weak.upgrade() {
                let (_, h) = fit_size((0.0, win.get_content_height()), avail);
                win.window().set_size(slint::LogicalSize::new(w, h));
            }
        });
    }

    fn show_and_front(&self) {
        if let Err(e) = self.win.show() {
            tracing::error!(error = %e, "explain window: show failed");
            return;
        }
        if let Some(hwnd) = self.hwnd.get() {
            bring_to_front(hwnd);
            return;
        }
        let probe = self.win.as_weak();
        let slot = self.hwnd.clone();
        wait_for_hwnd(
            move || {
                let w = probe
                    .upgrade()
                    .ok_or_else(|| "explain window dropped".to_string())?;
                hwnd_of(w.window())
            },
            HWND_RETRIES,
            move |res| match res {
                Ok(hwnd) => {
                    slot.set(Some(hwnd));
                    bring_to_front(hwnd);
                }
                Err(e) => tracing::error!(error = %e, "explain window: no HWND after retries"),
            },
        );
    }
}

fn resize_direction(dir: &str) -> Option<ResizeDirection> {
    Some(match dir {
        "n" => ResizeDirection::North,
        "s" => ResizeDirection::South,
        "e" => ResizeDirection::East,
        "w" => ResizeDirection::West,
        "ne" => ResizeDirection::NorthEast,
        "nw" => ResizeDirection::NorthWest,
        "se" => ResizeDirection::SouthEast,
        "sw" => ResizeDirection::SouthWest,
        _ => return None,
    })
}

struct SlintFactory;

impl SurfaceFactory for SlintFactory {
    type Surface = SlintSurface;

    fn create(&mut self) -> Result<SlintSurface, String> {
        let win = ExplainWin::new().map_err(|e| e.to_string())?;
        let weak = win.as_weak();
        win.on_close_requested(move || {
            // Closing only hides; the app keeps running (v5 §4.6).
            if let Some(w) = weak.upgrade()
                && let Err(e) = w.hide()
            {
                tracing::warn!(error = %e, "explain window: hide failed");
            }
        });
        let weak = win.as_weak();
        win.on_drag_requested(move || {
            if let Some(w) = weak.upgrade()
                && let Some(Err(e)) = w.window().with_winit_window(|ww| ww.drag_window())
            {
                tracing::debug!(error = %e, "explain window: drag failed");
            }
        });
        let weak = win.as_weak();
        win.on_resize_requested(move |dir| {
            let Some(direction) = resize_direction(dir.as_str()) else {
                return;
            };
            if let Some(w) = weak.upgrade()
                && let Some(Err(e)) = w
                    .window()
                    .with_winit_window(|ww| ww.drag_resize_window(direction))
            {
                tracing::debug!(error = %e, "explain window: resize failed");
            }
        });
        tracing::info!("explain window: created");
        Ok(SlintSurface {
            win,
            hwnd: Rc::new(Cell::new(None)),
        })
    }
}

thread_local! {
    /// The single explanation window host, owned by the UI thread.
    static HOST: RefCell<Option<ExplainHost<SlintFactory>>> = const { RefCell::new(None) };
}

/// `ExplainView` + `Notifier` handle; callable from any thread. Every call is marshalled to
/// the UI thread with `slint::invoke_from_event_loop`, where one window is created lazily
/// and reused.
#[derive(Default)]
pub struct ExplainWindow {
    _private: (),
}

impl ExplainWindow {
    pub fn new() -> Self {
        Self::default()
    }

    fn dispatch(&self, state: ExplainViewState, how: Present) {
        let sent = slint::invoke_from_event_loop(move || {
            HOST.with(|host| {
                host.borrow_mut()
                    .get_or_insert_with(|| ExplainHost::new(SlintFactory))
                    .present(&state, how);
            });
        });
        if let Err(e) = sent {
            tracing::warn!(error = %e, "explain window: update not delivered");
        }
    }
}

impl ExplainView for ExplainWindow {
    fn show_loading(&self, text: &str) {
        self.dispatch(loading_state(text), Present::Front);
    }

    fn show_error(&self, message: &str) {
        self.dispatch(error_state(message), Present::Front);
    }

    fn show_result(&self, result: ExplanationResult, mode: DisplayMode) {
        self.dispatch(build_view_state(&result, mode), Present::Fit);
    }

    fn show_correction(&self, sentences: Vec<CorrectionSentence>, mode: DisplayMode) {
        self.dispatch(build_correction_view_state(&sentences, mode), Present::Fit);
    }
}

impl Notifier for ExplainWindow {
    /// Rendered as `Error: {message}` in the explanation window, then brought to front.
    fn notify(&self, message: &str) {
        self.show_error(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ct_core::ExplanationSentence;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn sentence(original: &str, tr: &[(Lang, &str)]) -> ExplanationSentence {
        ExplanationSentence {
            original: original.into(),
            translations: tr.iter().map(|(l, t)| (*l, (*t).to_string())).collect(),
        }
    }

    fn two_sentences() -> ExplanationResult {
        ExplanationResult {
            sentences: vec![
                sentence("안녕.", &[(Lang::En, "Hi."), (Lang::Ja, "やあ。")]),
                sentence("잘 가.", &[(Lang::En, "Bye."), (Lang::Ja, "またね。")]),
            ],
        }
    }

    #[test]
    fn whole_text_joins_originals_and_each_language_column() {
        let state = build_view_state(&two_sentences(), DisplayMode::WholeText);
        assert_eq!(state.status, None);
        assert_eq!(
            state.blocks,
            vec![ExplainBlock {
                original: "안녕.\n잘 가.".into(),
                translations: vec![
                    (Some("EN"), "Hi.\nBye.".into()),
                    (Some("JA"), "やあ。\nまたね。".into()),
                ],
            }]
        );
    }

    #[test]
    fn per_sentence_one_block_per_sentence() {
        let state = build_view_state(&two_sentences(), DisplayMode::PerSentence);
        assert_eq!(state.status, None);
        assert_eq!(
            state.blocks,
            vec![
                ExplainBlock {
                    original: "안녕.".into(),
                    translations: vec![(Some("EN"), "Hi.".into()), (Some("JA"), "やあ。".into())],
                },
                ExplainBlock {
                    original: "잘 가.".into(),
                    translations: vec![
                        (Some("EN"), "Bye.".into()),
                        (Some("JA"), "またね。".into())
                    ],
                },
            ]
        );
    }

    #[test]
    fn language_tags_only_when_two_or_more() {
        let one = ExplanationResult {
            sentences: vec![
                sentence("안녕.", &[(Lang::En, "Hi.")]),
                sentence("잘 가.", &[(Lang::En, "Bye.")]),
            ],
        };
        let whole = build_view_state(&one, DisplayMode::WholeText);
        assert_eq!(
            whole.blocks[0].translations,
            vec![(None, "Hi.\nBye.".into())]
        );
        let per = build_view_state(&one, DisplayMode::PerSentence);
        assert_eq!(per.blocks[0].translations, vec![(None, "Hi.".into())]);
        assert_eq!(per.blocks[1].translations, vec![(None, "Bye.".into())]);
    }

    fn corrections() -> Vec<ct_core::CorrectionSentence> {
        ["가", "나"]
            .iter()
            .map(|o| ct_core::CorrectionSentence {
                original: (*o).into(),
                corrected: format!("{o}."),
            })
            .collect()
    }

    #[test]
    fn correction_per_sentence_one_block_per_sentence_without_tag() {
        let state = build_correction_view_state(&corrections(), DisplayMode::PerSentence);
        assert_eq!(state.status, None);
        assert_eq!(
            state.blocks,
            vec![
                ExplainBlock {
                    original: "가".into(),
                    translations: vec![(None, "가.".into())],
                },
                ExplainBlock {
                    original: "나".into(),
                    translations: vec![(None, "나.".into())],
                },
            ]
        );
    }

    #[test]
    fn correction_whole_text_joins_with_newline() {
        let state = build_correction_view_state(&corrections(), DisplayMode::WholeText);
        assert_eq!(state.status, None);
        assert_eq!(
            state.blocks,
            vec![ExplainBlock {
                original: "가\n나".into(),
                translations: vec![(None, "가.\n나.".into())],
            }]
        );
    }

    #[test]
    fn correction_empty_sentences_has_no_blocks() {
        let state = build_correction_view_state(&[], DisplayMode::WholeText);
        assert!(state.blocks.is_empty());
        assert_eq!(state.status, None);
    }

    #[test]
    fn error_status_has_prefix() {
        let state = error_state("boom");
        assert_eq!(state.status.as_deref(), Some("Error: boom"));
        assert!(state.blocks.is_empty());
        let loading = loading_state("Translating...");
        assert_eq!(loading.status.as_deref(), Some("Translating..."));
        assert!(loading.blocks.is_empty());
    }

    #[derive(Default)]
    struct Log {
        created: usize,
        applied: Vec<ExplainViewState>,
        fits: usize,
        fronts: usize,
    }

    struct FakeSurface(Rc<RefCell<Log>>);
    impl ExplainSurface for FakeSurface {
        fn apply(&self, state: &ExplainViewState) {
            self.0.borrow_mut().applied.push(state.clone());
        }
        fn fit_to_content(&self) {
            self.0.borrow_mut().fits += 1;
        }
        fn show_and_front(&self) {
            self.0.borrow_mut().fronts += 1;
        }
    }

    struct FakeFactory(Rc<RefCell<Log>>);
    impl SurfaceFactory for FakeFactory {
        type Surface = FakeSurface;
        fn create(&mut self) -> Result<FakeSurface, String> {
            self.0.borrow_mut().created += 1;
            Ok(FakeSurface(self.0.clone()))
        }
    }

    #[test]
    fn explain_view_reuses_single_window() {
        let log = Rc::new(RefCell::new(Log::default()));
        let mut host = ExplainHost::new(FakeFactory(log.clone()));
        let first = build_view_state(&two_sentences(), DisplayMode::WholeText);
        let second = build_view_state(&two_sentences(), DisplayMode::PerSentence);
        host.present(&first, Present::Fit);
        host.present(&second, Present::Fit);
        let log = log.borrow();
        assert_eq!(log.created, 1);
        assert_eq!(log.applied.last(), Some(&second));
        assert_eq!(log.fits, 2);
        // A result never re-shows / re-fronts the window.
        assert_eq!(log.fronts, 0);
    }

    #[test]
    fn fit_size_clamps_to_min_and_max() {
        assert_eq!(
            fit_size((10.0, 10.0), Some((1920.0, 1040.0))),
            (480.0, 240.0)
        );
        assert_eq!(
            fit_size((2000.0, 2000.0), Some((1920.0, 1040.0))),
            (700.0, 800.0)
        );
        assert_eq!(
            fit_size((2000.0, 2000.0), Some((600.0, 500.0))),
            (560.0, 420.0)
        );
        assert_eq!(fit_size((500.0, 300.0), None), (540.0, 380.0));
    }

    #[test]
    fn loading_and_error_bring_to_front_without_fitting() {
        let log = Rc::new(RefCell::new(Log::default()));
        let mut host = ExplainHost::new(FakeFactory(log.clone()));
        host.present(&loading_state("Translating..."), Present::Front);
        host.present(&error_state("x"), Present::Front);
        let log = log.borrow();
        assert_eq!(log.created, 1);
        assert_eq!(log.fronts, 2);
        assert_eq!(log.fits, 0);
    }
}
