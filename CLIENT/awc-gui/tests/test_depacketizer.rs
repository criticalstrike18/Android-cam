//! Depacketizer unit tests.
//!
//! These are pure logic tests: no phone, no ADB, no Media Foundation. They exist
//! because the aggregation-packet and fragmentation fixes shipped without any
//! regression coverage, and because the RTP framing bugs were only ever
//! discoverable against live hardware.

use awc_gui::stream::rtsp_client::{
    depacketize_payload, strip_rtp_framing, RtspCodec, RtpHeader, RtpStats, MAX_FU_BUFFER,
    MAX_PENDING_NALUS,
};
use std::collections::VecDeque;

/// Per-test depacketizer state.
struct Depacketizer {
    codec: RtspCodec,
    fu_buffer: Vec<u8>,
    pending: VecDeque<Vec<u8>>,
    stats: RtpStats,
    last_seq: Option<u16>,
}

impl Depacketizer {
    fn new(codec: RtspCodec) -> Self {
        Self {
            codec,
            fu_buffer: Vec::new(),
            pending: VecDeque::new(),
            stats: RtpStats::default(),
            last_seq: None,
        }
    }

    /// Feeds one RTP packet (full packet including the 12-byte header) and returns
    /// a completed NALU if one became available. Mirrors the session's own handling
    /// of malformed packets, so invalid input is counted rather than panicking.
    fn feed(&mut self, seq: u16, packet: &[u8]) -> Option<Vec<u8>> {
        let Some(header) = RtpHeader::parse(packet) else {
            self.stats.malformed_packets += 1;
            return None;
        };

        let contiguous = match self.last_seq {
            Some(prev) => prev.wrapping_add(1) == seq,
            None => true,
        };
        if !contiguous {
            self.stats.seq_gaps += 1;
            if !self.fu_buffer.is_empty() {
                self.fu_buffer.clear();
                self.stats.dropped_partial_nalus += 1;
            }
        }
        self.last_seq = Some(seq);

        if header.padding {
            self.stats.padded_packets += 1;
        }
        let Some(payload) = strip_rtp_framing(packet, header.len, header.padding) else {
            self.stats.malformed_packets += 1;
            return None;
        };

        if let Some(nalu) = self.pending.pop_front() {
            return Some(nalu);
        }
        depacketize_payload(
            self.codec,
            payload,
            &mut self.fu_buffer,
            &mut self.pending,
            &mut self.stats,
        )
    }

    fn drain_pending(&mut self) -> Vec<Vec<u8>> {
        self.pending.drain(..).collect()
    }
}

/// Builds an RTP packet with the given options and payload.
fn rtp_packet(payload: &[u8], pad: usize, csrc: &[u32], ext_words: Option<&[u8]>) -> Vec<u8> {
    let mut pkt = vec![0u8; 12];
    pkt[0] = 0x80; // V=2
    if pad > 0 {
        pkt[0] |= 0x20; // P
    }
    pkt[0] |= csrc.len() as u8 & 0x0F;
    if ext_words.is_some() {
        pkt[0] |= 0x10; // X
    }
    for &c in csrc {
        pkt.extend_from_slice(&c.to_be_bytes());
    }
    if let Some(ext) = ext_words {
        // 4-byte extension header: profile(2) + length in 32-bit words (2).
        let mut hdr = vec![0xBE, 0xDE];
        hdr.extend_from_slice(&((ext.len() / 4) as u16).to_be_bytes());
        pkt.extend_from_slice(&hdr);
        pkt.extend_from_slice(ext);
    }
    pkt.extend_from_slice(payload);
    if pad > 0 {
        pkt.extend(std::iter::repeat_n(0xAB, pad - 1));
        pkt.push(pad as u8);
    }
    pkt
}

fn stap_a(nalus: &[&[u8]]) -> Vec<u8> {
    let mut p = vec![24u8]; // type 24
    for n in nalus {
        p.extend_from_slice(&(n.len() as u16).to_be_bytes());
        p.extend_from_slice(n);
    }
    p
}

fn h265_ap(nalus: &[&[u8]]) -> Vec<u8> {
    let mut p = vec![(48 << 1) as u8, 1u8]; // type 48
    for n in nalus {
        p.extend_from_slice(&(n.len() as u16).to_be_bytes());
        p.extend_from_slice(n);
    }
    p
}

fn h265_fu(orig_type: u8, start: bool, end: bool, body: &[u8]) -> Vec<u8> {
    let mut p = vec![(49 << 1) as u8, 0x01];
    let mut fu = 0u8;
    if start {
        fu |= 0x80;
    }
    if end {
        fu |= 0x40;
    }
    p.push(fu | (orig_type & 0x3F));
    p.extend_from_slice(body);
    p
}

/// Builds an FU-A packet: F=1, NRI preserved in the indicator, type 28, plus the
/// FU header carrying the start/end flags and the original NAL type.
fn h264_fua(orig_type: u8, start: bool, end: bool, body: &[u8]) -> Vec<u8> {
    let indicator = 0x80 | (0x03 << 5) | 28; // F=1, NRI=3, type=FU-A
    let mut fu = orig_type & 0x1F;
    if start {
        fu |= 0x80;
    }
    if end {
        fu |= 0x40;
    }
    let mut p = vec![indicator, fu];
    p.extend_from_slice(body);
    p
}

/// The NAL header reconstructed from an FU-A with `orig_type` and NRI=3.
fn h264_reconstructed(orig_type: u8) -> u8 {
    0xE0 | (orig_type & 0x1F)
}

// ---------------------------------------------------------------- RTP framing

#[test]
fn header_rejects_non_rtp2_version() {
    let mut pkt = rtp_packet(&[0x65, 0x88], 0, &[], None);
    pkt[0] = 0x40; // V=1
    assert!(RtpHeader::parse(&pkt).is_none());
}

#[test]
fn header_rejects_truncated_packet() {
    assert!(RtpHeader::parse(&[0x80, 0x60]).is_none());
}

#[test]
fn header_accounts_for_csrc_list() {
    let csrc = [0xDEAD_BEEFu32, 0x1234_5678];
    let pkt = rtp_packet(&[0x65, 0xAA, 0xBB], 0, &csrc, None);
    let h = RtpHeader::parse(&pkt).expect("valid");
    assert_eq!(h.len, 12 + 8);
    // The NAL header must be found *after* the CSRC list, not at offset 12.
    assert_eq!(strip_rtp_framing(&pkt, h.len, h.padding).unwrap(), &[0x65, 0xAA, 0xBB]);
}

#[test]
fn header_accounts_for_rtp_extension() {
    let ext: Vec<u8> = (0..8).collect(); // 8 bytes = 2 words
    let pkt = rtp_packet(&[0x65, 0xCC, 0xDD], 0, &[], Some(&ext));
    let h = RtpHeader::parse(&pkt).expect("valid");
    assert_eq!(h.len, 12 + 4 + 8);
    assert_eq!(strip_rtp_framing(&pkt, h.len, h.padding).unwrap(), &[0x65, 0xCC, 0xDD]);
}

#[test]
fn header_accounts_for_csrc_and_extension_together() {
    let ext: Vec<u8> = (0..4).collect();
    let pkt = rtp_packet(&[0x65, 0xEE], 0, &[1, 2, 3], Some(&ext));
    let h = RtpHeader::parse(&pkt).expect("valid");
    assert_eq!(h.len, 12 + 12 + 4 + 4);
    assert_eq!(strip_rtp_framing(&pkt, h.len, h.padding).unwrap(), &[0x65, 0xEE]);
}

#[test]
fn padding_is_stripped_from_single_nal() {
    // This is the regression that corrupted the tail of every padded packet.
    let pkt = rtp_packet(&[0x65, 0x11, 0x22, 0x33], 4, &[], None);
    let h = RtpHeader::parse(&pkt).unwrap();
    assert!(h.padding);
    assert_eq!(
        strip_rtp_framing(&pkt, h.len, h.padding).unwrap(),
        &[0x65, 0x11, 0x22, 0x33]
    );
}

#[test]
fn padding_stripped_from_whole_packet_pad_length() {
    let body = [0x65, 0x99];
    let pkt = rtp_packet(&body, body.len(), &[], None);
    let h = RtpHeader::parse(&pkt).unwrap();
    assert_eq!(strip_rtp_framing(&pkt, h.len, h.padding).unwrap(), &body[..]);
}

#[test]
fn malformed_padding_is_rejected_not_applied() {
    // Pad length larger than the payload must not silently wrap the slice.
    let pkt = rtp_packet(&[0x65, 0x11, 0x22, 0x33], 3, &[], None);
    let h = RtpHeader::parse(&pkt).unwrap();
    let mut bad = pkt.clone();
    *bad.last_mut().unwrap() = 200;
    assert!(strip_rtp_framing(&bad, h.len, h.padding).is_none());
}

#[test]
fn zero_padding_is_rejected() {
    // A pad-length byte of 0 is malformed and must not slice off zero bytes
    // while claiming padding was handled.
    let mut pkt = rtp_packet(&[0x65, 0x11], 4, &[], None);
    *pkt.last_mut().unwrap() = 0;
    let mut d = Depacketizer::new(RtspCodec::H264);
    assert!(d.feed(0, &pkt).is_none());
    assert_eq!(d.stats.malformed_packets, 1);
}

// --------------------------------------------------- H.264 aggregation (STAP-A)

#[test]
fn stap_a_fans_out_every_inner_nalu() {
    // The original bug returned only the first inner NALU and dropped the rest.
    let nals: [&[u8]; 3] = [&[0x67, 0x01], &[0x68, 0x02], &[0x65, 0x03]];
    let pkt = rtp_packet(&stap_a(&nals), 0, &[], None);
    let mut d = Depacketizer::new(RtspCodec::H264);

    let first = d.feed(0, &pkt).expect("first NALU");
    assert_eq!(first, [0, 0, 0, 1, 0x67, 0x01]);

    let queued = d.drain_pending();
    assert_eq!(queued.len(), 2, "aggregate overflow must be queued, not dropped");
    assert_eq!(queued[0], [0, 0, 0, 1, 0x68, 0x02]);
    assert_eq!(queued[1], [0, 0, 0, 1, 0x65, 0x03]);
}

#[test]
fn stap_a_with_single_nalu_emits_it() {
    let pkt = rtp_packet(&stap_a(&[&[0x67, 0xAA]]), 0, &[], None);
    let mut d = Depacketizer::new(RtspCodec::H264);
    assert_eq!(d.feed(0, &pkt).unwrap(), [0, 0, 0, 1, 0x67, 0xAA]);
    assert!(d.drain_pending().is_empty());
}

#[test]
fn stap_a_malformed_tail_does_not_panic_or_spin() {
    // Declares an inner NAL longer than the remaining payload.
    let mut payload = vec![24u8];
    payload.extend_from_slice(&9999u16.to_be_bytes());
    payload.extend_from_slice(&[0x67, 0x01]);
    let pkt = rtp_packet(&payload, 0, &[], None);
    let mut d = Depacketizer::new(RtspCodec::H264);
    assert!(d.feed(0, &pkt).is_none());
    assert!(d.drain_pending().is_empty());
}

#[test]
fn stap_a_zero_length_inner_nalu_rejects_the_aggregate() {
    // A zero size field cannot be skipped unambiguously, so the whole aggregate is
    // dropped rather than guessing where the next NALU starts.
    let mut payload = vec![24u8, 0, 0];
    payload.extend_from_slice(&[0x67, 0x01, 0x68, 0x02]);
    let pkt = rtp_packet(&payload, 0, &[], None);
    let mut d = Depacketizer::new(RtspCodec::H264);
    assert!(d.feed(0, &pkt).is_none());
    assert!(d.drain_pending().is_empty());
}

#[test]
fn aggregation_overflow_is_bounded() {
    let many: Vec<Vec<u8>> = (0..MAX_PENDING_NALUS + 20).map(|i| vec![0x41, i as u8]).collect();
    let refs: Vec<&[u8]> = many.iter().map(|v| v.as_slice()).collect();
    let pkt = rtp_packet(&stap_a(&refs), 0, &[], None);
    let mut d = Depacketizer::new(RtspCodec::H264);

    assert!(d.feed(0, &pkt).is_some());
    assert_eq!(d.drain_pending().len(), MAX_PENDING_NALUS);
    assert!(d.stats.overflow_drops > 0);
}

// --------------------------------------------------------- H.264 fragmentation

#[test]
fn fu_a_multi_packet_reassembles() {
    let mut d = Depacketizer::new(RtspCodec::H264);
    let pkt0 = rtp_packet(&h264_fua(5, true, false, &[0xAA, 0xBB]), 0, &[], None);
    let pkt1 = rtp_packet(&h264_fua(5, false, false, &[0xCC]), 0, &[], None);
    let pkt2 = rtp_packet(&h264_fua(5, false, true, &[0xDD, 0xEE]), 0, &[], None);

    assert!(d.feed(0, &pkt0).is_none());
    assert!(d.feed(1, &pkt1).is_none());
    let out = d.feed(2, &pkt2).expect("completed NALU");
    assert_eq!(out, [0, 0, 0, 1, h264_reconstructed(5), 0xAA, 0xBB, 0xCC, 0xDD, 0xEE]);
    assert!(d.fu_buffer.is_empty(), "buffer must be released after completion");
}

#[test]
fn fu_a_single_fragment_completes_immediately() {
    let mut d = Depacketizer::new(RtspCodec::H264);
    let pkt = rtp_packet(&h264_fua(5, true, true, &[0x11, 0x22]), 0, &[], None);
    let out = d.feed(0, &pkt).expect("single-fragment FU-A");
    assert_eq!(out, [0, 0, 0, 1, h264_reconstructed(5), 0x11, 0x22]);
}

#[test]
fn fu_a_padding_does_not_corrupt_the_tail() {
    // Padding used to be appended into the reassembled NAL body.
    let mut d = Depacketizer::new(RtspCodec::H264);
    let pkt0 = rtp_packet(&h264_fua(5, true, false, &[0x11]), 0, &[], None);
    let pkt1 = rtp_packet(&h264_fua(5, false, true, &[0x22]), 6, &[], None);

    assert!(d.feed(0, &pkt0).is_none());
    assert_eq!(d.feed(1, &pkt1).unwrap(), [0, 0, 0, 1, h264_reconstructed(5), 0x11, 0x22]);
    assert_eq!(d.stats.padded_packets, 1);
}

#[test]
fn fu_a_continuation_without_start_is_dropped() {
    let mut d = Depacketizer::new(RtspCodec::H264);
    let pkt = rtp_packet(&h264_fua(5, false, true, &[0x99]), 0, &[], None);
    assert!(d.feed(0, &pkt).is_none());
}

#[test]
fn fu_a_start_restarts_a_stale_fragment() {
    let mut d = Depacketizer::new(RtspCodec::H264);
    let pkt0 = rtp_packet(&h264_fua(5, true, false, &[0xAA]), 0, &[], None);
    let pkt1 = rtp_packet(&h264_fua(5, true, true, &[0xBB]), 0, &[], None);
    assert!(d.feed(0, &pkt0).is_none());
    assert_eq!(d.feed(1, &pkt1).unwrap(), [0, 0, 0, 1, h264_reconstructed(5), 0xBB]);
}

#[test]
fn lost_fragment_discards_partial_nalu() {
    // A sequence gap mid-FU must invalidate the partial NALU instead of feeding a
    // corrupt NALU downstream, which is what broke the HEVC path.
    let mut d = Depacketizer::new(RtspCodec::H264);
    let pkt0 = rtp_packet(&h264_fua(5, true, false, &[0xAA]), 0, &[], None);
    let pkt9 = rtp_packet(&h264_fua(5, false, true, &[0xBB]), 0, &[], None);

    assert!(d.feed(0, &pkt0).is_none());
    assert!(d.feed(9, &pkt9).is_none(), "partial NALU must be discarded on loss");
    assert_eq!(d.stats.seq_gaps, 1);
    assert_eq!(d.stats.dropped_partial_nalus, 1);
    assert!(d.fu_buffer.is_empty());
}

#[test]
fn sequence_wraparound_is_contiguous() {
    let mut d = Depacketizer::new(RtspCodec::H264);
    let pkt = rtp_packet(&h264_fua(5, true, true, &[0x11]), 0, &[], None);
    assert!(d.feed(65535, &pkt).is_some());
    let pkt2 = rtp_packet(&[0x41, 0x02], 0, &[], None);
    assert_eq!(d.feed(0, &pkt2).unwrap(), [0, 0, 0, 1, 0x41, 0x02]);
    assert_eq!(d.stats.seq_gaps, 0, "65535 -> 0 must not count as a gap");
}

#[test]
fn oversized_fragment_is_capped_not_allocated() {
    let mut d = Depacketizer::new(RtspCodec::H264);
    // Simulate a sender that never sets the end bit by repeatedly sending
    // fragments; the buffer must be invalidated rather than grown unbounded.
    let chunk = vec![0x5Au8; 1 << 20];
    let pkt = rtp_packet(&h264_fua(5, false, false, &chunk), 0, &[], None);

    let mut seq = 0u16;
    let first = rtp_packet(&h264_fua(5, true, false, &chunk), 0, &[], None);
    assert!(d.feed(seq, &first).is_none());
    seq += 1;

    for _ in 0..(MAX_FU_BUFFER / chunk.len()) {
        d.feed(seq, &pkt);
        seq += 1;
        if d.stats.overflow_drops > 0 {
            break;
        }
    }
    assert!(d.stats.overflow_drops > 0, "cap must trigger and invalidate");
    assert!(d.fu_buffer.is_empty());
}

// ------------------------------------------------------------- H.265 AP and FU

#[test]
fn h265_ap_fans_out_every_inner_nalu() {
    let nals: [&[u8]; 3] = [&[0x40, 0x01], &[0x42, 0x02], &[0x26, 0x03]];
    let pkt = rtp_packet(&h265_ap(&nals), 0, &[], None);
    let mut d = Depacketizer::new(RtspCodec::H265);

    assert_eq!(d.feed(0, &pkt).unwrap(), [0, 0, 0, 1, 0x40, 0x01]);
    let queued = d.drain_pending();
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0], [0, 0, 0, 1, 0x42, 0x02]);
    assert_eq!(queued[1], [0, 0, 0, 1, 0x26, 0x03]);
}

#[test]
fn h265_single_nalu_passthrough() {
    let pkt = rtp_packet(&[0x26, 0x01, 0xAF], 0, &[], None);
    let mut d = Depacketizer::new(RtspCodec::H265);
    assert_eq!(d.feed(0, &pkt).unwrap(), [0, 0, 0, 1, 0x26, 0x01, 0xAF]);
}

#[test]
fn h265_fu_reassembles_and_restores_original_type() {
    let mut d = Depacketizer::new(RtspCodec::H265);
    let pkt0 = rtp_packet(&h265_fu(1, true, false, &[0xAA]), 0, &[], None);
    let pkt1 = rtp_packet(&h265_fu(1, false, true, &[0xBB]), 0, &[], None);

    assert!(d.feed(0, &pkt0).is_none());
    let out = d.feed(1, &pkt1).expect("completed NALU");
    // IDR_W_RADL is HEVC type 19; the FU indicator must not survive in the header.
    assert_eq!(out, [0, 0, 0, 1, (1 << 1), 0x01, 0xAA, 0xBB]);
}

#[test]
fn h265_lost_fragment_discards_partial_nalu() {
    // This is the direct cause of `slice without a first slice segment`: a
    // continuation slice arriving with no picture in flight.
    let mut d = Depacketizer::new(RtspCodec::H265);
    let pkt0 = rtp_packet(&h265_fu(1, true, false, &[0xAA]), 0, &[], None);
    let pkt7 = rtp_packet(&h265_fu(1, false, true, &[0xBB]), 0, &[], None);

    assert!(d.feed(0, &pkt0).is_none());
    assert!(d.feed(7, &pkt7).is_none());
    assert_eq!(d.stats.dropped_partial_nalus, 1);
}

#[test]
fn h265_fu_single_fragment_completes() {
    let mut d = Depacketizer::new(RtspCodec::H265);
    let pkt = rtp_packet(&h265_fu(19, true, true, &[0x01, 0x02]), 0, &[], None);
    let out = d.feed(0, &pkt).unwrap();
    assert_eq!(out, [0, 0, 0, 1, 19 << 1, 0x01, 0x01, 0x02]);
}

#[test]
fn malformed_packet_counts_and_continues() {
    let mut d = Depacketizer::new(RtspCodec::H264);
    let mut bad = rtp_packet(&[0x65], 0, &[], None);
    bad[0] = 0x40; // bad version
    assert!(d.feed(0, &bad).is_none());
    assert_eq!(d.stats.malformed_packets, 1);

    let good = rtp_packet(&[0x41, 0x01], 0, &[], None);
    assert_eq!(d.feed(1, &good).unwrap(), [0, 0, 0, 1, 0x41, 0x01]);
}