//! The tray icon. The app keeps running there when its window is closed (if the settings say
//! so); its menu shows the window, sets a quick speed limit, starts or pauses everything, and
//! quits.

use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Wry};

/// Speed limits offered in the tray menu, bytes per second.
pub const LIMITS: [(Option<u64>, &str); 5] = [
    (None, "No limit"),
    (Some(256 << 10), "256 KB/s"),
    (Some(1 << 20), "1 MB/s"),
    (Some(4 << 20), "4 MB/s"),
    (Some(16 << 20), "16 MB/s"),
];

/// Menu item ids.
pub const SHOW: &str = "show";
pub const START_ALL: &str = "start_all";
pub const PAUSE_ALL: &str = "pause_all";
pub const QUIT: &str = "quit";
/// Followed by the index into [`LIMITS`].
pub const LIMIT_PREFIX: &str = "limit:";

pub struct Tray {
    limits: Vec<(Option<u64>, CheckMenuItem<Wry>)>,
}

impl Tray {
    /// Ticks the entry for `limit`; none when it is not one of the presets.
    pub fn show_limit(&self, limit: Option<u64>) {
        for (l, item) in &self.limits {
            item.set_checked(*l == limit).ok();
        }
    }
}

/// Builds the tray icon, or, when it exists, replaces its menu (a new language).
pub fn build(app: &AppHandle, limit: Option<u64>, lang: &str) -> tauri::Result<Tray> {
    let tr = |text: &str| crate::i18n::t(lang, text, &[]);
    let limits = LIMITS
        .iter()
        .enumerate()
        .map(|(i, (l, label))| {
            CheckMenuItem::with_id(
                app,
                format!("{LIMIT_PREFIX}{i}"),
                tr(label),
                true,
                *l == limit,
                None::<&str>,
            )
            .map(|item| (*l, item))
        })
        .collect::<tauri::Result<Vec<_>>>()?;
    let limit_items: Vec<&dyn IsMenuItem<Wry>> = limits
        .iter()
        .map(|(_, i)| i as &dyn IsMenuItem<Wry>)
        .collect();
    let limit_menu = Submenu::with_items(app, tr("Speed limit"), true, &limit_items)?;

    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, SHOW, tr("Show idm-alternative"), true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &limit_menu,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, START_ALL, tr("Start all"), true, None::<&str>)?,
            &MenuItem::with_id(app, PAUSE_ALL, tr("Pause all"), true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, QUIT, tr("Quit"), true, None::<&str>)?,
        ],
    )?;

    if let Some(existing) = app.tray_by_id("main") {
        existing.set_menu(Some(menu))?;
        return Ok(Tray { limits });
    }
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("idm-alternative")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| crate::on_tray_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(Tray { limits })
}
