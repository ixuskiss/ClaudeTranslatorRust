//! System tray icon and context menu (v5 `tray_application.py` §4.3, `icon.py` §4.2).
//!
//! `Tray` only turns menu clicks into `TrayCommand`s; applying them (saving settings,
//! opening windows, pausing) is the main loop's job. Everything must run on the UI thread.

use std::cell::RefCell;

use ct_core::{Lang, UserSettings, normalize_targets};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

const TOOLTIP: &str = "ClaudeTranslator";
/// Menu order of the target languages (differs from the Settings order).
const TARGET_ORDER: [Lang; 4] = [Lang::En, Lang::Vi, Lang::Ja, Lang::Ko];

const ID_DICTIONARY: &str = "dictionary";
const ID_SETTINGS: &str = "settings";
const ID_PAUSE: &str = "pause";
const ID_EXIT: &str = "exit";
const ID_TARGET_PREFIX: &str = "target:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetMenuItem {
    pub lang: Lang,
    pub label: String,
    pub checked: bool,
}

/// Target submenu items: every language except `base`, in [En, Vi, Ja, Ko] order. A checked
/// language carries its output order, e.g. `en (1)`.
pub fn target_menu_items(base: Lang, targets: &[Lang]) -> Vec<TargetMenuItem> {
    let selected = normalize_targets(base, targets);
    TARGET_ORDER
        .iter()
        .filter(|lang| **lang != base)
        .map(|&lang| match selected.iter().position(|l| *l == lang) {
            Some(i) => TargetMenuItem {
                lang,
                label: format!("{} ({})", lang.code(), i + 1),
                checked: true,
            },
            None => TargetMenuItem {
                lang,
                label: lang.code().to_string(),
                checked: false,
            },
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    ToggleTarget(Lang),
    OpenDictionary,
    OpenSettings,
    SetPaused(bool),
    Exit,
}

fn target_id(lang: Lang) -> String {
    format!("{ID_TARGET_PREFIX}{}", lang.code())
}

/// Maps a menu item id to its command. `paused` is the item's check state after the click
/// (only used by the Pause item).
fn command_for_id(id: &str, paused: bool) -> Option<TrayCommand> {
    match id {
        ID_DICTIONARY => Some(TrayCommand::OpenDictionary),
        ID_SETTINGS => Some(TrayCommand::OpenSettings),
        ID_PAUSE => Some(TrayCommand::SetPaused(paused)),
        ID_EXIT => Some(TrayCommand::Exit),
        _ => {
            let code = id.strip_prefix(ID_TARGET_PREFIX)?;
            Lang::ALL
                .iter()
                .find(|l| l.code() == code)
                .map(|&l| TrayCommand::ToggleTarget(l))
        }
    }
}

/// 32x32 RGBA: transparent background with an antialiased filled circle `#2563EB`.
/// The white "CT" lettering of v5 is not drawn (no font rasterizer; placeholder icon).
fn icon_rgba(size: u32) -> Vec<u8> {
    let c = size as f32 / 2.0;
    let radius = c - 0.5;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - c;
            let dy = y as f32 + 0.5 - c;
            let dist = (dx * dx + dy * dy).sqrt();
            let coverage = (radius - dist + 0.5).clamp(0.0, 1.0);
            rgba.extend_from_slice(&[0x25, 0x63, 0xEB, (coverage * 255.0).round() as u8]);
        }
    }
    rgba
}

fn make_icon() -> Result<Icon, String> {
    Icon::from_rgba(icon_rgba(32), 32, 32).map_err(|e| format!("tray icon: {e}"))
}

pub struct Tray {
    _icon: TrayIcon,
    target_menu: Submenu,
    pause_item: CheckMenuItem,
    /// Items currently in the target submenu, kept so they can be removed on refresh.
    target_items: RefCell<Vec<CheckMenuItem>>,
    /// Last model applied; re-applied after a click so a refused toggle snaps back.
    model: RefCell<Vec<TargetMenuItem>>,
}

impl Tray {
    pub fn new(initial: &UserSettings) -> Result<Tray, String> {
        let menu = Menu::new();
        let target_menu = Submenu::new("Target Language", true);
        let dictionary = MenuItem::with_id(
            MenuId::new(ID_DICTIONARY),
            "Personal Dictionary",
            true,
            None,
        );
        let settings = MenuItem::with_id(MenuId::new(ID_SETTINGS), "Settings", true, None);
        let pause_item = CheckMenuItem::with_id(
            MenuId::new(ID_PAUSE),
            "Pause Translation",
            true,
            false,
            None,
        );
        let exit = MenuItem::with_id(MenuId::new(ID_EXIT), "Exit", true, None);
        menu.append_items(&[&target_menu, &dictionary, &settings, &pause_item, &exit])
            .map_err(|e| format!("tray menu: {e}"))?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(TOOLTIP)
            .with_icon(make_icon()?)
            .build()
            .map_err(|e| format!("tray build: {e}"))?;

        let tray = Tray {
            _icon: icon,
            target_menu,
            pause_item,
            target_items: RefCell::new(Vec::new()),
            model: RefCell::new(Vec::new()),
        };
        tray.refresh(initial);
        Ok(tray)
    }

    /// Rebuilds the Target submenu labels and checks from `settings` (P6). The Pause item
    /// is left alone.
    pub fn refresh(&self, settings: &UserSettings) {
        let model = target_menu_items(settings.base_language, &settings.target_languages);
        self.apply_model(&model);
        *self.model.borrow_mut() = model;
    }

    fn apply_model(&self, model: &[TargetMenuItem]) {
        let mut items = self.target_items.borrow_mut();
        for old in items.drain(..) {
            let _ = self.target_menu.remove(&old);
        }
        for m in model {
            let item = CheckMenuItem::with_id(
                MenuId::new(target_id(m.lang)),
                &m.label,
                true,
                m.checked,
                None,
            );
            if self.target_menu.append(&item).is_ok() {
                items.push(item);
            }
        }
    }

    /// Drains pending tray/menu events into commands. Call from a `slint::Timer`.
    pub fn poll(&self) -> Vec<TrayCommand> {
        while TrayIconEvent::receiver().try_recv().is_ok() {}
        let mut commands = Vec::new();
        let mut reset_targets = false;
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            let id = ev.id.as_ref();
            if let Some(cmd) = command_for_id(id, self.pause_item.is_checked()) {
                reset_targets |= matches!(cmd, TrayCommand::ToggleTarget(_));
                commands.push(cmd);
            }
        }
        if reset_targets {
            // muda flips a check item on click; restore the stored state until the caller
            // saves the new settings and calls `refresh`.
            let model = self.model.borrow().clone();
            self.apply_model(&model);
        }
        commands
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_excludes_base_and_numbers_checked_in_order() {
        let items = target_menu_items(Lang::Ko, &[Lang::Ja, Lang::En]);
        let view: Vec<(Lang, &str, bool)> = items
            .iter()
            .map(|i| (i.lang, i.label.as_str(), i.checked))
            .collect();
        assert_eq!(
            view,
            vec![
                (Lang::En, "en (2)", true),
                (Lang::Vi, "vi", false),
                (Lang::Ja, "ja (1)", true),
            ]
        );
    }

    #[test]
    fn menu_with_other_base_includes_ko_last() {
        let items = target_menu_items(Lang::En, &[Lang::Ko]);
        let langs: Vec<Lang> = items.iter().map(|i| i.lang).collect();
        assert_eq!(langs, vec![Lang::Vi, Lang::Ja, Lang::Ko]);
        assert_eq!(items[2].label, "ko (1)");
    }

    #[test]
    fn ids_map_to_commands() {
        assert_eq!(
            command_for_id(&target_id(Lang::Vi), false),
            Some(TrayCommand::ToggleTarget(Lang::Vi))
        );
        assert_eq!(
            command_for_id("dictionary", false),
            Some(TrayCommand::OpenDictionary)
        );
        assert_eq!(
            command_for_id("settings", false),
            Some(TrayCommand::OpenSettings)
        );
        assert_eq!(
            command_for_id("pause", true),
            Some(TrayCommand::SetPaused(true))
        );
        assert_eq!(
            command_for_id("pause", false),
            Some(TrayCommand::SetPaused(false))
        );
        assert_eq!(command_for_id("exit", false), Some(TrayCommand::Exit));
        assert_eq!(command_for_id("target:xx", false), None);
        assert_eq!(command_for_id("other", false), None);
    }

    #[test]
    fn icon_is_transparent_corner_and_blue_center() {
        let px = icon_rgba(32);
        assert_eq!(px.len(), 32 * 32 * 4);
        assert_eq!(px[3], 0, "corner transparent");
        let center = (16 * 32 + 16) * 4;
        assert_eq!(&px[center..center + 4], &[0x25, 0x63, 0xEB, 0xFF]);
    }
}
