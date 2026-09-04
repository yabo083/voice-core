//! The native caption, painted in the app's colours.
//!
//! The alternative was `decorations: false` plus a caption drawn in HTML, which is
//! what most Tauri apps with a custom title bar do. It was rejected: an undecorated
//! window has to re-implement resize borders, double-click-to-maximise, Windows 11
//! snap layouts on maximise hover, and the accessibility affordances of the real
//! caption buttons - four things Windows already does correctly and none of which are
//! this product's problem.
//!
//! DWM has exposed the three colours since Windows 11 22000, so the whole job is
//! three attribute writes: caption background, caption text, window border. The
//! buttons stay native and keep their hit-testing; only the paint changes. On an
//! older build the calls fail and the window keeps the system caption, which is the
//! correct degradation - the app is still perfectly usable, just less integrated.

#[cfg(windows)]
pub fn paint(window: &tauri::WebviewWindow) {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
    };

    // COLORREF is 0x00BBGGRR. Every value here is a neutral grey, so the byte order
    // is a non-issue - which is also why they are written as the same hex the CSS uses.
    // --bg-rail, so the caption reads as the rail continuing upward rather than as a
    // separate bar sitting on top of the app.
    const CAPTION: u32 = 0x0016_1616;
    // --ink.
    const TEXT: u32 = 0x00DA_DADA;
    // Between --bg-raised and --line-strong: visible against a dark desktop, invisible
    // against a light one, which is the same job the system border does.
    const BORDER: u32 = 0x002B_2B2B;

    let Ok(handle) = window.hwnd() else { return };
    let hwnd = handle.0 as HWND;
    for (attribute, value) in [
        (DWMWA_CAPTION_COLOR, CAPTION),
        (DWMWA_TEXT_COLOR, TEXT),
        (DWMWA_BORDER_COLOR, BORDER),
    ] {
        // SAFETY: `hwnd` is the live window's handle and the pointer is a `u32` of the
        // size the attribute expects. A pre-22000 build returns an error, which is the
        // documented "attribute unknown" path and needs no handling beyond ignoring it.
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                attribute as u32,
                std::ptr::addr_of!(value).cast(),
                std::mem::size_of::<u32>() as u32,
            );
        }
    }
}

#[cfg(not(windows))]
pub fn paint(_window: &tauri::WebviewWindow) {}
