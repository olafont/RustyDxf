// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! Linux: desktop entry and MIME association for the current user.
//!
//! Files opened from a file manager arrive in argv (`Exec=… %f`).

use anyhow::{Context, Result};
use rustydxf_core::brand;
use std::path::{Path, PathBuf};
use std::process::Command;

const MIME_TYPE: &str = "image/vnd.dxf";
const ICON: &[u8] = include_bytes!("../../../assets/icon-256.png");

fn desktop_file_name() -> String {
    format!("{}.desktop", brand::APP_ID)
}

fn data_home() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let base = directories::BaseDirs::new().context("cannot find the home directory")?;
    Ok(base.home_dir().join(".local").join("share"))
}

/// Quote a path for the `Exec=` key of a desktop entry.
fn exec_quote(path: &Path) -> String {
    let mut out = String::from("\"");
    for c in path.display().to_string().chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         GenericName=DXF Viewer\n\
         Comment={description}\n\
         Exec={exec} %f\n\
         Icon={id}\n\
         Terminal=false\n\
         Categories=Graphics;Engineering;Viewer;\n\
         MimeType={MIME_TYPE};\n\
         StartupWMClass={id}\n",
        name = brand::PRODUCT_NAME,
        description = brand::DESCRIPTION,
        exec = exec_quote(exe),
        id = brand::APP_ID,
    )
}

fn mime_package() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <mime-info xmlns=\"http://www.freedesktop.org/standards/shared-mime-info\">\n  \
         <mime-type type=\"{MIME_TYPE}\">\n    \
         <comment>DXF drawing</comment>\n    \
         <glob pattern=\"*.dxf\"/>\n    \
         <glob pattern=\"*.DXF\"/>\n  \
         </mime-type>\n\
         </mime-info>\n"
    )
}

/// Run a helper tool, reporting (but tolerating) its absence or failure.
fn run(program: &str, args: &[&std::ffi::OsStr]) -> bool {
    match Command::new(program).args(args).status() {
        Ok(s) if s.success() => true,
        Ok(s) => {
            println!("  note: {program} exited with {s}");
            false
        }
        Err(e) => {
            println!("  note: could not run {program}: {e}");
            false
        }
    }
}

pub fn register() -> Result<()> {
    let exe = std::env::current_exe().context("locating the executable")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let data = data_home()?;

    let applications = data.join("applications");
    std::fs::create_dir_all(&applications)?;
    let desktop = applications.join(desktop_file_name());
    std::fs::write(&desktop, desktop_entry(&exe))
        .with_context(|| format!("writing {}", desktop.display()))?;
    println!("Installed {}", desktop.display());

    let icons = data.join("icons/hicolor/256x256/apps");
    std::fs::create_dir_all(&icons)?;
    std::fs::write(icons.join(format!("{}.png", brand::APP_ID)), ICON)?;

    // Make sure *.dxf maps to image/vnd.dxf even where the system database
    // does not know the type.
    let mime = data.join("mime");
    let packages = mime.join("packages");
    std::fs::create_dir_all(&packages)?;
    std::fs::write(
        packages.join(format!("{}.xml", brand::APP_ID)),
        mime_package(),
    )?;

    run("update-mime-database", &[mime.as_os_str()]);
    run("update-desktop-database", &[applications.as_os_str()]);
    let name = desktop_file_name();
    let is_default = run(
        "xdg-mime",
        &["default".as_ref(), name.as_ref(), MIME_TYPE.as_ref()],
    );

    if is_default {
        println!(
            "{} is now the default app for .dxf files ({MIME_TYPE}).",
            brand::PRODUCT_NAME
        );
    } else {
        println!(
            "The desktop entry is installed, but the default could not be set automatically.\n\
             Run: xdg-mime default {name} {MIME_TYPE}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_contents() {
        let entry = desktop_entry(Path::new("/opt/art tech/rustydxf"));
        assert!(entry.contains("Exec=\"/opt/art tech/rustydxf\" %f\n"));
        assert!(entry.contains("MimeType=image/vnd.dxf;\n"));
        assert!(entry.contains("Name=RustyDxf\n"));
        assert_eq!(desktop_file_name(), "dev.art-tech.rustydxf.desktop");
        assert_eq!(exec_quote(Path::new("/a\"b$c")), "\"/a\\\"b\\$c\"");
    }

    #[test]
    fn mime_package_declares_the_glob() {
        let xml = mime_package();
        assert!(xml.contains("<mime-type type=\"image/vnd.dxf\">"));
        assert!(xml.contains("<glob pattern=\"*.dxf\"/>"));
    }
}
