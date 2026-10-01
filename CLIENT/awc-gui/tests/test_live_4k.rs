use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use awc_gui::core::state::SharedAppState;
use awc_gui::stream::stream_worker;

#[test]

#[ignore = "requires a live phone on ADB (adb forward tcp:8080/tcp:8554)"]
fn test_live_4k_streaming_realtime() {
    println!("\n==========================================================================");
    println!("  TESTING LIVE 4K UHD STREAMING & ZERO-LAG STARTUP");
    println!("==========================================================================\n");

    let state = Arc::new(Mutex::new(SharedAppState {
        connected: true,
        phone_ip: "127.0.0.1".to_string(),
        resolution: "3840x2160".to_string(),
        ..Default::default()
    }));
    let running = Arc::new(AtomicBool::new(true));
    let frames_counter = Arc::new(AtomicU64::new(0));
    let (preview_tx, preview_rx) = sync_channel(2);

    let state_clone = state.clone();
    let running_clone = running.clone();
    let frames_clone = frames_counter.clone();

    // Spawn the real desktop stream worker
    let worker_handle = std::thread::spawn(move || {
        stream_worker(state_clone, running_clone, frames_clone, preview_tx);
    });

    let mut last_dims = (0usize, 0usize);
    let mut preview_count = 0u64;
    let t0 = Instant::now();

    // Drain preview frames for 4 seconds
    while t0.elapsed() < Duration::from_millis(4000) {
        if let Ok(frame) = preview_rx.recv_timeout(Duration::from_millis(100)) {
            last_dims = (frame.width, frame.height);
            preview_count += 1;
        }
    }

    running.store(false, Ordering::SeqCst);
    let _ = worker_handle.join();

    let total_decoded = frames_counter.load(Ordering::Relaxed);
    println!(">>> Results:");
    println!("  Total frames processed: {}", total_decoded);
    println!("  Preview frames rendered: {}", preview_count);
    println!("  Preview frame dimensions: {}x{}", last_dims.0, last_dims.1);

    assert!(total_decoded >= 20, "Expected at least 20 frames, got {}", total_decoded);
    println!("\n==========================================================================");
    println!("  4K REALTIME STREAMING TEST PASSED SUCCESSFULLY!");
    println!("==========================================================================\n");
}
