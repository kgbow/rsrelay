//! Native macOS CoreAudio HAL Virtual Microphone (`rsrelayVirtualMic.driver`) installer & helpers.
//!
//! Embeds [`drivers/macos/rsrelay_virtual_mic.c`](../../../../drivers/macos/rsrelay_virtual_mic.c)
//! and [`drivers/macos/Info.plist`](../../../../drivers/macos/Info.plist) directly into the binary
//! so `rsrelay` on macOS can build, ad-hoc sign, and install `/Library/Audio/Plug-Ins/HAL/rsrelayVirtualMic.driver`
//! in a single click without external downloads or kernel extensions (`kext`).

use std::path::Path;

/// Name of the pure-input microphone endpoint exposed to Zoom, Chrome, WeChat, Discord, and OBS on macOS.
pub const MACOS_RSRELAY_VMIC_INPUT_NAME: &str = "rsrelay Virtual Mic";

/// Name of the dedicated bridge render endpoint (`CanBeDefaultDevice = 0`) that `rsrelay` writes into on macOS.
pub const MACOS_RSRELAY_VMIC_SINK_NAME: &str = "rsrelay Virtual Mic Sink";

/// Embedded C11 CoreAudio `AudioServerPlugIn` source code.
pub const EMBEDDED_DRIVER_C_SOURCE: &str = include_str!("../../../drivers/macos/rsrelay_virtual_mic.c");

/// Embedded `Contents/Info.plist` for `rsrelayVirtualMic.driver`.
pub const EMBEDDED_DRIVER_INFO_PLIST: &str = include_str!("../../../drivers/macos/Info.plist");

/// Check whether `/Library/Audio/Plug-Ins/HAL/rsrelayVirtualMic.driver` is installed on macOS.
pub fn is_macos_rsrelay_driver_installed() -> bool {
    #[cfg(target_os = "macos")]
    {
        Path::new("/Library/Audio/Plug-Ins/HAL/rsrelayVirtualMic.driver/Contents/MacOS/rsrelayVirtualMic")
            .exists()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = Path::new("");
        false
    }
}

/// Build, sign, and install `/Library/Audio/Plug-Ins/HAL/rsrelayVirtualMic.driver` on macOS,
/// prompting the user via standard macOS administrator authorization (`osascript`) and restarting `coreaudiod`.
pub fn install_macos_virtual_mic_driver() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        use std::fs;
        use std::process::Command;
        use std::thread;
        use std::time::Duration;

        let stage_root = std::env::temp_dir().join("rsrelay_vmic_driver_build");
        let _ = fs::remove_dir_all(&stage_root);
        let bundle_dir = stage_root.join("rsrelayVirtualMic.driver");
        let contents_dir = bundle_dir.join("Contents");
        let macos_dir = contents_dir.join("MacOS");

        fs::create_dir_all(&macos_dir)
            .map_err(|e| format!("Failed to create staging directory: {e}"))?;

        let c_src_path = stage_root.join("rsrelay_virtual_mic.c");
        fs::write(&c_src_path, EMBEDDED_DRIVER_C_SOURCE)
            .map_err(|e| format!("Failed to write driver source: {e}"))?;
        fs::write(contents_dir.join("Info.plist"), EMBEDDED_DRIVER_INFO_PLIST)
            .map_err(|e| format!("Failed to write Info.plist: {e}"))?;

        let bin_path = macos_dir.join("rsrelayVirtualMic");

        // Try compiling as a Universal Binary (arm64 + x86_64) first, falling back to host architecture
        let universal_out = Command::new("clang")
            .args([
                "-std=c11",
                "-O2",
                "-bundle",
                "-arch",
                "arm64",
                "-arch",
                "x86_64",
                "-framework",
                "CoreAudio",
                "-framework",
                "CoreFoundation",
                "-o",
            ])
            .arg(&bin_path)
            .arg(&c_src_path)
            .output();

        let compiled_ok = match universal_out {
            Ok(out) if out.status.success() => true,
            _ => {
                let native_out = Command::new("clang")
                    .args([
                        "-std=c11",
                        "-O2",
                        "-bundle",
                        "-framework",
                        "CoreAudio",
                        "-framework",
                        "CoreFoundation",
                        "-o",
                    ])
                    .arg(&bin_path)
                    .arg(&c_src_path)
                    .output()
                    .map_err(|e| {
                        format!(
                            "Xcode Command Line Tools (`clang`) not found ({e}). Run `xcode-select --install` or `brew install --cask blackhole-2ch`."
                        )
                    })?;
                if !native_out.status.success() {
                    let stderr = String::from_utf8_lossy(&native_out.stderr);
                    return Err(format!("clang failed to compile HAL driver: {stderr}"));
                }
                true
            }
        };

        if !compiled_ok {
            return Err("Failed to compile rsrelayVirtualMic.driver".to_string());
        }

        // Ad-hoc sign the .driver bundle (required on macOS Ventura / Sonoma / Sequoia)
        let sign_out = Command::new("codesign")
            .args(["--force", "--deep", "--sign", "-"])
            .arg(&bundle_dir)
            .output()
            .map_err(|e| format!("Failed to run codesign: {e}"))?;

        if !sign_out.status.success() {
            let stderr = String::from_utf8_lossy(&sign_out.stderr);
            return Err(format!("codesign failed: {stderr}"));
        }

        // Install into /Library/Audio/Plug-Ins/HAL/ with administrator privileges and reload coreaudiod
        let bundle_str = bundle_dir.to_string_lossy().replace('\'', "'\\''");
        let shell_cmd = format!(
            "mkdir -p /Library/Audio/Plug-Ins/HAL && \
             rm -rf /Library/Audio/Plug-Ins/HAL/rsrelayVirtualMic.driver && \
             cp -R '{bundle_str}' /Library/Audio/Plug-Ins/HAL/rsrelayVirtualMic.driver && \
             chown -R root:wheel /Library/Audio/Plug-Ins/HAL/rsrelayVirtualMic.driver && \
             chmod -R 755 /Library/Audio/Plug-Ins/HAL/rsrelayVirtualMic.driver && \
             (killall -9 coreaudiod || killall coreaudiod || true)"
        );
        let osa_script = format!(
            "do shell script \"{}\" with administrator privileges",
            shell_cmd.replace('\\', "\\\\").replace('"', "\\\"")
        );

        let install_out = Command::new("osascript")
            .arg("-e")
            .arg(&osa_script)
            .output()
            .map_err(|e| format!("Failed to invoke macOS authorization dialog: {e}"))?;

        if !install_out.status.success() {
            let stderr = String::from_utf8_lossy(&install_out.stderr);
            return Err(format!("Driver installation cancelled or failed: {stderr}"));
        }

        // Allow coreaudiod ~750ms to respawn and register the new HAL devices
        thread::sleep(Duration::from_millis(750));
        Ok("/Library/Audio/Plug-Ins/HAL/rsrelayVirtualMic.driver".to_string())
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err("macOS HAL driver installer is only supported on macOS.".to_string())
    }
}

/// On Windows, launch a one-click `winget` installation of `VBAudio.VBCable` in a visible console
/// when neither `Virtual Mic` nor `VB-CABLE` is installed yet.
pub fn launch_windows_vbcable_install() -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        Command::new("cmd")
            .args([
                "/C",
                "start",
                "rsrelay - Install VB-CABLE Virtual Audio Driver",
                "powershell",
                "-NoExit",
                "-Command",
                "Write-Host 'Installing signed VB-CABLE Virtual Audio Driver via winget...' -ForegroundColor Cyan; winget install -e --id VBAudio.VBCable --accept-package-agreements --accept-source-agreements",
            ])
            .spawn()
            .map_err(|e| format!("Failed to launch winget installer: {e}"))?;
        Ok("VBAudio.VBCable".to_string())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("Windows VB-CABLE winget helper is only supported on Windows.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedded_macos_hal_driver_assets_valid() {
        assert!(EMBEDDED_DRIVER_C_SOURCE.contains("RsRelayVirtualMic_Create"));
        assert!(EMBEDDED_DRIVER_C_SOURCE.contains(MACOS_RSRELAY_VMIC_INPUT_NAME));
        assert!(EMBEDDED_DRIVER_C_SOURCE.contains(MACOS_RSRELAY_VMIC_SINK_NAME));
        assert!(EMBEDDED_DRIVER_INFO_PLIST.contains("443ABAB8-E7B3-491A-B985-BEB9187030DB"));
    }
}
