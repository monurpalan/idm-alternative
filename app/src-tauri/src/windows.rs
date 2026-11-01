//! The windows: the main one (made when shown, dropped in the tray), the progress
//! windows, messages to the page, and the tray menu.

use super::*;

/// WebView2 switches of every window; they share one browser process, so all must pass the
/// same ones. The first three features are what wry turns off by default. The rest run the
/// network in the browser process, one renderer for all windows and no background updates.
/// Drawing stays on the GPU: in software (or with `--enable-low-end-device-mode`, or with the
/// GPU in the browser process) colours lose the monitor's colour profile and look different.
pub(crate) const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
    --enable-features=NetworkServiceInProcess2 --renderer-process-limit=1 \
    --disable-background-networking --disable-component-update";

/// Asks WebView2 to give memory back while the window is minimized (`low`), and to work as
/// usual again once it is shown. What is on screen does not change.
#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn memory_target(window: &tauri::WebviewWindow, low: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
        ICoreWebView2_19,
    };
    use windows_core::Interface;
    let level = if low {
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
    } else {
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
    };
    window
        .with_webview(move |w| {
            // SAFETY: COM calls on the WebView's own thread, where `with_webview` runs this.
            let set = unsafe {
                w.controller()
                    .CoreWebView2()
                    .and_then(|c| c.cast::<ICoreWebView2_19>())
                    .and_then(|c| c.SetMemoryUsageTargetLevel(level))
            };
            if let Err(e) = set {
                tracing::debug!("could not set the WebView's memory target: {e}");
            }
        })
        .ok();
}

/// Brings the window back, also from the tray.
///
/// In the tray the window does not exist (its WebView would keep 160 MB for nothing), so it
/// is built again here; the page then asks for what it missed (`page_ready`).
pub(crate) fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        w.show().ok();
        w.unminimize().ok();
        w.set_focus().ok();
        return;
    }
    let Some(config) = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .cloned()
    else {
        return;
    };
    // Built off the main thread: building a window in the tray or single-instance callbacks,
    // which run on it, would deadlock on Windows.
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if app.get_webview_window("main").is_some() {
            return;
        }
        let built = tauri::WebviewWindowBuilder::from_config(&app, &config)
            .and_then(|b| b.additional_browser_args(BROWSER_ARGS).build());
        match built {
            Ok(w) => {
                w.set_focus().ok();
            }
            Err(e) => tracing::error!("could not open the window: {e}"),
        }
    });
}

/// At most this many progress windows open by themselves: starting a long queue must not
/// cover the screen.
pub(crate) const MAX_PROGRESS_WINDOWS: usize = 4;

/// The small progress window of download `id` (see `routes/progress`). `asked`: the user
/// asked for it, so the limit does not apply.
pub(crate) fn open_progress(handle: &AppHandle, id: TaskId, asked: bool) {
    let label = format!("progress-{id}");
    if let Some(w) = handle.get_webview_window(&label) {
        w.show().ok();
        w.set_focus().ok();
        return;
    }
    let open = handle
        .webview_windows()
        .keys()
        .filter(|l| l.starts_with("progress-"))
        .count();
    if !asked && open >= MAX_PROGRESS_WINDOWS {
        return;
    }
    let handle = handle.clone();
    // Not on the main thread, for the same reason as `show_main`.
    tauri::async_runtime::spawn(async move {
        let built = tauri::WebviewWindowBuilder::new(
            &handle,
            &label,
            tauri::WebviewUrl::App(format!("progress?id={id}").into()),
        )
        .title(i18n::t(lang(&handle), "Download", &[]))
        .inner_size(440.0, 288.0)
        .resizable(false)
        .maximizable(false)
        .additional_browser_args(BROWSER_ARGS)
        .build();
        if let Err(e) = built {
            tracing::warn!("could not open the progress window: {e}");
        }
    });
}

/// Opens the progress window of a download from the list.
#[tauri::command]
pub(crate) fn show_progress(handle: AppHandle, id: TaskId) {
    open_progress(&handle, id, true);
}

/// Events the page must not miss while it is closed or still loading.
#[derive(Default)]
pub(crate) struct Page {
    pub(crate) ready: bool,
    pub(crate) waiting: Vec<(&'static str, serde_json::Value)>,
}

/// Sends `event` to the page now if it is open, otherwise when it has loaded; `show` brings
/// the window up. For what the user has to see (a captured download, a countdown, a notice);
/// the list and the settings the page reads afresh when it opens.
pub(crate) fn to_page(
    handle: &AppHandle,
    event: &'static str,
    payload: impl Serialize,
    show: bool,
) {
    let payload = serde_json::to_value(payload).unwrap_or_default();
    let app = handle.state::<App>();
    let ready = {
        let mut page = lock(&app.page);
        if !page.ready {
            // Days in the tray must not pile up notices without end.
            if page.waiting.len() >= 20 {
                page.waiting.remove(0);
            }
            page.waiting.push((event, payload.clone()));
        }
        page.ready
    };
    if ready {
        handle.emit(event, payload).ok();
    }
    if show {
        show_main(handle);
    }
}

/// The page has loaded and listens: hands it what came while it was closed.
#[tauri::command]
pub(crate) fn page_ready(handle: AppHandle, app: State<'_, App>) {
    // Also what the Linux smoke test waits for: the window's page loaded and reached the app.
    tracing::info!("the window's page is ready");
    let waiting = {
        let mut page = lock(&app.page);
        page.ready = true;
        std::mem::take(&mut page.waiting)
    };
    for (event, payload) in waiting {
        handle.emit(event, payload).ok();
    }
}

pub(crate) fn on_tray_menu(app: &AppHandle, id: &str) {
    let manager = app.state::<App>().manager.clone();
    match id {
        tray::SHOW => show_main(app),
        tray::START_ALL => {
            tauri::async_runtime::spawn(async move { manager.start_all().await.ok() });
        }
        tray::PAUSE_ALL => {
            tauri::async_runtime::spawn(async move { manager.pause_all().await.ok() });
        }
        // The page asks for the file: the dialog is there.
        tray::ORIGIN => to_page(app, "origin-pick", (), true),
        tray::QUIT => app.exit(0),
        id => {
            let Some((limit, _)) = id
                .strip_prefix(tray::LIMIT_PREFIX)
                .and_then(|i| i.parse::<usize>().ok())
                .and_then(|i| tray::LIMITS.get(i))
            else {
                return;
            };
            let settings = Settings {
                speed_limit: *limit,
                ..lock(&app.state::<App>().settings).clone()
            };
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = apply_settings(&handle, settings).await {
                    tracing::warn!("could not set the speed limit: {e}");
                }
            });
        }
    }
}
