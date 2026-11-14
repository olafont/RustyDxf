//! Build helpers for RustyDxf (by art-tech).
//!
//! ```text
//! cargo xtask icons              # regenerate assets/icon.{png,icns,ico}
//! cargo xtask bundle [--debug]   # build dist/RustyDxf.app (macOS)
//! ```

mod icon;

use anyhow::{Context, Result, bail};
use rustydxf_core::brand;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() -> Result<()> {
    let root = workspace_root();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("icons") => icon::generate(&root.join("assets")),
        Some("bundle") => bundle(&root, args.iter().any(|a| a == "--debug")),
        _ => {
            eprintln!("usage: cargo xtask <icons | bundle [--debug]>");
            std::process::exit(2);
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives inside the workspace")
        .to_path_buf()
}

fn bundle(root: &Path, debug: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("`cargo xtask bundle` builds a macOS .app and must run on macOS");
    }
    let profile = if debug { "debug" } else { "release" };
    let mut cmd = Command::new(env!("CARGO"));
    cmd.current_dir(root).args(["build", "-p", "rustydxf"]);
    if !debug {
        cmd.arg("--release");
    }
    if !cmd.status().context("running cargo build")?.success() {
        bail!("cargo build failed");
    }

    let icns = root.join("assets/icon.icns");
    if !icns.exists() {
        icon::generate(&root.join("assets"))?;
    }

    let app = root
        .join("dist")
        .join(format!("{}.app", brand::PRODUCT_NAME));
    if app.exists() {
        fs::remove_dir_all(&app).context("removing old bundle")?;
    }
    let macos = app.join("Contents/MacOS");
    let resources = app.join("Contents/Resources");
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(&resources)?;

    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    fs::copy(
        target_dir.join(profile).join("rustydxf"),
        macos.join("rustydxf"),
    )
    .context("copying binary into bundle")?;
    fs::copy(&icns, resources.join("icon.icns"))?;
    fs::write(app.join("Contents/Info.plist"), info_plist())?;
    fs::write(app.join("Contents/PkgInfo"), "APPL????")?;

    // Ad-hoc signature: required for the bundle to launch on Apple silicon
    // once its contents changed, and enough for local use.
    let signed = Command::new("codesign")
        .args(["--force", "--deep", "--sign", "-"])
        .arg(&app)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !signed {
        eprintln!("warning: ad-hoc codesign failed; the app may not launch on Apple silicon");
    }

    println!("bundle created: {}", app.display());
    println!(
        "register as the default .dxf app with:\n  {} --register",
        macos.join("rustydxf").display()
    );
    Ok(())
}

fn info_plist() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleExecutable</key>
    <string>rustydxf</string>
    <key>CFBundleIdentifier</key>
    <string>{id}</string>
    <key>CFBundleName</key>
    <string>{name}</string>
    <key>CFBundleDisplayName</key>
    <string>{name}</string>
    <key>CFBundleIconFile</key>
    <string>icon</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>{version}</string>
    <key>CFBundleVersion</key>
    <string>{version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.graphics-design</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>{copyright}</string>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key>
            <string>DXF Drawing</string>
            <key>CFBundleTypeRole</key>
            <string>Viewer</string>
            <key>LSHandlerRank</key>
            <string>Owner</string>
            <key>CFBundleTypeIconFile</key>
            <string>icon</string>
            <key>CFBundleTypeExtensions</key>
            <array>
                <string>dxf</string>
            </array>
            <key>LSItemContentTypes</key>
            <array>
                <string>com.autodesk.dxf</string>
            </array>
        </dict>
    </array>
    <key>UTImportedTypeDeclarations</key>
    <array>
        <dict>
            <key>UTTypeIdentifier</key>
            <string>com.autodesk.dxf</string>
            <key>UTTypeDescription</key>
            <string>DXF Drawing</string>
            <key>UTTypeConformsTo</key>
            <array>
                <string>public.data</string>
            </array>
            <key>UTTypeTagSpecification</key>
            <dict>
                <key>public.filename-extension</key>
                <array>
                    <string>dxf</string>
                </array>
                <key>public.mime-type</key>
                <array>
                    <string>image/vnd.dxf</string>
                </array>
            </dict>
        </dict>
    </array>
</dict>
</plist>
"#,
        id = brand::APP_ID,
        name = brand::PRODUCT_NAME,
        copyright = brand::COPYRIGHT,
    )
}
