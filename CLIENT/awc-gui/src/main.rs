#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod core;
mod network;
mod platform;
mod stream;
mod ui;

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::core::config::DEFAULT_PHONE_IP;
use crate::core::state::SharedAppState;
use crate::network::sync_worker;
use crate::stream::stream_worker;
use crate::ui::AwcApp;

const HEADLESS_USAGE: &str = "\
AWC Desktop Client
Usage:
  awc-gui.exe [--headless] [--phone-ip <ip>] [--wifi-ip <ip>] [--mode usb|wifi>]

Options:
  --headless        Run without the GUI window: stream straight to the virtual
                    camera with console status. Press Enter to stop.
  --phone-ip <ip>   Phone endpoint (default 127.0.0.1 over ADB forwards).
                    Use the phone's Wi-Fi address when unplugged.
  --wifi-ip <ip>    Fallback address for USB<->Wi-Fi auto-failover.
  --mode usb|wifi   Start on this transport (default usb).
  --duration-secs N Stop automatically after N seconds (for scripts/tests).
";

/// Release builds set windows_subsystem=\"windows\", so there is no console.
/// Attach to the launching terminal, or allocate one for double-click starts.
/// Failures are ignored: without a console the status lines simply go nowhere.
#[cfg(windows)]
fn ensure_console() {
    unsafe {
        use windows::Win32::System::Console::{
            ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole,
        };
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            let _ = AllocConsole();
        }
    }
}

#[cfg(not(windows))]
fn ensure_console() {}

fn print_usage() {
    ensure_console();
    println!("{HEADLESS_USAGE}");
}

fn run_headless(args: &[String]) -> eframe::Result<()> {
    ensure_console();

    // Minimal hand-rolled parsing: avoids a new CLI dependency for four flags.
    let mut phone_ip: Option<String> = None;
    let mut wifi_ip: Option<String> = None;
    let mut mode: Option<String> = None;
    let mut duration_secs: Option<u64> = None;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--phone-ip" => phone_ip = it.next().cloned(),
            "--wifi-ip" => wifi_ip = it.next().cloned(),
            "--mode" => mode = it.next().cloned(),
            "--duration-secs" => {
                duration_secs = it.next().and_then(|v| v.parse::<u64>().ok());
            }
            "--headless" | "--help" => {}
            other => eprintln!("[Headless] ignoring unknown argument: {other}"),
        }
    }

    let state = Arc::new(Mutex::new(SharedAppState::default()));
    {
        let mut s = state.lock().unwrap();
        if let Some(w) = wifi_ip {
            s.wifi_ip = w;
        }
        match mode.as_deref().map(str::to_lowercase).as_deref() {
            Some("wifi") => {
                s.phone_ip = s.wifi_ip.clone();
                s.connection_mode = "wifi".to_string();
            }
            Some("usb") => {
                s.connection_mode = "usb".to_string();
            }
            Some(other) => eprintln!("[Headless] ignoring unknown --mode: {other} (want usb|wifi)"),
            None => {}
        }
        if let Some(ip) = phone_ip {
            s.phone_ip = ip.clone();
            // A non-loopback endpoint with no explicit mode means Wi-Fi transport:
            // keep the sync worker from running USB/ADB upkeep against it.
            if mode.is_none() && ip != DEFAULT_PHONE_IP {
                s.connection_mode = "wifi".to_string();
            }
        }
        println!(
            "[Headless] endpoint {} (mode {})",
            s.phone_ip, s.connection_mode
        );
    }

    let running = Arc::new(AtomicBool::new(true));
    let frames_counter = Arc::new(AtomicU64::new(0));
    let (preview_tx, preview_rx) = sync_channel(1);

    let stream_handle = {
        let s = state.clone();
        let r = running.clone();
        let fc = frames_counter.clone();
        thread::spawn(move || stream_worker(s, r, fc, preview_tx))
    };
    let sync_handle = {
        let s = state.clone();
        let r = running.clone();
        thread::spawn(move || sync_worker(s, r))
    };

    // Enter (or stdin EOF) is the stop signal: no signal-handler dependency,
    // and it works on a plain console in both debug and release builds.
    // Only when stdin is interactive: piped/closed stdin reads EOF instantly,
    // which would stop the stream before the first frame. Non-interactive runs
    // rely on --duration-secs or process kill instead.
    if std::io::stdin().is_terminal() {
        let r = running.clone();
        thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            r.store(false, Ordering::SeqCst);
        });
    } else {
        eprintln!("[Headless] stdin is not a terminal; running until --duration-secs or kill.");
    }

    println!("=== AWC headless: publishing to virtual camera. Press Enter to stop. ===");
    let t0 = Instant::now();
    let mut last_status = Instant::now() - Duration::from_secs(5);
    let mut preview_frames = 0u64;
    loop {
        match preview_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(_) => preview_frames += 1,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if !running.load(Ordering::SeqCst) {
            break;
        }
        if let Some(d) = duration_secs {
            if t0.elapsed() >= Duration::from_secs(d) {
                println!("[Headless] duration limit ({}s) reached.", d);
                break;
            }
        }
        if last_status.elapsed() >= Duration::from_secs(5) {
            last_status = Instant::now();
            if let Ok(s) = state.lock() {
                println!(
                    "[Headless] t={:>4}s connected={} mode={} vcam={} fps={:>5.1} vcam_frames={} preview={} src={}x{}",
                    t0.elapsed().as_secs(),
                    s.connected,
                    s.connection_mode,
                    s.virtual_cam_active,
                    s.fps,
                    s.frames_sent,
                    preview_frames,
                    s.source_w,
                    s.source_h,
                );
            }
        }
    }

    running.store(false, Ordering::SeqCst);
    let _ = stream_handle.join();
    let _ = sync_handle.join();
    println!(
        "=== AWC headless exiting after {}s: {} vcam frames, {} preview frames drained ===",
        t0.elapsed().as_secs(),
        frames_counter.load(Ordering::SeqCst),
        preview_frames,
    );
    Ok(())
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help") {
        print_usage();
        return Ok(());
    }
    if args.iter().any(|a| a == "--headless") {
        return run_headless(&args);
    }
    println!("=== Starting AWC Desktop Client (Pure Native Rust) ===");
    let state = Arc::new(Mutex::new(SharedAppState::default()));
    let running = Arc::new(AtomicBool::new(true));
    let frames_counter = Arc::new(AtomicU64::new(0));

    // Bounded channel (capacity 1) for lock-free UI texture decoupling
    let (preview_tx, preview_rx) = sync_channel(1);

    {
        let s = state.clone();
        let r = running.clone();
        let fc = frames_counter.clone();
        thread::spawn(move || {
            stream_worker(s, r, fc, preview_tx);
        });
    }

    {
        let s = state.clone();
        let r = running.clone();
        thread::spawn(move || {
            sync_worker(s, r);
        });
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("AWC Desktop - Android Webcam Client"),
        ..Default::default()
    };

    let r_cleanup = running.clone();
    let res = eframe::run_native(
        "AWC Desktop Client",
        native_options,
        Box::new(|_cc| Ok(Box::new(AwcApp::new(state, preview_rx)))),
    );

    r_cleanup.store(false, Ordering::SeqCst);
    println!("=== AWC Desktop Client exiting: {:?} ===", res);
    res
}
