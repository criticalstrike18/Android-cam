//! Media Foundation Video Processor checks:
//! 1. Loopback correctness (synthetic NV12 64x64 -> NV12 32x32).
//! 2. Rescale throughput (NV12 1920x1080 -> NV12 1280x720, N iterations).
//! Run with: cargo test --test test_mf_pipe -- --nocapture

use awc_gui::stream::mf::mf_selftest_processor;
use awc_gui::stream::mf::mf_processor_throughput;

#[test]
fn test_mf_processor_loopback() {
    let out = mf_selftest_processor().expect("processor self-test failed");
    assert_eq!(out.len(), 32 * 32 * 3 / 2, "unexpected output size");
    // Gradient input => non-trivial output; check luma varies across rows.
    let first = out[0];
    let mid = out[32 * 16];
    println!("[pipe] out bytes={} y[0]={} y[mid]={}", out.len(), first, mid);
    assert_ne!(first, mid, "output looks constant - processor may not have run");
    println!("[pipe] PROCESSOR LOOPBACK OK");
}

#[test]
fn test_mf_processor_throughput() {
    let (iters, avg_ms, gbps) = mf_processor_throughput(1920, 1080, 1280, 720, 200)
        .expect("processor throughput test failed");
    println!(
        "[pipe] rescale 1920x1080 NV12 -> 1280x720 NV12: {iters} iters, avg {avg_ms:.3} ms/frame ({gbps:.2} GB/s)"
    );
    assert!(avg_ms < 10.0, "unexpectedly slow HW rescale: {avg_ms} ms");
}
