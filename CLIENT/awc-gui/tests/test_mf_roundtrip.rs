//! Encoder->decoder round-trip: synthesize NV12 frames, encode with the inbox H264
//! encoder MFT, feed the packets straight into MfRtspDecoder. Isolates our MFT
//! plumbing from live-stream content questions.
//! Run with: MF_DEBUG=1 cargo test --test test_mf_roundtrip -- --nocapture

use awc_gui::stream::mf::mf_roundtrip_selftest;

/// Lossless check for our Annex-B<->AVCC conversions (used by the MF path).
#[test]
fn test_avcc_conversion_lossless() {
    use awc_gui::stream::mf::mf_avcc_roundtrip_check;
    mf_avcc_roundtrip_check();
}

#[test]
fn test_mf_roundtrip() {
    let (enc_packets, decoded_frames, first_dims) =
        mf_roundtrip_selftest().expect("round-trip self-test failed");
    println!("[roundtrip] encoder packets={enc_packets} decoded_frames={decoded_frames} dims={first_dims:?}");
    assert!(decoded_frames >= 5, "round-trip produced no frames");
    println!("[roundtrip] ROUND-TRIP OK");
}
