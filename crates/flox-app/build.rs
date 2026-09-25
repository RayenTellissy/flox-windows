//! Compiles the Slint UI, and embeds the icon and application manifest when
//! building for Windows on Windows. Resource compilation needs the Windows SDK
//! tools, so any other host skips it and keeps cross `cargo check` green.

use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/app.slint", config)?;

    let target_is_windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let host_is_windows = cfg!(windows);
    if target_is_windows && host_is_windows {
        windows_resources()?;
    } else {
        println!("cargo:warning=skipping Windows resources on non-Windows host");
    }
    Ok(())
}

#[cfg(windows)]
fn windows_resources() -> Result<(), Box<dyn Error>> {
    const MANIFEST: &str = "flox.exe.manifest";
    const ICON: &str = "../../assets/flox.ico";
    println!("cargo:rerun-if-changed={MANIFEST}");
    println!("cargo:rerun-if-changed={ICON}");
    let mut res = winresource::WindowsResource::new();
    res.set_manifest_file(MANIFEST);
    res.set("ProductName", "Flox");
    res.set("FileDescription", "Flox");
    if std::path::Path::new(ICON).exists() {
        res.set_icon(ICON);
    }
    res.compile()?;
    Ok(())
}

#[cfg(not(windows))]
fn windows_resources() -> Result<(), Box<dyn Error>> {
    Ok(())
}
