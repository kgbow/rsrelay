fn main() {
    println!("cargo:rerun-if-changed=../../packaging/assets/rsrelay.ico");
    println!("cargo:rerun-if-changed=../../packaging/assets/AppIcon.icns");

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_resources();
    }
}

#[cfg(windows)]
fn embed_windows_resources() {
    use std::env;
    use std::fs;
    use std::path::PathBuf;

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

    let src_ico = manifest_dir.join("../../packaging/assets/rsrelay.ico");
    let dst_ico = out_dir.join("rsrelay.ico");
    fs::copy(&src_ico, &dst_ico).expect("failed to copy rsrelay.ico into OUT_DIR");

    let major: u16 = env::var("CARGO_PKG_VERSION_MAJOR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let minor: u16 = env::var("CARGO_PKG_VERSION_MINOR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let patch: u16 = env::var("CARGO_PKG_VERSION_PATCH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let pkg_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".to_string());

    let rc_contents = format!(
        r#"1 ICON "rsrelay.ico"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x00040004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "rsrelay contributors\0"
            VALUE "FileDescription", "rsrelay Ultra-Low-Latency Audio Relay\0"
            VALUE "FileVersion", "{pkg_version}\0"
            VALUE "InternalName", "rsrelay-gui\0"
            VALUE "LegalCopyright", "MIT OR Apache-2.0\0"
            VALUE "OriginalFilename", "rsrelay-gui.exe\0"
            VALUE "ProductName", "rsrelay\0"
            VALUE "ProductVersion", "{pkg_version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
    );

    let rc_path = out_dir.join("rsrelay.rc");
    fs::write(&rc_path, rc_contents).expect("failed to write rsrelay.rc");

    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
