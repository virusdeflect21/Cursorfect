#[path = "src/icon.rs"]
#[allow(dead_code)]
mod icon;

use std::path::PathBuf;

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security><requestedPrivileges><requestedExecutionLevel level="asInvoker" uiAccess="false"/></requestedPrivileges></security>
  </trustInfo>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=ui/main.slint");
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/main.slint", config).expect("Slint UI failed to compile");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
        let ico_path = out_dir.join("cursorfect.ico");
        std::fs::write(&ico_path, icon::build_ico(&[16, 32, 48, 64])).expect("write ico");
        // Also keep a copy in the source tree for reference / packaging.
        let _ = std::fs::create_dir_all("assets/icons");
        let _ = std::fs::write(
            "assets/icons/cursorfect.ico",
            icon::build_ico(&[16, 32, 48, 64]),
        );

        let mut res = winresource::WindowsResource::new();
        res.set_icon(ico_path.to_str().expect("utf-8 path"));
        res.set_manifest(MANIFEST);
        res.set("ProductName", "CursorFects");
        res.set(
            "FileDescription",
            "CursorFects - cursor effects",
        );
        res.set("LegalCopyright", "MIT License");
        if let Err(e) = res.compile() {
            // Not fatal: the app still works without the embedded icon/manifest
            // (DPI awareness is also requested at runtime).
            println!("cargo:warning=could not embed Windows resources: {e}");
        }
    }
}

