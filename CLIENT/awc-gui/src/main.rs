#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod core;
mod network;
mod platform;
mod stream;
mod ui;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui;

use crate::core::state::SharedAppState;
use crate::network::sync_worker;
use crate::stream::stream_worker;
use crate::ui::AwcApp;

fn main() -> eframe::Result<()> {
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
