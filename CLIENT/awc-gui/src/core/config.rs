pub const CAM_WIDTH: u32 = 1280;
pub const CAM_HEIGHT: u32 = 720;
#[allow(dead_code)]
pub const DSHOW_FRAME_BYTES: usize = (CAM_WIDTH * CAM_HEIGHT * 3) as usize;
pub const HTTP_PORT: u16 = 8080;
pub const RTSP_PORT: u16 = 8554;
pub const DEFAULT_PHONE_IP: &str = "127.0.0.1";
pub const DEFAULT_WIFI_IP: &str = "192.168.1.100";
