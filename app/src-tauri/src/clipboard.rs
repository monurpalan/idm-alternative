//! Watching the clipboard for copied download links, like IDM. Windows tells a listener
//! window about every change (`AddClipboardFormatListener`), so nothing is polled; the
//! listener only exists while the setting is on.

use idma_manager::Category;

/// The link in copied text worth offering: one http(s) URL whose file name has a type IDM
/// would take (archives, documents, music, programs, video), so copying an ordinary page
/// address does nothing.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "clipboard watching is Windows-only")
)]
pub fn link_in(text: &str) -> Option<String> {
    let text = text.trim();
    if text.len() > 4096 || text.contains(char::is_whitespace) {
        return None;
    }
    let url = url::Url::parse(text).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    let name = url.path_segments()?.next_back()?;
    (Category::for_name(name) != Category::General).then(|| url.to_string())
}

#[cfg(windows)]
pub use windows::Watcher;

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    use std::cell::RefCell;
    use std::sync::mpsc;
    use std::thread::JoinHandle;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::DataExchange::{
        AddClipboardFormatListener, CloseClipboard, GetClipboardData, OpenClipboard,
        RemoveClipboardFormatListener,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        HWND_MESSAGE, KillTimer, MSG, PostMessageW, PostQuitMessage, RegisterClassW, SetTimer,
        WM_CLIPBOARDUPDATE, WM_CLOSE, WM_DESTROY, WM_TIMER, WNDCLASSW,
    };

    const CF_UNICODETEXT: u32 = 13;

    /// How long after a change the clipboard is read. The program that copied may still be
    /// working on it right after the notice (.NET, for one, opens it again to flush), and
    /// opening it then would make that program's copy fail. Changes in between are read once.
    const SETTLE_MS: u32 = 150;
    const READ_TIMER: usize = 1;

    type OnText = Box<dyn Fn(String)>;

    thread_local! {
        /// The listener thread's callback, for the window procedure.
        static ON_TEXT: RefCell<Option<OnText>> = RefCell::new(None);
    }

    /// A listener on its own thread; dropping it stops listening.
    pub struct Watcher {
        hwnd: usize,
        thread: Option<JoinHandle<()>>,
    }

    impl Watcher {
        /// Calls `on_text` with the clipboard's text after every change.
        pub fn start(on_text: impl Fn(String) + Send + 'static) -> std::io::Result<Self> {
            let (tx, rx) = mpsc::channel();
            let thread = std::thread::Builder::new()
                .name("clipboard".into())
                .spawn(move || {
                    ON_TEXT.with(|c| *c.borrow_mut() = Some(Box::new(on_text)));
                    // SAFETY: plain Win32 calls on this thread's own window; the class name
                    // and window name are NUL-terminated and outlive the calls.
                    unsafe {
                        let class: Vec<u16> = "idma-clipboard\0".encode_utf16().collect();
                        let instance = GetModuleHandleW(std::ptr::null());
                        let wc = WNDCLASSW {
                            lpfnWndProc: Some(window_proc),
                            hInstance: instance,
                            lpszClassName: class.as_ptr(),
                            ..std::mem::zeroed()
                        };
                        RegisterClassW(&wc);
                        let hwnd = CreateWindowExW(
                            0,
                            class.as_ptr(),
                            class.as_ptr(),
                            0,
                            0,
                            0,
                            0,
                            0,
                            HWND_MESSAGE,
                            std::ptr::null_mut(),
                            instance,
                            std::ptr::null(),
                        );
                        if hwnd.is_null() || AddClipboardFormatListener(hwnd) == 0 {
                            tx.send(Err(std::io::Error::last_os_error())).ok();
                            return;
                        }
                        tx.send(Ok(hwnd as usize)).ok();
                        let mut msg: MSG = std::mem::zeroed();
                        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                            DispatchMessageW(&msg);
                        }
                    }
                })?;
            let hwnd = rx
                .recv()
                .map_err(|_| std::io::Error::other("the clipboard thread ended"))??;
            Ok(Self {
                hwnd,
                thread: Some(thread),
            })
        }
    }

    impl Drop for Watcher {
        fn drop(&mut self) {
            // SAFETY: the window belongs to the listener thread, which handles WM_CLOSE.
            unsafe {
                PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
            }
            if let Some(t) = self.thread.take() {
                t.join().ok();
            }
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: called by Windows on the listener thread for its own window.
        unsafe {
            match msg {
                WM_CLIPBOARDUPDATE => {
                    // Restarts the timer when it is running.
                    SetTimer(hwnd, READ_TIMER, SETTLE_MS, None);
                    0
                }
                WM_TIMER if wparam == READ_TIMER => {
                    KillTimer(hwnd, READ_TIMER);
                    if let Some(text) = read_text(hwnd) {
                        ON_TEXT.with(|c| {
                            if let Some(f) = c.borrow().as_ref() {
                                f(text);
                            }
                        });
                    }
                    0
                }
                WM_CLOSE => {
                    RemoveClipboardFormatListener(hwnd);
                    DestroyWindow(hwnd);
                    0
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    0
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
    }

    /// The clipboard's text, if it has some. Another program may hold the clipboard for a
    /// moment right after changing it, so opening is tried a few times.
    unsafe fn read_text(hwnd: HWND) -> Option<String> {
        // SAFETY: the clipboard is opened by this window and closed on every path; the
        // locked memory is read only up to its NUL and unlocked before closing.
        unsafe {
            let mut opened = false;
            for _ in 0..5 {
                if OpenClipboard(hwnd) != 0 {
                    opened = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            if !opened {
                return None;
            }
            let mut text = None;
            let handle = GetClipboardData(CF_UNICODETEXT);
            if !handle.is_null() {
                let ptr = GlobalLock(handle) as *const u16;
                if !ptr.is_null() {
                    let mut len = 0;
                    // Links are short; longer text is not a link.
                    while len < 8192 && *ptr.add(len) != 0 {
                        len += 1;
                    }
                    text = Some(String::from_utf16_lossy(std::slice::from_raw_parts(
                        ptr, len,
                    )));
                    GlobalUnlock(handle);
                }
            }
            CloseClipboard();
            text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_download_links_only() {
        assert_eq!(
            link_in("  https://example.com/files/setup.exe \r\n").as_deref(),
            Some("https://example.com/files/setup.exe")
        );
        assert!(link_in("http://cdn.example.org/a/film.mkv?token=1").is_some());
        assert!(link_in("https://example.com/").is_none());
        assert!(link_in("https://example.com/blog/post").is_none());
        assert!(link_in("https://example.com/page.html").is_none());
        assert!(link_in("see https://example.com/a.zip").is_none());
        assert!(link_in("ftp://example.com/a.zip").is_none());
        assert!(link_in("C:\\Users\\a.zip").is_none());
    }
}
