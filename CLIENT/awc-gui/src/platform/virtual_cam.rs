use std::path::PathBuf;
use std::process::Command;
use virtualcam::camera::available_backends;
use virtualcam::pixel_format::PixelFormat;
use virtualcam::Camera;
use crate::stream::pipeline::rgb_to_nv12;

const OBS_GUID: &str = "{A3FCE0F5-3493-419F-958A-ABA1250EC20B}";

pub struct VirtualCamera {
    camera: Option<Camera>,
    mjpeg_nv12_buf: Vec<u8>,
    width: u32,
    height: u32,
}

fn is_driver_registered() -> bool {
    Command::new("reg")
        .args(["query", &format!("HKCR\\CLSID\\{}", OBS_GUID)])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn get_bundled_driver_dir() -> Option<PathBuf> {
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(parent) = exe_path.parent() {
            let candidate = parent.join("obs-virtualcam-module");
            if candidate.exists() {
                return Some(candidate);
            }
            if let Some(workspace_dir) = parent.parent().and_then(|p| p.parent()) {
                let candidate = workspace_dir.join("CLIENT/obs-virtualcam-module");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidate = manifest_dir.join("../obs-virtualcam-module");
    if candidate.exists() {
        return Some(candidate);
    }

    None
}

fn ensure_bundled_driver_installed() {
    if is_driver_registered() {
        println!("[VirtualCam] OBS Virtual Camera driver is already registered in Windows Registry.");
        return;
    }

    println!("[VirtualCam] Virtual camera driver not registered. Attempting background installation...");

    if let Some(driver_dir) = get_bundled_driver_dir() {
        let install_script = driver_dir.join("install.ps1");
        let dll_path = driver_dir.join("obs-virtualcam-module64.dll");

        if dll_path.exists() {
            let _ = Command::new("regsvr32.exe")
                .args(["/s", dll_path.to_str().unwrap_or("")])
                .status();
        }

        if !is_driver_registered() && install_script.exists() {
            let ps_cmd = format!(
                "Start-Process powershell -ArgumentList '-ExecutionPolicy Bypass -NoProfile -WindowStyle Hidden -File \"{}\"' -Verb RunAs -Wait",
                install_script.display()
            );
            let _ = Command::new("powershell")
                .args(["-WindowStyle", "Hidden", "-Command", &ps_cmd])
                .status();
        }
    }

    if is_driver_registered() {
        println!("[VirtualCam] Bundled virtual camera driver successfully installed and registered!");
    } else {
        eprintln!("[VirtualCam] Driver installation script completed.");
    }
}

impl VirtualCamera {
    pub fn new(width: u32, height: u32, fps: f64) -> Self {
        ensure_bundled_driver_installed();

        let backends = available_backends();
        println!("[VirtualCam] Available virtual camera backends: {:?}", backends);

        // Build with native NV12 format for zero-overhead direct shared-memory publishing
        let camera = match Camera::builder(width, height, fps)
            .format(PixelFormat::NV12)
            .build()
        {
            Ok(c) => {
                println!(
                    "[VirtualCam] Virtual camera active via OBS Virtual Camera / Media Foundation ({}x{} @ {:.1} fps, native NV12)",
                    width, height, fps
                );
                Some(c)
            }
            Err(e) => {
                eprintln!(
                    "[VirtualCam] Warning: Could not initialize virtual camera driver: {}. Desktop preview will continue.",
                    e
                );
                None
            }
        };

        let nv12_size = (width * height * 3 / 2) as usize;
        Self {
            camera,
            mjpeg_nv12_buf: vec![0u8; nv12_size],
            width,
            height,
        }
    }

    /// Zero-overhead native NV12 frame publishing directly to OBS virtual camera shared memory
    pub fn send_nv12(&mut self, nv12_data: &[u8]) -> Result<(), String> {
        if let Some(ref mut cam) = self.camera {
            cam.send(nv12_data).map_err(|e| e.to_string())
        } else {
            Ok(())
        }
    }

    /// Backward compatibility / MJPEG RGB publishing: converts RGB to NV12 into preallocated buffer
    pub fn send_frame(&mut self, rgb_data: &[u8]) -> Result<(), String> {
        if self.camera.is_some() {
            let nv12_size = (self.width * self.height * 3 / 2) as usize;
            if self.mjpeg_nv12_buf.len() != nv12_size {
                self.mjpeg_nv12_buf.resize(nv12_size, 0);
            }
            rgb_to_nv12(rgb_data, self.width, self.height, &mut self.mjpeg_nv12_buf);
            if let Some(ref mut cam) = self.camera {
                cam.send(&self.mjpeg_nv12_buf).map_err(|e| e.to_string())
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }

    pub fn is_active(&self) -> bool {
        self.camera.is_some()
    }
}
