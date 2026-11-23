//! Reading the desktop from behind the window.
//!
//! The mini card frosts what the screen looks like under it, which means seeing the screen
//! without itself in it. GDI copies the composed desktop out of the redirection surface
//! DWM keeps, and it leaves layered windows out of that copy unless the caller asks for
//! them by name — so while mini mode is on the card wears `WS_EX_LAYERED` with a fully
//! opaque alpha: it paints exactly as it did, and the pixels under it are the desktop's.
//!
//! Windows only. Elsewhere there is no such copy to make, and the caller falls back.

use std::sync::atomic::AtomicIsize;

/// The main window, from `main`. Kept as an integer: it is only ever turned back into a
/// handle on the UI thread, and only on Windows.
#[cfg_attr(not(windows), allow(dead_code))]
static WINDOW: AtomicIsize = AtomicIsize::new(0);

/// The window this app can read the screen behind.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn set_hwnd(hwnd: *mut std::ffi::c_void) {
    #[cfg(windows)]
    imp::init(hwnd as isize);
}

/// The window's client area in screen pixels: (x, y, w, h). None before the handle lands,
/// or where the platform has no screen to read.
pub fn client_rect() -> Option<(i32, i32, u32, u32)> {
    #[cfg(windows)]
    return imp::client_rect();
    #[cfg(not(windows))]
    None
}

/// Whether that rectangle is on a screen at all: a window parked off the desktop (the
/// position it sits at before its first placement) copies as pure black, and a diagnostic
/// that reads that copy would report a dead grab.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn on_screen(x: i32, y: i32, w: u32, h: u32) -> bool {
    #[cfg(windows)]
    return imp::on_screen(x, y, w, h);
    #[cfg(not(windows))]
    false
}

/// Keep the window out of screen grabs (false lets it back in). False where there is no
/// grab to hide from.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn hide_from_grab(on: bool) -> bool {
    #[cfg(windows)]
    return imp::hide(on);
    #[cfg(not(windows))]
    false
}

/// BGRA, top-down, `w` × `h` pixels of the screen at (x, y). None when the copy failed.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn grab(x: i32, y: i32, w: u32, h: u32) -> Option<(Vec<u8>, u32, u32)> {
    #[cfg(windows)]
    return imp::grab(x, y, w, h);
    #[cfg(not(windows))]
    None
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::Ordering;

    use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BitBlt, ClientToScreen, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SRCCOPY, SelectObject,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetClientRect, GetSystemMetrics, GetWindowLongW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongW, SetWindowPos, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_EX_LAYERED,
    };

    use super::WINDOW;

    fn hwnd() -> Option<HWND> {
        let h = WINDOW.load(Ordering::Relaxed);
        (h != 0).then(|| HWND(h as *mut _))
    }

    pub fn init(hwnd: isize) {
        WINDOW.store(hwnd, Ordering::Relaxed);
    }

    pub fn client_rect() -> Option<(i32, i32, u32, u32)> {
        let hwnd = hwnd()?;
        unsafe {
            let mut r = RECT::default();
            GetClientRect(hwnd, &mut r).ok()?;
            let mut p = POINT::default();
            if !ClientToScreen(hwnd, &mut p).as_bool() {
                return None;
            }
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            (w > 0 && h > 0).then_some((p.x, p.y, w as u32, h as u32))
        }
    }

    pub fn on_screen(x: i32, y: i32, w: u32, h: u32) -> bool {
        unsafe {
            let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
            let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
            x >= vx && y >= vy && (x + w as i32) <= vx + vw && (y + h as i32) <= vy + vh
        }
    }

    /// The card's window steps out of (and back into) screen copies.
    pub fn hide(on: bool) -> bool {
        let Some(hwnd) = hwnd() else { return false };
        unsafe {
            let layered = WS_EX_LAYERED.0 as i32;
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
            let want = if on { ex | layered } else { ex & !layered };
            if want != ex {
                SetWindowLongW(hwnd, GWL_EXSTYLE, want);
            }
            // An opaque alpha, asked for after the style: a layered window that has had
            // neither this nor `UpdateLayeredWindow` draws nothing at all.
            let ok = if on { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA).is_ok() } else { true };
            // Nothing moves or resizes; the compositor just takes the new style up.
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            ok
        }
    }

    /// A rectangle of the screen, copied out without the layered windows on it.
    pub fn grab(x: i32, y: i32, w: u32, h: u32) -> Option<(Vec<u8>, u32, u32)> {
        let (w, h) = (w as i32, h as i32);
        // A monitor's worth, and then some. Past this something is wrong (a 0×0 client, a
        // window stretched across four displays) and the copy isn't worth attempting.
        if !(1..=8192).contains(&w) || !(1..=8192).contains(&h) {
            return None;
        }
        unsafe {
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(Some(screen));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    // Negative: the rows come out top-down, the way the blur wants them.
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            if bits.is_null() {
                let _ = DeleteObject(bmp.into());
                let _ = DeleteDC(mem);
                let _ = ReleaseDC(None, screen);
                return None;
            }
            let old = SelectObject(mem, bmp.into());
            let copied = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY).is_ok();
            let out = copied.then(|| std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec());
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            let _ = ReleaseDC(None, screen);
            Some((out?, w as u32, h as u32))
        }
    }
}
