//! macOS: open-file Apple Events and Launch Services registration.

use anyhow::{Context, Result, bail};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSArray, NSString, NSURL};
use rustydxf_core::brand;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::Command;

const DXF_UTI: &str = "com.autodesk.dxf";
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/\
                          LaunchServices.framework/Support/lsregister";

/// `-[NSApplicationDelegate application:openURLs:]`.
///
/// Finder (and `open file.dxf`) deliver documents with this Apple Event, both
/// on a cold launch and when the app is already running. They never appear
/// in argv.
unsafe extern "C-unwind" fn application_open_urls(
    _this: &AnyObject,
    _cmd: Sel,
    _application: &AnyObject,
    urls: &NSArray<NSURL>,
) {
    for url in urls.iter() {
        if let Some(path) = url.path() {
            super::push_opened_file(PathBuf::from(path.to_string()));
        }
    }
}

/// Teach winit's application delegate to receive open-file events.
///
/// Workaround: winit 0.30 owns the `NSApplication` delegate (class
/// `WinitApplicationDelegate`), exposes no open-file event, and panics if the
/// delegate is replaced. So instead of replacing it, add the missing method
/// to winit's delegate class at runtime. Must be called after the event loop
/// is built (that is when winit registers the class and sets the delegate).
pub fn install_open_handler() -> bool {
    let Some(class) = AnyClass::get(c"WinitApplicationDelegate") else {
        return false;
    };
    type OpenUrls = unsafe extern "C-unwind" fn(&AnyObject, Sel, &AnyObject, &NSArray<NSURL>);
    // SAFETY: `Imp` is an untyped function pointer; the real signature is
    // described by the type encoding below (void; self, _cmd, id, id), which
    // matches `application:openURLs:`.
    let added = unsafe {
        let imp: Imp = std::mem::transmute::<OpenUrls, Imp>(application_open_urls);
        objc2::ffi::class_addMethod(
            class as *const AnyClass as *mut AnyClass,
            sel!(application:openURLs:),
            imp,
            c"v@:@@".as_ptr(),
        )
        .as_bool()
    };
    if added {
        // AppKit may cache which optional methods a delegate implements when
        // the delegate is set, so set the same delegate again.
        // SAFETY: plain message sends on the main thread. winit keeps its own
        // strong reference to the delegate, so clearing NSApp's weak
        // reference for a moment cannot free it.
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            if !app.is_null() {
                let delegate: Option<Retained<AnyObject>> = msg_send![app, delegate];
                if let Some(delegate) = delegate {
                    let nil: *const AnyObject = std::ptr::null();
                    let _: () = msg_send![app, setDelegate: nil];
                    let _: () = msg_send![app, setDelegate: &*delegate];
                }
            }
        }
    }
    added
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    /// `OSStatus LSSetDefaultRoleHandlerForContentType(CFStringRef, LSRolesMask, CFStringRef)`
    fn LSSetDefaultRoleHandlerForContentType(
        content_type: *const c_void,
        role: u32,
        handler_bundle_id: *const c_void,
    ) -> i32;
    /// `CFArrayRef UTTypeCreateAllIdentifiersForTag(CFStringRef, CFStringRef, CFStringRef)`
    fn UTTypeCreateAllIdentifiersForTag(
        tag_class: *const c_void,
        tag: *const c_void,
        conforming_to: *const c_void,
    ) -> *mut c_void;
    /// `CFStringRef LSCopyDefaultRoleHandlerForContentType(CFStringRef, LSRolesMask)`
    fn LSCopyDefaultRoleHandlerForContentType(
        content_type: *const c_void,
        role: u32,
    ) -> *mut c_void;
}

/// Every type identifier the system maps the `.dxf` extension to, preferred
/// first. Other installed CAD apps often declare their own type for the
/// extension, and that type, not `com.autodesk.dxf`, is then the one Finder
/// uses, so the default handler has to be set for each of them.
fn dxf_type_identifiers() -> Vec<String> {
    let class = NSString::from_str("public.filename-extension");
    let tag = NSString::from_str("dxf");
    // SAFETY: valid CFString arguments; the result follows the Create rule
    // and CFArray is toll-free bridged with NSArray.
    let found: Option<Retained<NSArray<NSString>>> = unsafe {
        let array = UTTypeCreateAllIdentifiersForTag(
            Retained::as_ptr(&class).cast(),
            Retained::as_ptr(&tag).cast(),
            std::ptr::null(),
        );
        Retained::from_raw(array.cast())
    };
    let mut ids: Vec<String> = found
        .map(|a| a.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    // Dynamic types (dyn.*) cannot be given a handler.
    ids.retain(|id| !id.starts_with("dyn."));
    if !ids.iter().any(|id| id == DXF_UTI) {
        ids.push(DXF_UTI.to_string());
    }
    ids
}

fn default_handler(uti: &str) -> Option<String> {
    let uti = NSString::from_str(uti);
    // SAFETY: valid CFString argument; the result follows the Copy rule.
    let handler: Option<Retained<NSString>> = unsafe {
        Retained::from_raw(
            LSCopyDefaultRoleHandlerForContentType(Retained::as_ptr(&uti).cast(), K_LS_ROLES_ALL)
                .cast(),
        )
    };
    handler.map(|h| h.to_string())
}

const K_LS_ROLES_ALL: u32 = 0xFFFF_FFFF;

/// The `.app` bundle containing the running executable, if any.
pub fn bundle_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

pub fn register() -> Result<()> {
    let Some(bundle) = bundle_path() else {
        bail!(
            "--register must be run from inside the app bundle.\n\
             Build it with `cargo xtask bundle`, then run:\n  \
             dist/RustyDxf.app/Contents/MacOS/rustydxf --register"
        );
    };
    println!("Registering {} with Launch Services…", bundle.display());
    let status = Command::new(LSREGISTER)
        .arg("-f")
        .arg(&bundle)
        .status()
        .context("running lsregister")?;
    if !status.success() {
        bail!("lsregister failed with {status}");
    }

    // NSString is toll-free bridged with CFString.
    let bundle_id = NSString::from_str(brand::APP_ID);
    let types = dxf_type_identifiers();
    let mut all_set = true;
    for uti in &types {
        let uti_string = NSString::from_str(uti);
        // SAFETY: both arguments are valid CFString references for the call.
        let os_status = unsafe {
            LSSetDefaultRoleHandlerForContentType(
                Retained::as_ptr(&uti_string).cast(),
                K_LS_ROLES_ALL,
                Retained::as_ptr(&bundle_id).cast(),
            )
        };
        // Read it back: the call can report success without taking effect.
        let now = default_handler(uti);
        let ours = now
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(brand::APP_ID));
        if os_status == 0 && ours {
            println!("  {uti}: default handler set");
        } else {
            all_set = false;
            println!(
                "  {uti}: NOT set (OSStatus {os_status}, handler is {})",
                now.as_deref().unwrap_or("unset")
            );
        }
    }
    if all_set {
        println!(
            "{} is now the default app for .dxf files.",
            brand::PRODUCT_NAME
        );
    } else {
        println!(
            "Launch Services did not accept every change; macOS may require confirming it yourself."
        );
    }
    println!(
        "\nIf double-clicking a .dxf still opens another app, set it manually:\n  \
         select a .dxf in Finder → File → Get Info → Open with → {} → Change All…",
        brand::PRODUCT_NAME
    );
    Ok(())
}
