//! A minimal Win32 window, shared by both viewports.
//!
//! Raw Win32 rather than a windowing crate: the four-pane demo needs exact
//! control over position and decoration to tile cleanly, both APIs need the same
//! `HWND` contract, and this is a few hundred lines against a dependency that
//! would also pull in an event loop we do not want.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, LoadCursorW, PeekMessageW,
    PostQuitMessage, RegisterClassExW, SetWindowTextW, ShowWindow, TranslateMessage, CS_HREDRAW,
    CS_OWNDC, CS_VREDRAW, IDC_ARROW, MSG, PM_REMOVE, SW_SHOWNOACTIVATE, WM_CLOSE, WM_DESTROY,
    WNDCLASSEXW, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);
const CLASS_NAME: PCWSTR = w!("rusty_dds_sim_viewport");

pub struct Window {
    pub hwnd: HWND,
    pub width: u32,
    pub height: u32,
    open: bool,
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CLOSE => {
            // SAFETY: `hwnd` is the window this proc was invoked for.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: standard teardown; posts WM_QUIT to this thread's queue.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: forwarding to the default handler with the parameters given.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

impl Window {
    pub fn new(title: &str, x: i32, y: i32, width: u32, height: u32) -> windows::core::Result<Window> {
        // SAFETY: every call below is a documented Win32 entry point invoked
        // with valid, correctly-sized arguments; the window class is registered
        // exactly once per process.
        unsafe {
            let instance = GetModuleHandleW(None)?;
            if !CLASS_REGISTERED.swap(true, Ordering::SeqCst) {
                let class = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    style: CS_HREDRAW | CS_VREDRAW | CS_OWNDC,
                    lpfnWndProc: Some(wnd_proc),
                    hInstance: instance.into(),
                    hCursor: LoadCursorW(None, IDC_ARROW)?,
                    lpszClassName: CLASS_NAME,
                    ..Default::default()
                };
                if RegisterClassExW(&class) == 0 {
                    return Err(windows::core::Error::from_thread());
                }
            }

            let title_w = to_wide(title);
            let hwnd = CreateWindowExW(
                WS_EX_APPWINDOW,
                CLASS_NAME,
                PCWSTR(title_w.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                x,
                y,
                width as i32,
                height as i32,
                None,
                None,
                Some(instance.into()),
                None,
            )?;

            // NOACTIVATE: four panes opening at once must not fight over focus.
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            Ok(Window {
                hwnd,
                width,
                height,
                open: true,
            })
        }
    }

    pub fn set_title(&self, title: &str) {
        let w = to_wide(title);
        // SAFETY: `hwnd` is live while `self` is; `w` is NUL-terminated.
        unsafe {
            let _ = SetWindowTextW(self.hwnd, PCWSTR(w.as_ptr()));
        }
    }

    /// Drain pending messages. Returns `false` once the window has been closed.
    pub fn pump(&mut self) -> bool {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid out-param; PM_REMOVE consumes each message.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == 0x0012 {
                    // WM_QUIT
                    self.open = false;
                    return false;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        self.open
    }
}
