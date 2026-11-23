use std::process::Stdio;
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error)]
pub enum AdbError {
    #[error("failed to execute adb: {0}")]
    Io(#[from] std::io::Error),
    #[error("adb command failed: {0}")]
    CommandFailed(String),
    #[error("no connected Android device found")]
    NoDeviceFound,
    #[error("process not found for package '{0}' (is the app running?)")]
    ProcessNotFound(String),
}

#[derive(Debug, Clone)]
pub struct AdbDevice {
    pub serial: String,
    pub state: String,
    pub model: Option<String>,
    pub device: Option<String>,
}

/// Guard that automatically removes the `adb forward` rule on drop.
pub struct JdwpForwardGuard {
    serial: String,
    local_port: u16,
    pid: u32,
}

impl JdwpForwardGuard {
    pub fn local_addr(&self) -> String {
        format!("127.0.0.1:{}", self.local_port)
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn serial(&self) -> &str {
        &self.serial
    }
}

impl Drop for JdwpForwardGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("adb")
            .args([
                "-s",
                &self.serial,
                "forward",
                "--remove",
                &format!("tcp:{}", self.local_port),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

async fn run_adb(args: &[&str]) -> Result<String, AdbError> {
    let output = Command::new("adb").args(args).output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        return Err(AdbError::CommandFailed(msg));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Lists all connected Android devices/emulators from `adb devices -l`.
pub async fn list_devices() -> Result<Vec<AdbDevice>, AdbError> {
    let out = run_adb(&["devices", "-l"]).await?;
    let mut devices = Vec::new();
    for line in out.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else { continue };
        let Some(state) = parts.next() else { continue };
        let mut model = None;
        let mut device = None;
        for token in parts {
            if let Some(v) = token.strip_prefix("model:") {
                model = Some(v.to_string());
            } else if let Some(v) = token.strip_prefix("device:") {
                device = Some(v.to_string());
            }
        }
        devices.push(AdbDevice {
            serial: serial.to_string(),
            state: state.to_string(),
            model,
            device,
        });
    }
    Ok(devices)
}

/// Picks the specified serial or defaults to the first online device.
pub async fn resolve_device(serial: Option<&str>) -> Result<AdbDevice, AdbError> {
    let devices = list_devices().await?;
    if let Some(s) = serial {
        devices
            .into_iter()
            .find(|d| d.serial == s)
            .ok_or(AdbError::NoDeviceFound)
    } else {
        devices
            .into_iter()
            .find(|d| d.state == "device")
            .ok_or(AdbError::NoDeviceFound)
    }
}

/// Finds the PID of a running Android package using `pidof`.
pub async fn find_package_pid(serial: &str, package: &str) -> Result<u32, AdbError> {
    let out = run_adb(&["-s", serial, "shell", "pidof", "-s", package]).await;
    if let Ok(s) = out {
        if let Ok(pid) = s.trim().parse::<u32>() {
            return Ok(pid);
        }
    }
    // Fallback without `-s` (older Android / toybox variants)
    let out = run_adb(&["-s", serial, "shell", "pidof", package]).await;
    if let Ok(s) = out {
        if let Some(first) = s.split_whitespace().next() {
            if let Ok(pid) = first.parse::<u32>() {
                return Ok(pid);
            }
        }
    }
    Err(AdbError::ProcessNotFound(package.to_string()))
}

/// Lists installed third-party packages matching an optional substring.
pub async fn list_packages(serial: &str, filter: Option<&str>) -> Result<Vec<String>, AdbError> {
    let out = run_adb(&["-s", serial, "shell", "pm", "list", "packages", "-3"]).await?;
    let mut pkgs: Vec<String> = out
        .lines()
        .filter_map(|l| l.trim().strip_prefix("package:").map(str::to_string))
        .filter(|p| filter.map(|f| p.contains(f)).unwrap_or(true))
        .collect();
    pkgs.sort();
    Ok(pkgs)
}

/// Launches an activity with `-D` (Wait for Debugger) so JDWP can attach before startup code runs.
pub async fn launch_wait_for_debugger(serial: &str, component: &str) -> Result<(), AdbError> {
    run_adb(&["-s", serial, "shell", "am", "start", "-D", "-n", component]).await?;
    Ok(())
}

/// Forwards `tcp:<local_port>` to `jdwp:<pid>` and returns a guard that cleans it up on drop.
pub async fn forward_jdwp(
    serial: &str,
    local_port: u16,
    pid: u32,
) -> Result<JdwpForwardGuard, AdbError> {
    let local = format!("tcp:{local_port}");
    let remote = format!("jdwp:{pid}");
    run_adb(&["-s", serial, "forward", &local, &remote]).await?;
    Ok(JdwpForwardGuard {
        serial: serial.to_string(),
        local_port,
        pid,
    })
}
