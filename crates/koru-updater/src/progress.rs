// SPDX-License-Identifier: MPL-2.0

//! The small surface that owns an update after Koru has closed.
//!
//! The updater cannot reuse Koru's drawn window: its whole purpose is to keep
//! running while that executable is no longer on disk.  Task Dialog gives the
//! helper a compact native surface and a real marquee progress control without
//! starting a second winit event loop just to say that replacement is under way.

/// Keep a compact progress window visible while `work` replaces or restarts
/// Koru. Linux has no native surface here yet — no Task Dialog, and nothing
/// this helper can rely on every desktop having — so the swap runs silently
/// and the seconds with no Koru are unannounced; the work still runs and
/// reports its normal error.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn show(_title: &str, _detail: &str, work: impl FnOnce() + Send + 'static) {
    work();
}

/// macOS has no Task Dialog and an `NSAlert` needs a main-thread run loop
/// this helper does not have, so the person is told through a notification
/// banner instead — posted by `osascript`, which owns its own process — and
/// the swap runs underneath it. A bundle swap is two renames, so the banner
/// is usually still on screen when Koru reopens; the point is that the
/// seconds with no Koru in the Dock are announced rather than silent.
/// Nothing here can fail the update: a refused or missing notification is
/// logged and the work runs regardless.
#[cfg(target_os = "macos")]
pub fn show(title: &str, detail: &str, work: impl FnOnce() + Send + 'static) {
    let script = notification_script(title, detail);
    if let Err(error) = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        eprintln!("koru-updater: could not post the progress notification: {error}");
    }
    work();
}

/// The AppleScript for the banner. Pure, so the quoting is testable — the
/// title and detail are this binary's own strings today, but a quote in
/// either must never end the literal.
#[cfg(any(target_os = "macos", test))]
pub fn notification_script(title: &str, detail: &str) -> String {
    fn literal(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for c in text.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                _ => out.push(c),
            }
        }
        out.push('"');
        out
    }
    format!(
        "display notification {} with title {}",
        literal(detail),
        literal(title)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notification_script_keeps_text_inside_its_literals() {
        let script = notification_script(r#"Koru "update""#, r#"a \ b"#);
        assert_eq!(
            script,
            r#"display notification "a \\ b" with title "Koru \"update\"""#
        );
    }

    /// The script parses: `osacompile` compiles it without posting anything.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_notification_script_compiles() {
        let out = std::env::temp_dir().join("koru-updater-notification-test.scpt");
        let output = std::process::Command::new("/usr/bin/osacompile")
            .args([
                "-o",
                &out.display().to_string(),
                "-e",
                &notification_script("Koru update", "Installing."),
            ])
            .output()
            .expect("osacompile runs");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(windows)]
pub fn show(title: &str, detail: &str, work: impl FnOnce() + Send + 'static) {
    use std::ffi::c_void;
    use std::thread;
    use windows_sys::Win32::Foundation::{FreeLibrary, HWND, LPARAM, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::UI::Controls::{
        TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TASKDIALOGCONFIG_1, TDF_ALLOW_DIALOG_CANCELLATION,
        TDF_SHOW_MARQUEE_PROGRESS_BAR, TDM_SET_PROGRESS_BAR_MARQUEE, TDN_CREATED,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

    type Work = Box<dyn FnOnce() + Send>;
    type TaskDialog =
        unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut i32) -> i32;

    unsafe extern "system" fn callback(
        hwnd: HWND,
        notification: i32,
        _wparam: WPARAM,
        _lparam: LPARAM,
        data: isize,
    ) -> i32 {
        if notification == TDN_CREATED {
            // The dialog is now alive, so starting the worker here guarantees
            // there is something visible before the replacing process begins.
            let work = unsafe { &mut *(data as *mut Option<Work>) }.take();
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
                    hwnd,
                    TDM_SET_PROGRESS_BAR_MARQUEE as u32,
                    1,
                    0,
                );
            }
            if let Some(work) = work {
                // Raw Win32 handles are pointers in `windows-sys` and therefore
                // are not `Send`; their numeric value is safe to carry to the
                // worker and is converted back only for the final close post.
                let hwnd = hwnd as usize;
                thread::spawn(move || {
                    work();
                    // The actual result is reported by the updater process; a
                    // finished helper simply takes its progress surface away.
                    unsafe {
                        PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0);
                    }
                });
            }
        }
        0
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    let title = wide(title);
    let instruction = wide("Updating Koru");
    let detail = wide(detail);
    let mut work: Option<Work> = Some(Box::new(work));
    let config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: std::ptr::null_mut(),
        hInstance: std::ptr::null_mut(),
        dwFlags: TDF_SHOW_MARQUEE_PROGRESS_BAR | TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: 0,
        pszWindowTitle: title.as_ptr(),
        Anonymous1: TASKDIALOGCONFIG_0 {
            hMainIcon: std::ptr::null_mut(),
        },
        pszMainInstruction: instruction.as_ptr(),
        pszContent: detail.as_ptr(),
        cButtons: 0,
        pButtons: std::ptr::null(),
        nDefaultButton: 0,
        cRadioButtons: 0,
        pRadioButtons: std::ptr::null(),
        nDefaultRadioButton: 0,
        pszVerificationText: std::ptr::null(),
        pszExpandedInformation: std::ptr::null(),
        pszExpandedControlText: std::ptr::null(),
        pszCollapsedControlText: std::ptr::null(),
        Anonymous2: TASKDIALOGCONFIG_1 {
            hFooterIcon: std::ptr::null_mut(),
        },
        pszFooter: std::ptr::null(),
        pfCallback: Some(callback),
        lpCallbackData: (&mut work as *mut Option<Work>) as *mut c_void as isize,
        // 0 asks the task-dialog manager for the ideal width for this content.
        // 360 dialog units (~750 px) was set when the helper was written and is
        // far wider than two short sentences need; letting Windows size it also
        // keeps it right if these strings are ever localized, where German runs
        // about a third longer than English.
        cxWidth: 0,
    };
    // TaskDialogIndirect belongs to Common Controls v6. Linking it directly
    // makes the loader reject this helper before `main` on older activation
    // contexts, so resolve it dynamically and retain a no-UI fallback.
    let library = wide("comctl32.dll");
    let module = unsafe { LoadLibraryW(library.as_ptr()) };
    let shown = if module.is_null() {
        Err(())
    } else {
        let procedure = unsafe { GetProcAddress(module, b"TaskDialogIndirect\0".as_ptr()) };
        let result = if let Some(procedure) = procedure {
            let dialog: TaskDialog = unsafe { std::mem::transmute(procedure) };
            Ok(unsafe {
                dialog(
                    &config,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            })
        } else {
            Err(())
        };
        unsafe { FreeLibrary(module) };
        result
    };
    if !matches!(shown, Ok(code) if code >= 0) {
        if let Some(work) = work.take() {
            work();
        }
    }
}
