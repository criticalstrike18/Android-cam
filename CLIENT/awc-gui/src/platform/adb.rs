use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x08000000;

pub fn is_adb_device_connected() -> bool {
    let mut cmd = Command::new("adb");
    cmd.arg("devices");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    match cmd.output() {
        Ok(output) if output.status.success() => {
            let out_str = String::from_utf8_lossy(&output.stdout);
            out_str
                .lines()
                .skip(1)
                .any(|line| line.contains("\tdevice") || line.ends_with(" device"))
        }
        _ => false,
    }
}

pub fn run_adb_forward() -> Result<String, String> {
    let mut cmd1 = Command::new("adb");
    cmd1.args(["forward", "tcp:8080", "tcp:8080"]);
    #[cfg(windows)]
    cmd1.creation_flags(CREATE_NO_WINDOW);
    let output = cmd1.output().map_err(|e| format!("adb forward 8080 error: {}", e))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let mut cmd2 = Command::new("adb");
    cmd2.args(["forward", "tcp:8554", "tcp:8554"]);
    #[cfg(windows)]
    cmd2.creation_flags(CREATE_NO_WINDOW);
    let output2 = cmd2.output().map_err(|e| format!("adb forward 8554 error: {}", e))?;
    if !output2.status.success() {
        return Err(String::from_utf8_lossy(&output2.stderr).to_string());
    }
    Ok("ADB port forwards (8080 & 8554) active".into())
}
