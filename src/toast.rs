//! On-screen toast: a small always-on-top message near the bottom of the screen ("Voice: Robot",
//! "Effects off") after hotkeys and tray actions.
//!
//! Plain Win32 on its own thread, so it works while the main window is hidden in the tray. It
//! never takes focus and lets clicks pass through, so it can't disturb a game. (Exclusive
//! fullscreen games draw over everything, so it won't show there; borderless/windowed is fine.)

use std::sync::Mutex;
use std::sync::mpsc;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DT_CALCRECT, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, HFONT, InvalidateRect, PAINTSTRUCT, ReleaseDC, SelectObject,
    SetBkMode, SetTextColor, SetWindowRgn, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HWND_TOPMOST, KillTimer, LWA_ALPHA, MSG,
    PostMessageW, RegisterClassW, SPI_GETWORKAREA, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow,
    SystemParametersInfoW, TranslateMessage, WM_APP, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

const WM_SHOW_TOAST: u32 = WM_APP + 1;
const TIMER_HIDE: usize = 1;
const SHOW_MS: u32 = 1600;

/// Text the window thread should display next.
static TEXT: Mutex<String> = Mutex::new(String::new());

pub struct Toaster {
    hwnd: isize,
}

impl Toaster {
    /// Create the (hidden) toast window on its own thread. `None` if Windows refused.
    pub fn start() -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("toast".into())
            .spawn(move || {
                // SAFETY: standard window class/window creation and message loop on this thread.
                unsafe {
                    let instance = GetModuleHandleW(None).unwrap_or_default();
                    let class = WNDCLASSW {
                        lpfnWndProc: Some(wnd_proc),
                        hInstance: instance.into(),
                        lpszClassName: w!("VoiceChangerToast"),
                        ..Default::default()
                    };
                    RegisterClassW(&class);
                    let hwnd = CreateWindowExW(
                        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
                        w!("VoiceChangerToast"),
                        PCWSTR::null(),
                        WS_POPUP,
                        0,
                        0,
                        0,
                        0,
                        None,
                        None,
                        Some(instance.into()),
                        None,
                    );
                    let Ok(hwnd) = hwnd else {
                        let _ = tx.send(None);
                        return;
                    };
                    let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 235, LWA_ALPHA);
                    let _ = tx.send(Some(hwnd.0 as isize));
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            })
            .ok()?;
        rx.recv().ok().flatten().map(|hwnd| Self { hwnd })
    }

    pub fn show(&self, text: &str) {
        if let Ok(mut t) = TEXT.lock() {
            *t = text.to_string();
        }
        // SAFETY: posting a message to our own window; it is valid for the process lifetime.
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_SHOW_TOAST, WPARAM(0), LPARAM(0));
        }
    }
}

fn scale(px: i32) -> i32 {
    // SAFETY: trivial query.
    let dpi = unsafe { GetDpiForSystem() } as i32;
    px * dpi.max(96) / 96
}

fn toast_font() -> HFONT {
    // SAFETY: creating a GDI font; callers select it and delete it after use.
    unsafe {
        CreateFontW(
            scale(20),
            0,
            0,
            0,
            600,
            0,
            0,
            0,
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            0,
            w!("Segoe UI"),
        )
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY (whole function): called by Windows on the toast thread with a valid hwnd.
    unsafe {
        match msg {
            WM_SHOW_TOAST => {
                let mut work = RECT::default();
                let _ = SystemParametersInfoW(
                    SPI_GETWORKAREA,
                    0,
                    Some(&mut work as *mut _ as *mut _),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                );
                // Fit the text: measured width plus padding, within sensible bounds.
                let text: Vec<u16> = TEXT.lock().map(|t| t.encode_utf16().collect()).unwrap_or_default();
                let dc = GetDC(Some(hwnd));
                let font = toast_font();
                let old = SelectObject(dc, font.into());
                let mut measure = RECT::default();
                let mut t = text.clone();
                DrawTextW(dc, &mut t, &mut measure, DT_CALCRECT | DT_SINGLELINE);
                SelectObject(dc, old);
                let _ = DeleteObject(font.into());
                ReleaseDC(Some(hwnd), dc);
                let max_w = ((work.right - work.left) as f32 * 0.9) as i32;
                let (w, h) = ((measure.right - measure.left + scale(48)).clamp(scale(220), max_w), scale(52));
                let x = work.left + (work.right - work.left - w) / 2;
                let y = work.bottom - h - scale(64);
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
                let r = scale(14);
                SetWindowRgn(hwnd, Some(CreateRoundRectRgn(0, 0, w + 1, h + 1, r, r)), true);
                let _ = InvalidateRect(Some(hwnd), None, true);
                let _ = KillTimer(Some(hwnd), TIMER_HIDE);
                SetTimer(Some(hwnd), TIMER_HIDE, SHOW_MS, None);
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_HIDE => {
                let _ = KillTimer(Some(hwnd), TIMER_HIDE);
                let _ = ShowWindow(hwnd, SW_HIDE);
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rect = ps.rcPaint;
                let bg = CreateSolidBrush(COLORREF(0x00302A28)); // BGR: dark slate
                FillRect(hdc, &rect, bg);
                let _ = DeleteObject(bg.into());
                let font = toast_font();
                let old = SelectObject(hdc, font.into());
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(0x00FFFFFF));
                let mut text: Vec<u16> = TEXT.lock().map(|t| t.encode_utf16().collect()).unwrap_or_default();
                DrawTextW(hdc, &mut text, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                SelectObject(hdc, old);
                let _ = DeleteObject(font.into());
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
