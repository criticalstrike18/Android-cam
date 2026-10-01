//! Transport-switch race test (Phase 2 gate): flip the endpoint USB<->WiFi
//! mid-stream and assert frames keep flowing through every cutover.
//! Counts *preview* frames (not vcam publishes) so the test is robust when
//! another process holds the OBS virtual camera.
//!
//! Requires: USB forwards (127.0.0.1:8080/8554) AND Wi-Fi (192.168.29.140)
//! both live, phone foreground and streaming.
//! Run with: cargo test --test test_transport_switch -- --ignored --nocapture
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use awc_gui::core::state::SharedAppState;
use awc_gui::stream::stream_worker;

const USB_IP: &str = "127.0.0.1";
const WIFI_IP: &str = "192.168.29.140";

fn drain_preview(
    rx: &std::sync::mpsc::Receiver<awc_gui::core::state::PreviewFrame>,
    millis: u64,
) -> u64 {
    let t0 = Instant::now();
    let mut n = 0u64;
    while t0.elapsed() < Duration::from_millis(millis) {
        if rx
            .recv_timeout(Duration::from_millis(100))
            .is_ok()
        {
            n += 1;
        }
    }
    n
}

fn switch_to(state: &Arc<Mutex<SharedAppState>>, ip: &str) {
    let mut s = state.lock().unwrap();
    s.phone_ip = ip.to_string();
    s.switch_generation = s.switch_generation.wrapping_add(1);
    println!(">>> TEST: requested endpoint -> {} (gen {})", ip, s.switch_generation);
}

#[test]
#[ignore = "requires live phone on USB forwards AND WiFi with AWA foreground"]
fn test_transport_switch_make_before_break() {
    println!("\n==========================================================================");
    println!("  TRANSPORT SWITCH TEST: USB -> WiFi -> USB under a live stream");
    println!("==========================================================================\n");

    let state = Arc::new(Mutex::new(SharedAppState {
        connected: true,
        phone_ip: USB_IP.to_string(),
        ..Default::default()
    }));
    let running = Arc::new(AtomicBool::new(true));
    let frames_counter = Arc::new(AtomicU64::new(0));
    let (preview_tx, preview_rx) = sync_channel(2);

    let worker = {
        let (s, r, f) = (state.clone(), running.clone(), frames_counter.clone());
        std::thread::spawn(move || stream_worker(s, r, f, preview_tx))
    };

    // Phase 1: settle on USB.
    let usb_frames = drain_preview(&preview_rx, 6000);
    println!(">>> phase USB: {} preview frames", usb_frames);

    // Phase 2: race to WiFi while USB keeps publishing.
    switch_to(&state, WIFI_IP);
    let wifi_frames = drain_preview(&preview_rx, 9000);
    println!(">>> phase WiFi: {} preview frames", wifi_frames);

    // Phase 3: race back to USB.
    switch_to(&state, USB_IP);
    let usb2_frames = drain_preview(&preview_rx, 9000);
    println!(">>> phase USB-again: {} preview frames", usb2_frames);

    running.store(false, Ordering::SeqCst);
    let _ = worker.join();

    // Each phase must carry live video on its own: a teardown gap would show up
    // here as a starved phase. Thresholds are generous (9 s at ~15+ fps).
    assert!(usb_frames >= 20, "USB phase starved: {}", usb_frames);
    assert!(wifi_frames >= 20, "WiFi phase starved (race failed?): {}", wifi_frames);
    assert!(usb2_frames >= 20, "USB-return phase starved: {}", usb2_frames);
    println!("\n==========================================================================");
    println!("  TRANSPORT SWITCH TEST PASSED: frames flowed across both cutovers");
    println!("==========================================================================\n");
}
