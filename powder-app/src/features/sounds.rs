//! Powder's feedback sounds (bookmark, recording saved ...). The original played bundled WAVs.
pub fn play(kind: &str) {
    #[cfg(windows)]
    {
        // PlaySound with the system default sounds keeps this dependency-free
        use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONASTERISK, MB_OK};
        unsafe {
            MessageBeep(if kind == "notification" { MB_OK } else { MB_ICONASTERISK });
        }
    }
    #[cfg(not(windows))]
    let _ = kind;
}
