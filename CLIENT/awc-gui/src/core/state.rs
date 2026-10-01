use serde::{Deserialize, Serialize};
use crate::core::config::{DEFAULT_PHONE_IP, DEFAULT_WIFI_IP};

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct PhoneSettings {
    pub camera: String,
    pub resolution_str: String,
    pub stream_protocol: String,
    pub rotation: String,
    #[serde(default)]
    pub video_codec: String,
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

/// Capability ranges from the phone's `/features` endpoint. The sliders are
/// bounded by these — never by hardcoded guesses — so a thumb can always reach
/// both ends. Defaults equal the historical hardcoded bounds, so an unreachable
/// `/features` degrades to exactly today's behaviour.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PhoneFeatures {
    #[serde(default = "default_exposure_min")]
    pub exposure_lower: i32,
    #[serde(default = "default_exposure_max")]
    pub exposure_upper: i32,
    #[serde(default = "default_zoom_min")]
    pub zoom_min: f32,
    #[serde(default = "default_zoom_max")]
    pub zoom_max: f32,
}

fn default_exposure_min() -> i32 { -12 }
fn default_exposure_max() -> i32 { 12 }
fn default_zoom_min() -> f32 { 1.0 }
fn default_zoom_max() -> f32 { 5.0 }

impl Default for PhoneFeatures {
    fn default() -> Self {
        Self {
            exposure_lower: default_exposure_min(),
            exposure_upper: default_exposure_max(),
            zoom_min: default_zoom_min(),
            zoom_max: default_zoom_max(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PreviewFrame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Control-plane (HTTP/WS settings sync) state. Owned exclusively by sync_worker.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ControlStage {
    /// No reachable control endpoint.
    #[default]
    Idle,
    /// WebSocket or HTTP settings channel is up.
    Signaling,
}

/// Media-plane (RTSP/decode/publish) state. Owned exclusively by stream_worker.
/// Split from ControlStage so the two workers never fight over one field.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StreamStage {
    /// Nothing dialed.
    #[default]
    Idle,
    /// Dialing the RTSP endpoint (first attempt or cold start).
    Connecting,
    /// Session up, discarding pre-keyframe backlog at the live edge.
    WaitingKeyframe,
    /// Publishing frames to vcam + preview.
    Live,
    /// A live session broke and is being re-established (or raced).
    Reconnecting,
}

impl StreamStage {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamStage::Idle => "idle",
            StreamStage::Connecting => "connecting",
            StreamStage::WaitingKeyframe => "waiting for keyframe",
            StreamStage::Live => "live",
            StreamStage::Reconnecting => "reconnecting",
        }
    }
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
    pub video_codec: String, // "h264" or "h265"
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
    pub features: PhoneFeatures,
    pub control_stage: ControlStage,
    pub stream_stage: StreamStage,
    /// Bumped on every explicit transport/cell switch request. The stream worker
    /// tags its background racer with the generation it saw; a completion from a
    /// superseded generation is dropped, so click-spam resolves latest-wins with
    /// no transition lock and no ignored clicks.
    pub switch_generation: u64,
    /// Wall time between the last frame published by the old session and the
    /// first frame published by the new one at cutover. The canary for handoff
    /// regressions: make-before-break should hold this near one frame interval.
    pub last_cutover_gap_ms: u64,
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
            video_codec: "h264".to_string(),
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
            features: PhoneFeatures::default(),
            control_stage: ControlStage::Idle,
            stream_stage: StreamStage::Idle,
            switch_generation: 0,
            last_cutover_gap_ms: 0,
        }
    }
}
