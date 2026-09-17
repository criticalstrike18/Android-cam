use serde::{Deserialize, Serialize};
use crate::core::config::{DEFAULT_PHONE_IP, DEFAULT_WIFI_IP};

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct PhoneSettings {
    pub camera: String,
    pub resolution_str: String,
    pub stream_protocol: String,
    pub rotation: String,
    #[serde(default)]
    pub supported_resolutions: Vec<String>,
    #[serde(default)]
    pub flash: bool,
    #[serde(default)]
    pub has_flash_unit: bool,
    #[serde(default)]
    pub zoom: f32,
    #[serde(default)]
    pub exposure_index: i32,
}

#[derive(Clone, Debug)]
pub struct PreviewFrame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct SharedAppState {
    pub phone_ip: String,
    pub wifi_ip: String,
    pub auto_fallback: bool,
    pub connection_mode: String, // "usb" or "wifi"
    pub connected: bool,
    pub virtual_cam_active: bool,
    pub camera: String,
    pub resolution: String,
    pub codec: String,
    pub rotation: String,
    pub supported_resolutions: Vec<String>,
    pub flash_enabled: bool,
    pub has_flash: bool,
    pub zoom: f32,
    pub exposure: i32,
    pub fps: f32,
    pub frames_sent: u64,
    pub source_w: u32,
    pub source_h: u32,
    pub pending_command: Option<String>,
}

impl Default for SharedAppState {
    fn default() -> Self {
        Self {
            phone_ip: DEFAULT_PHONE_IP.to_string(),
            wifi_ip: DEFAULT_WIFI_IP.to_string(),
            auto_fallback: true,
            connection_mode: "usb".to_string(),
            connected: false,
            virtual_cam_active: false,
            camera: "back".to_string(),
            resolution: "1280x720".to_string(),
            codec: "rtsp".to_string(),
            rotation: "auto".to_string(),
            supported_resolutions: vec![
                "3840x2160".to_string(),
                "2560x1440".to_string(),
                "1920x1080".to_string(),
                "1280x720".to_string(),
                "640x480".to_string(),
            ],
            flash_enabled: false,
            has_flash: true,
            zoom: 1.0,
            exposure: 0,
            fps: 0.0,
            frames_sent: 0,
            source_w: 1280,
            source_h: 720,
            pending_command: None,
        }
    }
}
