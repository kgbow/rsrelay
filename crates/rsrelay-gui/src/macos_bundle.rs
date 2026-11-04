//! macOS `.app` bundle generator and TCC permission helper for `rsrelay-gui`.
//!
//! Why this is needed on macOS:
//! macOS TCC (*System Settings > Privacy & Security > Screen & System Audio Recording*) only
//! registers and displays `.app` Application Bundles that have a valid `Contents/Info.plist`
//! (`CFBundleIdentifier`) and code signature. Running a bare `target/release/rsrelay-gui` binary
//! from Cargo/Terminal either attributes TCC requests to the parent Terminal/IDE or cannot be
//! selected in the System Settings `+` file picker.
//!
//! This module packages the current executable into `/Applications/rsrelay.app` (with a generated
//! 128x128 `.icns` icon, `Info.plist`, and ad-hoc `codesign`) in a single click or CLI command.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Check whether the current process is executing from inside a macOS `.app` bundle
/// (i.e. `<path>/rsrelay.app/Contents/MacOS/rsrelay-gui`).
pub fn is_running_in_app_bundle() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.contains(".app/Contents/MacOS/")))
        .unwrap_or(false)
}

/// Return the path to `/Applications/rsrelay.app` if it exists, or `~/Applications/rsrelay.app`.
pub fn installed_app_bundle_path() -> Option<PathBuf> {
    let sys_app = PathBuf::from("/Applications/rsrelay.app");
    if sys_app.exists() {
        return Some(sys_app);
    }
    if let Ok(home) = std::env::var("HOME") {
        let user_app = PathBuf::from(home).join("Applications/rsrelay.app");
        if user_app.exists() {
            return Some(user_app);
        }
    }
    None
}

/// Package the currently running `rsrelay-gui` executable into `/Applications/rsrelay.app`
/// (falling back to `~/Applications/rsrelay.app` if `/Applications` is not writable),
/// generate `Contents/Resources/AppIcon.icns` and `Contents/Info.plist`, and ad-hoc sign it.
pub fn install_macos_app_bundle() -> anyhow::Result<PathBuf> {
    let current_exe = std::env::current_exe()?;

    let target_app_dir = if is_dir_writable(Path::new("/Applications")) {
        PathBuf::from("/Applications/rsrelay.app")
    } else if let Ok(home) = std::env::var("HOME") {
        let user_apps = PathBuf::from(home).join("Applications");
        let _ = fs::create_dir_all(&user_apps);
        user_apps.join("rsrelay.app")
    } else {
        PathBuf::from("target/release/rsrelay.app")
    };

    let contents_dir = target_app_dir.join("Contents");
    let macos_dir = contents_dir.join("MacOS");
    let resources_dir = contents_dir.join("Resources");

    fs::create_dir_all(&macos_dir)?;
    fs::create_dir_all(&resources_dir)?;

    // 1. Copy executable binary into Contents/MacOS/rsrelay-gui
    let dest_bin = macos_dir.join("rsrelay-gui");
    if current_exe != dest_bin {
        fs::copy(&current_exe, &dest_bin)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&dest_bin)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dest_bin, perms)?;
    }

    // 2. Write Contents/Resources/AppIcon.icns (128x128 PNG wrapped in ICNS `ic07`)
    let icns_bytes = build_app_icon_icns_128();
    let icns_path = resources_dir.join("AppIcon.icns");
    fs::write(&icns_path, icns_bytes)?;

    // 3. Write Contents/Info.plist
    let info_plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>rsrelay</string>
    <key>CFBundleExecutable</key>
    <string>rsrelay-gui</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundleIdentifier</key>
    <string>dev.rsrelay.app</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>rsrelay</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>rsrelay requires Microphone access to stream ultra-low-latency voice audio over LAN.</string>
    <key>NSAudioCaptureUsageDescription</key>
    <string>rsrelay requires System Audio Recording access to capture desktop audio via ScreenCaptureKit.</string>
    <key>NSScreenCaptureUsageDescription</key>
    <string>rsrelay uses ScreenCaptureKit to capture system desktop audio without virtual audio drivers.</string>
</dict>
</plist>
"#;
    fs::write(contents_dir.join("Info.plist"), info_plist)?;

    // 4. Ad-hoc sign the .app bundle so macOS TCC persistently tracks its identity
    let _ = Command::new("codesign")
        .args(["--force", "--deep", "--sign", "-"])
        .arg(&target_app_dir)
        .output();

    Ok(target_app_dir)
}

/// Reveal the `.app` bundle in macOS Finder (`open -R <path>`).
pub fn reveal_in_finder(path: &Path) {
    let _ = Command::new("open").arg("-R").arg(path).spawn();
}

/// Open macOS *System Settings > Privacy & Security > Screen & System Audio Recording*.
pub fn open_macos_screen_capture_settings() {
    let _ = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .spawn();
}

fn is_dir_writable(dir: &Path) -> bool {
    let probe = dir.join(".rsrelay_write_probe");
    if fs::write(&probe, b"ok").is_ok() {
        let _ = fs::remove_file(&probe);
        true
    } else {
        false
    }
}

/// Generate a 128x128 Sonic Obsidian waveform icon wrapped in a valid Apple `.icns` (`ic07`) container.
pub fn build_app_icon_icns_128() -> Vec<u8> {
    let w = 128u32;
    let h = 128u32;
    let mut rgba = vec![0u8; (w * h * 4) as usize];

    let bar_heights = [24i32, 48, 80, 104, 72, 96, 56, 32];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let idx = ((y as u32 * w + x as u32) * 4) as usize;
            let dx = x - 64;
            let dy = y - 64;
            let dist_sq = dx * dx + dy * dy;

            if dist_sq <= 60 * 60 {
                // Deep Obsidian circular badge background (#121722)
                rgba[idx] = 18;
                rgba[idx + 1] = 23;
                rgba[idx + 2] = 34;
                rgba[idx + 3] = 255;

                // Outer Acoustic Mint ring highlight (#00D4AA)
                if dist_sq >= 56 * 56 {
                    rgba[idx] = 0;
                    rgba[idx + 1] = 212;
                    rgba[idx + 2] = 170;
                    rgba[idx + 3] = 220;
                }

                // 8 Acoustic Mint equalizer bars inside badge
                if (16..112).contains(&x) && (x - 16) % 12 < 8 {
                    let bar_idx = ((x - 16) / 12) as usize;
                    if bar_idx < bar_heights.len() {
                        let half_h = bar_heights[bar_idx] / 2;
                        if dy.abs() <= half_h {
                            rgba[idx] = 0;
                            rgba[idx + 1] = 212;
                            rgba[idx + 2] = 170;
                            rgba[idx + 3] = 255;
                        }
                    }
                }
            }
        }
    }

    let png_bytes = encode_uncompressed_png_rgba(&rgba, w, h);
    let block_len = (8 + png_bytes.len()) as u32;
    let total_len = 8 + block_len;

    let mut icns = Vec::with_capacity(total_len as usize);
    icns.extend_from_slice(b"icns");
    icns.extend_from_slice(&total_len.to_be_bytes());
    icns.extend_from_slice(b"ic07"); // 128x128 PNG icon type
    icns.extend_from_slice(&block_len.to_be_bytes());
    icns.extend_from_slice(&png_bytes);
    icns
}

/// Encode raw RGBA8 pixels into a standard-compliant uncompressed PNG image (zero external crates).
fn encode_uncompressed_png_rgba(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut raw_scanlines = Vec::with_capacity(((width * 4 + 1) * height) as usize);
    let row_bytes = (width * 4) as usize;
    for y in 0..height as usize {
        raw_scanlines.push(0u8); // Filter type 0 (None)
        let start = y * row_bytes;
        raw_scanlines.extend_from_slice(&rgba[start..start + row_bytes]);
    }

    // Wrap raw_scanlines in Zlib (RFC 1950) with uncompressed Deflate blocks (RFC 1951)
    let mut zlib = Vec::with_capacity(raw_scanlines.len() + 64);
    zlib.push(0x78); // CMF
    zlib.push(0x01); // FLG (no preset dict, fastest)

    let chunks: Vec<&[u8]> = raw_scanlines.chunks(65_535).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let is_last = i + 1 == chunks.len();
        zlib.push(if is_last { 0x01 } else { 0x00 });
        let len = chunk.len() as u16;
        let nlen = !len;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&nlen.to_le_bytes());
        zlib.extend_from_slice(chunk);
    }
    zlib.extend_from_slice(&adler32(&raw_scanlines).to_be_bytes());

    let mut png = Vec::with_capacity(zlib.len() + 128);
    png.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]); // PNG signature

    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8; // bit depth = 8
    ihdr[9] = 6; // color type = 6 (RGBA)
    write_png_chunk(&mut png, b"IHDR", &ihdr);
    write_png_chunk(&mut png, b"IDAT", &zlib);
    write_png_chunk(&mut png, b"IEND", &[]);
    png
}

fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    let mut crc = 0xFFFF_FFFFu32;
    for &b in chunk_type.iter().chain(data.iter()) {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    out.extend_from_slice(&(!crc).to_be_bytes());
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_icns_generation_header() {
        let icns = build_app_icon_icns_128();
        assert_eq!(&icns[0..4], b"icns");
        assert_eq!(&icns[8..12], b"ic07");
        assert!(icns.len() > 64 * 1024);
    }
}
