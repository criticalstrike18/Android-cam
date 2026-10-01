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

/// Serial of the USB-attached device, if any. Wi-Fi transports appear as
/// `ip:port` (contain a colon); a bare serial means USB. Used to build
/// `adb -s <serial>` selectors so forwards keep working when USB *and*
/// Wi-Fi transports are attached at the same time — a bare `adb forward`
/// fails then with "more than one device/emulator" and the client would
/// sit offline forever while the phone streams happily.
pub fn usb_device_serial() -> Option<String> {
    let mut cmd = Command::new("adb");
    cmd.arg("devices");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let out_str = String::from_utf8_lossy(&output.stdout);
    out_str.lines().skip(1).find_map(|line| {
        let mut parts = line.split_whitespace();
        let serial = parts.next()?;
        let status = parts.next()?;
        if status == "device" && !serial.contains(':') {
            Some(serial.to_string())
        } else {
            None
        }
    })
}

fn run_single_forward(serial: &Option<String>, port: u16) -> Result<(), String> {
    let mut cmd = Command::new("adb");
    if let Some(s) = serial {
        cmd.args(["-s", s]);
    }
    let spec = format!("tcp:{port}");
    cmd.args(["forward", &spec, &spec]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let output = cmd
        .output()
        .map_err(|e| format!("adb forward {port} error: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    Ok(())
}

pub fn run_adb_forward() -> Result<String, String> {
    // Pin to the USB transport when present; fall back to whatever single
    // device adb would pick (e.g. Wi-Fi only) otherwise.
    let serial = usb_device_serial();
    run_single_forward(&serial, 8080)?;
    run_single_forward(&serial, 8554)?;
    Ok("ADB port forwards (8080 & 8554) active".into())
}
