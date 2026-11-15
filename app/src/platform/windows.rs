// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! Windows: per-user file association and the IPC pipe's access control.
//!
//! Files opened from Explorer arrive in argv (`"rustydxf.exe" "%1"`), so no
//! special event handling is needed.

use anyhow::{Context, Result};
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use rustydxf_core::brand;
use std::io;
use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

const ICON: &[u8] = include_bytes!("../../../assets/icon.ico");

/// Writes the association under `HKCU\Software\Classes` (no admin rights).
pub fn register() -> Result<()> {
    let exe = std::env::current_exe().context("locating the executable")?;
    let exe = exe.display().to_string();
    let open_command = format!("\"{exe}\" \"%1\"");

    // The executable carries no icon resource, so install the .ico next to
    // the user's app data and point the association at it.
    let icon_ref = match install_icon() {
        Ok(path) => path,
        Err(e) => {
            log::warn!("could not install the document icon: {e}");
            format!("\"{exe}\",0")
        }
    };

    let classes = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey("Software\\Classes")
        .context("opening HKCU\\Software\\Classes")?
        .0;
    let prog_id = brand::WINDOWS_PROG_ID;

    // .dxf -> ProgID
    let (ext, _) = classes.create_subkey(".dxf")?;
    ext.set_value("", &prog_id)?;
    ext.set_value("Content Type", &"image/vnd.dxf")?;
    ext.create_subkey("OpenWithProgids")?
        .0
        .set_value(prog_id, &"")?;

    // ProgID
    let (prog, _) = classes.create_subkey(prog_id)?;
    prog.set_value("", &"DXF Drawing")?;
    prog.set_value("FriendlyTypeName", &"DXF Drawing")?;
    prog.create_subkey("DefaultIcon")?
        .0
        .set_value("", &icon_ref)?;
    prog.create_subkey("shell\\open\\command")?
        .0
        .set_value("", &open_command)?;

    // Application entry, so RustyDxf shows up in "Open with".
    let (app, _) = classes.create_subkey("Applications\\rustydxf.exe")?;
    app.set_value("FriendlyAppName", &brand::PRODUCT_NAME)?;
    app.create_subkey("DefaultIcon")?
        .0
        .set_value("", &icon_ref)?;
    app.create_subkey("shell\\open\\command")?
        .0
        .set_value("", &open_command)?;
    app.create_subkey("SupportedTypes")?
        .0
        .set_value(".dxf", &"")?;

    // Tell the shell that associations changed.
    // SAFETY: SHCNF_IDLIST with null items is the documented way to send
    // SHCNE_ASSOCCHANGED.
    unsafe {
        use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
        SHChangeNotify(
            SHCNE_ASSOCCHANGED as i32,
            SHCNF_IDLIST,
            std::ptr::null(),
            std::ptr::null(),
        );
    }

    println!("{} is registered for .dxf files.", brand::PRODUCT_NAME);
    println!(
        "Windows does not let an app make itself the default. Settings will open now:\n  \
         Default apps → search for \".dxf\" → choose {}.",
        brand::PRODUCT_NAME
    );
    let opened = std::process::Command::new("cmd")
        .args(["/C", "start", "", "ms-settings:defaultapps"])
        .status();
    if !opened.map(|s| s.success()).unwrap_or(false) {
        println!("Could not open Settings automatically; open Settings → Apps → Default apps.");
    }
    Ok(())
}

fn install_icon() -> io::Result<String> {
    let dirs = directories::ProjectDirs::from("dev", "art-tech", "rustydxf")
        .ok_or_else(|| io::Error::other("no user data directory"))?;
    let dir = dirs.data_local_dir();
    std::fs::create_dir_all(dir)?;
    let path = dir.join("rustydxf.ico");
    std::fs::write(&path, ICON)?;
    Ok(path.display().to_string())
}

/// A security descriptor that grants access to the current user only, for
/// the IPC named pipe.
pub fn current_user_only_descriptor() -> io::Result<SecurityDescriptor> {
    let sid = current_user_sid()?;
    // Protected DACL with a single entry: Generic All for this user.
    let sddl = format!("D:P(A;;GA;;;{sid})");
    let wide = widestring::U16CString::from_str(&sddl).map_err(io::Error::other)?;
    SecurityDescriptor::deserialize(&wide)
}

/// The current user's SID in string form ("S-1-5-21-…").
fn current_user_sid() -> io::Result<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: straightforward Win32 calls; every pointer passed is valid for
    // the duration of its call and every handle/allocation is released.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            let mut len = 0u32;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
            if len == 0 {
                return Err(io::Error::last_os_error());
            }
            // u64 storage keeps the buffer aligned for TOKEN_USER.
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            if GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
                return Err(io::Error::last_os_error());
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut text: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut n = 0;
            while *text.add(n) != 0 {
                n += 1;
            }
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
            LocalFree(text.cast());
            Ok(sid)
        })();
        CloseHandle(token);
        result
    }
}
