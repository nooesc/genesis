//! Cross-platform clipboard image extraction.
//!
//! Extracts image data from the system clipboard using OS-level CLI tools.
//! No external Rust dependencies are required — only standard process spawning.
//!
//! ## Platform support
//!
//! | Platform       | Primary tool    | Fallback           |
//! |----------------|-----------------|--------------------|
//! | macOS          | `pngpaste`      | `osascript`        |
//! | Linux (Wayland)| `wl-paste`      | —                  |
//! | Linux (X11)    | `xclip`         | —                  |

use std::path::Path;
use std::process::Command;

/// Errors that can occur during clipboard image operations.
#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("no image found in clipboard")]
    NoImage,
    #[error("clipboard tool not available: {0}")]
    ToolNotAvailable(String),
    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),
    #[error("clipboard extraction failed: {0}")]
    ExtractionFailed(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// The detected display platform for clipboard operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayPlatform {
    MacOs,
    LinuxWayland,
    LinuxX11,
}

/// Detect the current display platform.
///
/// On macOS, always returns `MacOs`.
/// On Linux, checks the `WAYLAND_DISPLAY` environment variable to distinguish
/// Wayland from X11.
pub fn detect_platform() -> Result<DisplayPlatform, ClipboardError> {
    if cfg!(target_os = "macos") {
        Ok(DisplayPlatform::MacOs)
    } else if cfg!(target_os = "linux") {
        if std::env::var("WAYLAND_DISPLAY").is_ok() {
            Ok(DisplayPlatform::LinuxWayland)
        } else {
            Ok(DisplayPlatform::LinuxX11)
        }
    } else {
        Err(ClipboardError::UnsupportedPlatform(
            std::env::consts::OS.to_owned(),
        ))
    }
}

/// Check whether a CLI tool exists on `$PATH`.
fn tool_exists(name: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {name} >/dev/null 2>&1")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

/// Save clipboard image using `pngpaste` (preferred on macOS).
fn macos_pngpaste(dest: &Path) -> Result<(), ClipboardError> {
    let status = Command::new("pngpaste")
        .arg(dest)
        .status()
        .map_err(|e| ClipboardError::ExtractionFailed(format!("pngpaste: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(ClipboardError::NoImage)
    }
}

/// Save clipboard image using `osascript` (AppleScript fallback on macOS).
///
/// Uses Objective-C bridge to read the pasteboard and write PNG data to a file.
fn macos_osascript(dest: &Path) -> Result<(), ClipboardError> {
    let dest_str = dest.display().to_string();
    // Escape backslashes and double quotes so the path is safe inside an
    // AppleScript string literal.
    let escaped = dest_str.replace('\\', "\\\\").replace('"', "\\\"");
    // AppleScript that checks the pasteboard for image data and writes it out.
    let script = format!(
        r#"use framework "AppKit"
set pb to current application's NSPasteboard's generalPasteboard()
set imgData to pb's dataForType:(current application's NSPasteboardTypePNG)
if imgData is missing value then
    error "no image"
end if
set filePath to "{escaped}"
set writeResult to (imgData's writeToFile:filePath atomically:true) as boolean
if not writeResult then
    error "write failed"
end if"#
    );
    let output = Command::new("osascript")
        .arg("-l")
        .arg("AppleScript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| ClipboardError::ExtractionFailed(format!("osascript: {e}")))?;
    if output.status.success() {
        // Belt-and-suspenders: verify the file was actually created.
        if !dest.exists() {
            return Err(ClipboardError::ExtractionFailed(
                "osascript: writeToFile returned success but output file is missing".to_owned(),
            ));
        }
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no image") {
            Err(ClipboardError::NoImage)
        } else {
            Err(ClipboardError::ExtractionFailed(format!(
                "osascript: {stderr}"
            )))
        }
    }
}

/// Check if the macOS clipboard contains image data (lightweight, no temp file).
fn macos_has_image() -> Result<bool, ClipboardError> {
    // First try pngpaste --help-style check via its `-v` flag,
    // but pngpaste doesn't have a dry-run mode. Instead, use osascript
    // to query the pasteboard types — this is fast and doesn't write anything.
    let script = r#"use framework "AppKit"
set pb to current application's NSPasteboard's generalPasteboard()
set imgData to pb's dataForType:(current application's NSPasteboardTypePNG)
if imgData is missing value then
    return "no"
else
    return "yes"
end if"#;
    let output = Command::new("osascript")
        .arg("-l")
        .arg("AppleScript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| ClipboardError::ExtractionFailed(format!("osascript: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.trim() == "yes")
}

// ---------------------------------------------------------------------------
// Linux (Wayland)
// ---------------------------------------------------------------------------

/// Save clipboard image using `wl-paste` (Wayland).
fn wayland_paste(dest: &Path) -> Result<(), ClipboardError> {
    let output = Command::new("wl-paste")
        .args(["--type", "image/png"])
        .output()
        .map_err(|e| ClipboardError::ExtractionFailed(format!("wl-paste: {e}")))?;
    if output.status.success() && !output.stdout.is_empty() {
        std::fs::write(dest, &output.stdout)?;
        Ok(())
    } else {
        Err(ClipboardError::NoImage)
    }
}

/// Check if the Wayland clipboard contains image data.
fn wayland_has_image() -> Result<bool, ClipboardError> {
    let output = Command::new("wl-paste")
        .args(["--list-types"])
        .output()
        .map_err(|e| ClipboardError::ExtractionFailed(format!("wl-paste: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().any(|t| t.starts_with("image/")))
}

// ---------------------------------------------------------------------------
// Linux (X11)
// ---------------------------------------------------------------------------

/// Save clipboard image using `xclip` (X11).
fn x11_paste(dest: &Path) -> Result<(), ClipboardError> {
    let output = Command::new("xclip")
        .args(["-selection", "clipboard", "-target", "image/png", "-o"])
        .output()
        .map_err(|e| ClipboardError::ExtractionFailed(format!("xclip: {e}")))?;
    if output.status.success() && !output.stdout.is_empty() {
        std::fs::write(dest, &output.stdout)?;
        Ok(())
    } else {
        Err(ClipboardError::NoImage)
    }
}

/// Check if the X11 clipboard contains image data.
fn x11_has_image() -> Result<bool, ClipboardError> {
    let output = Command::new("xclip")
        .args(["-selection", "clipboard", "-target", "TARGETS", "-o"])
        .output()
        .map_err(|e| ClipboardError::ExtractionFailed(format!("xclip: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().any(|t| t.starts_with("image/")))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract the clipboard image and save it as a PNG file at `dest`.
///
/// Tries platform-appropriate tools in priority order (e.g. `pngpaste` then
/// `osascript` on macOS). Returns `Ok(())` on success. Returns an error if no
/// image is in the clipboard or no suitable tool is found.
pub fn save_clipboard_image(dest: &Path) -> Result<(), ClipboardError> {
    let platform = detect_platform()?;
    match platform {
        DisplayPlatform::MacOs => {
            // Try pngpaste first, fall back to osascript
            if tool_exists("pngpaste") {
                match macos_pngpaste(dest) {
                    Ok(()) => return Ok(()),
                    Err(ClipboardError::NoImage) => {
                        // No image — fall through to osascript which may
                        // support additional pasteboard types.
                    }
                    Err(e) => {
                        tracing::warn!("pngpaste failed, falling back to osascript: {e}");
                    }
                }
            }
            macos_osascript(dest)
        }
        DisplayPlatform::LinuxWayland => {
            if !tool_exists("wl-paste") {
                return Err(ClipboardError::ToolNotAvailable(
                    "wl-paste (install wl-clipboard)".to_owned(),
                ));
            }
            wayland_paste(dest)
        }
        DisplayPlatform::LinuxX11 => {
            if !tool_exists("xclip") {
                return Err(ClipboardError::ToolNotAvailable(
                    "xclip (install xclip)".to_owned(),
                ));
            }
            x11_paste(dest)
        }
    }
}

/// Lightweight check for whether the clipboard currently contains image data.
///
/// Does not write any files. Returns `Ok(true)` if an image is present,
/// `Ok(false)` if not, or `Err` if the check itself fails.
pub fn has_clipboard_image() -> Result<bool, ClipboardError> {
    let platform = detect_platform()?;
    match platform {
        DisplayPlatform::MacOs => macos_has_image(),
        DisplayPlatform::LinuxWayland => {
            if !tool_exists("wl-paste") {
                return Ok(false);
            }
            wayland_has_image()
        }
        DisplayPlatform::LinuxX11 => {
            if !tool_exists("xclip") {
                return Ok(false);
            }
            x11_has_image()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_platform_returns_valid_result() {
        // Should not error on macOS or Linux (the platforms we test on).
        // On unsupported platforms, it returns an error which is also fine.
        let result = detect_platform();
        if cfg!(target_os = "macos") {
            assert_eq!(result.unwrap(), DisplayPlatform::MacOs);
        } else if cfg!(target_os = "linux") {
            let platform = result.unwrap();
            assert!(
                platform == DisplayPlatform::LinuxWayland || platform == DisplayPlatform::LinuxX11
            );
        } else {
            assert!(result.is_err());
        }
    }

    #[test]
    fn tool_exists_finds_common_tools() {
        // `sh` should always exist on macOS/Linux
        assert!(tool_exists("sh"));
        // A nonsense name should not exist
        assert!(!tool_exists("__nonexistent_tool_xyz__"));
    }

    #[test]
    fn clipboard_error_display() {
        let e = ClipboardError::NoImage;
        assert_eq!(e.to_string(), "no image found in clipboard");

        let e = ClipboardError::ToolNotAvailable("pngpaste".to_owned());
        assert_eq!(e.to_string(), "clipboard tool not available: pngpaste");

        let e = ClipboardError::UnsupportedPlatform("windows".to_owned());
        assert_eq!(e.to_string(), "unsupported platform: windows");
    }

    #[test]
    fn save_clipboard_image_to_nonexistent_dir_fails_gracefully() {
        let bad_path = std::path::Path::new("/tmp/__nonexistent_dir_xyz__/clip.png");
        let result = save_clipboard_image(bad_path);
        // Should fail (either no image, tool error, or I/O error) — not panic
        assert!(result.is_err());
    }

    #[test]
    fn has_clipboard_image_does_not_panic() {
        // This just verifies the function can be called without panicking,
        // regardless of whether there is actually an image in the clipboard.
        let _ = has_clipboard_image();
    }

    #[test]
    fn display_platform_debug_and_eq() {
        let a = DisplayPlatform::MacOs;
        let b = DisplayPlatform::MacOs;
        assert_eq!(a, b);
        assert_ne!(DisplayPlatform::MacOs, DisplayPlatform::LinuxX11);
        // Debug impl works
        let _ = format!("{:?}", DisplayPlatform::LinuxWayland);
    }
}
