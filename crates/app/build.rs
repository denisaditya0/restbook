//! Embeds the Windows executable's resources: its icon and its version info.
//!
//! This is the only branding the *build* owns; macOS and Linux take theirs from
//! packaging, so on those targets the script does nothing and `winres` is a
//! `cfg(windows)` dependency. The values are repeated as literals because a
//! build script cannot read `src/branding.rs`.
//!
//! The GNU `ld` keeps only one `.rsrc` section and silently drops the rest, so a
//! GNU build links the manifest without the icon or version block. The release
//! matrix builds MSVC, where the merge works; the script warns rather than leave
//! a generic-icon binary unexplained.

fn main() {
    // Compile-time inputs. Declared on every platform so a change re-runs the
    // script everywhere, not only on Windows.
    println!("cargo:rerun-if-changed=icons/restbook.ico");
    println!("cargo:rerun-if-env-changed=RESTBOOK_APP_ID");

    #[cfg(windows)]
    {
        if let Err(error) = embed_windows_resources() {
            // A missing resource compiler or a malformed icon should not read as
            // "the app does not compile". Say what broke and keep going: the
            // binary runs with a generic icon until this is fixed.
            println!("cargo:warning=windows resources not embedded: {error}");
        }
    }
}

#[cfg(windows)]
fn embed_windows_resources() -> Result<(), Box<dyn std::error::Error>> {
    let mut res = winres::WindowsResource::new();

    // The taskbar, Explorer's columns and the Alt-Tab switcher read these from
    // the resource section, so they are the one place a user sees the app's own
    // name instead of the exe's file name.
    res.set("FileDescription", "restbook");
    res.set("ProductName", "restbook");
    res.set("OriginalFilename", "restbook.exe");
    res.set("LegalCopyright", "MIT OR Apache-2.0");
    res.set("FileVersion", env!("CARGO_PKG_VERSION"));
    res.set("ProductVersion", env!("CARGO_PKG_VERSION"));

    // Must equal `branding::APP_ID` and the installer's `AppUserModelID`, or a
    // shortcut and a running window resolve to two different taskbar identities.
    // The env var is the same override `branding::app_id()` reads.
    let app_id =
        std::env::var("RESTBOOK_APP_ID").unwrap_or_else(|_| "com.restbook.restbook".to_owned());
    res.set("AppUserModelID", &app_id);

    res.set_icon("icons/restbook.ico");
    res.compile()?;

    // `winres` links through `windres` on the GNU toolchain, where the merge
    // above does not happen. Detect it here rather than trusting a setting: the
    // tell is `gcc` as the linker driver, which only the GNU target uses.
    if std::env::var_os("CARGO_CFG_TARGET_ENV").as_deref() == Some(std::ffi::OsStr::new("gnu")) {
        println!(
            "cargo:warning=the GNU toolchain's linker keeps only one .rsrc section, so the icon \
             and version info will not appear in this build. Build with the MSVC toolchain \
             (`--target x86_64-pc-windows-msvc`) to get them; the release artifacts already do."
        );
    }

    Ok(())
}
