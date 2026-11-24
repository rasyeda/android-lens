pub mod storage;
pub mod uiautomator;

use std::process::Stdio;
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error)]
pub enum InspectorError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ADB error: {0}")]
    Adb(#[from] adb_bridge::AdbError),
    #[error("{0}")]
    Message(String),
}

/// Checks whether `scrcpy` is installed and available on `PATH`.
pub async fn check_scrcpy() -> Option<String> {
    let out = Command::new("scrcpy")
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("scrcpy")
        .trim()
        .to_string();
    Some(first)
}

/// Launches `scrcpy` as a detached mirror window for the target ADB device serial.
pub async fn launch_scrcpy(serial: Option<&str>) -> Result<String, InspectorError> {
    let dev = adb_bridge::resolve_device(serial).await?;
    let title = match dev.model.as_deref() {
        Some(m) => format!("Android Lens — {m} ({})", dev.serial),
        None => format!("Android Lens — {}", dev.serial),
    };

    let mut cmd = Command::new("scrcpy");
    cmd.args(["-s", &dev.serial, "--window-title", &title])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    match cmd.spawn() {
        Ok(_child) => Ok(format!("Launched scrcpy for {}", dev.serial)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(InspectorError::Message(
            "scrcpy not found on PATH — install it with `brew install scrcpy` (macOS) or your package manager".into(),
        )),
        Err(e) => Err(InspectorError::Io(e)),
    }
}
