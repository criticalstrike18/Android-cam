use std::path::PathBuf;
use std::process::Command;
use virtualcam::camera::available_backends;
use virtualcam::pixel_format::PixelFormat;
use virtualcam::Camera;

const OBS_GUID: &str = "{A3FCE0F5-3493-419F-958A-ABA1250EC20B}";

pub struct VirtualCamera {
    camera: Option<Camera>,
    pub width: u32,
    pub height: u32,
}

fn is_driver_registered() -> bool {
    Command::new("reg")
        .args(["query", &format!("HKCR\\CLSID\\{}", OBS_GUID)])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn get_bundled_driver_dir() -> Option<PathBuf> {
    // 1. Current executable directory (production)
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let driver_dir = exe_dir.join("drivers").join("obs-virtualcam");
            if driver_dir.join("obs-virtualcam-module64.dll").exists() {
                return Some(driver_dir);
            }
        }
    }

    // 2. Cargo manifest directory (development)
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dev_driver_dir = manifest_dir.join("drivers").join("obs-virtualcam");
    if dev_driver_dir.join("obs-virtualcam-module64.dll").exists() {
        return Some(dev_driver_dir);
    }

    None
}

fn ensure_bundled_driver_installed() {
    if is_driver_registered() {
        return;
    }

    println!("[VirtualCam] OBS Virtual Camera driver not registered. Attempting auto-registration from bundled files...");

    let driver_dir = match get_bundled_driver_dir() {
        Some(d) => d,
        None => {
            eprintln!("[VirtualCam] Bundled driver directory not found. Skipping auto-install.");
            return;
        }
    };

    let dll_path = driver_dir.join("obs-virtualcam-module64.dll");
    let install_script = driver_dir.join("install.ps1");

    if !dll_path.exists() {
        eprintln!("[VirtualCam] Driver DLL not found at: {}", dll_path.display());
        return;
    }

    // Attempt direct regsvr32 silent registration first
    let reg_status = Command::new("regsvr32.exe")
        .args(["/s", &dll_path.to_string_lossy()])
        .status();

    if let Ok(status) = reg_status {
        if status.success() && is_driver_registered() {
            println!("[VirtualCam] Successfully registered bundled virtual camera driver via regsvr32!");
            return;
        }
    }

    // If direct registration failed (e.g. requires elevation), trigger elevated install.ps1
    if install_script.exists() {
        println!("[VirtualCam] Requesting elevation to register virtual camera driver...");
        #[cfg(target_os = "windows")]
        {
            let ps_cmd = format!(
                "Start-Process powershell -ArgumentList '-ExecutionPolicy Bypass -NoProfile -WindowStyle Hidden -File \\\"{}\\\"' -Verb RunAs -Wait",
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

        let mut cam = Self {
            camera: None,
            width,
            height,
        };
        cam.build_camera(fps);
        cam
    }

    /// Rebuilds the camera at new dimensions without re-running the driver
    /// installation check (no repeated `reg query`, no repeat UAC risk).
    /// Called when the stream's native resolution changes so the virtual camera
    /// publishes full-resolution frames instead of a fixed downscale.
    pub fn recreate(&mut self, width: u32, height: u32, fps: f64) {
        if self.width == width && self.height == height {
            return;
        }
        println!(
            "[VirtualCam] Output resolution change {}x{} -> {}x{}; rebuilding virtual camera.",
            self.width, self.height, width, height
        );
        // Drop the old camera BEFORE building the new one: the OBS driver holds
        // the device exclusively, so building while the old instance is alive
        // always fails with "already in use".
        self.camera = None;
        self.width = width;
        self.height = height;
        self.build_camera(fps);
        if self.camera.is_none() {
            eprintln!("[VirtualCam] Rebuild failed; publishing paused until the driver recovers.");
        }
    }

    fn build_camera(&mut self, fps: f64) {
        // Build with native NV12 format for zero-overhead direct shared-memory publishing
        self.camera = match Camera::builder(self.width, self.height, fps)
            .format(PixelFormat::NV12)
            .build()
        {
            Ok(c) => {
                println!(
                    "[VirtualCam] Virtual camera active via OBS Virtual Camera / Media Foundation ({}x{} @ {:.1} fps, native NV12)",
                    self.width, self.height, fps
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
    }

    /// Zero-overhead native NV12 frame publishing directly to OBS virtual camera shared memory.
    ///
    /// Returns `Err` when no virtual camera is present, rather than a silent
    /// `Ok(())`. Reporting success here made the FPS and frame counters claim a
    /// working virtual camera on machines where the driver never came up.
    pub fn send_nv12(&mut self, nv12_data: &[u8]) -> Result<(), String> {
        match self.camera.as_mut() {
            Some(cam) => cam.send(nv12_data).map_err(|e| e.to_string()),
            None => Err("no virtual camera available (driver not initialised)".to_string()),
        }
    }

    pub fn is_active(&self) -> bool {
        self.camera.is_some()
    }
}
