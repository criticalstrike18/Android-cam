use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtspCodec {
    H264,
    H265,
}

#[cfg(windows)]
fn set_low_latency_socket_buffer(stream: &TcpStream) {
    use std::os::windows::io::AsRawSocket;
    unsafe {
        #[link(name = "ws2_32")]
        extern "system" {
            fn setsockopt(
                s: usize,
                level: i32,
                optname: i32,
                optval: *const i8,
                optlen: i32,
            ) -> i32;
        }
        const SOL_SOCKET: i32 = 0xffff;
        const SO_RCVBUF: i32 = 0x1002;
        let size: i32 = 512 * 1024; // Limit to 512 KB to avoid 3-4s buffer bloat
        let s = stream.as_raw_socket() as usize;
        let _ = setsockopt(
            s,
            SOL_SOCKET,
            SO_RCVBUF,
            &size as *const _ as *const i8,
            std::mem::size_of::<i32>() as i32,
        );
    }
}

#[cfg(not(windows))]
fn set_low_latency_socket_buffer(_stream: &TcpStream) {}

/// A single NALU cannot legitimately approach this size. The cap stops a sender
/// that never sets the fragmentation end bit from growing the buffer unbounded.
pub const MAX_FU_BUFFER: usize = 4 * 1024 * 1024;

/// Upper bound on queued aggregation-packet overflow. A STAP-A/AP packet carries
/// a handful of NALUs, so exceeding this means the stream is desynchronised.
pub const MAX_PENDING_NALUS: usize = 64;

/// Depacketizer health counters, surfaced so stream problems become visible
/// instead of silently degrading into a black preview.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RtpStats {
    /// Packets whose sequence number skipped one or more predecessors.
    pub seq_gaps: u64,
    /// Partial NALUs discarded because a fragment was lost mid-fragmentation.
    pub dropped_partial_nalus: u64,
    /// Packets that carried RTP padding which was stripped from the payload.
    pub padded_packets: u64,
    /// Packets discarded as malformed (bad version, truncated, bad padding).
    pub malformed_packets: u64,
    /// NALUs dropped because the aggregation or fragmentation bound was exceeded.
    pub overflow_drops: u64,
}

/// The RTP header fields needed for depacketization.
pub struct RtpHeader {
    pub padding: bool,
    pub seq: u16,
    /// Total header size in bytes, including the CSRC list and any RTP extension.
    pub len: usize,
}

impl RtpHeader {
    /// Parses the fixed header, the CSRC list and any extension header.
    ///
    /// Returns `None` when the packet is truncated or is not RTP version 2.
    /// The previous fixed 12-byte assumption mis-read the NAL header whenever a
    /// sender used CSRCs or a header extension.
    pub fn parse(buf: &[u8]) -> Option<RtpHeader> {
        if buf.len() < 12 {
            return None;
        }
        let b0 = buf[0];
        if b0 >> 6 != 2 {
            return None;
        }
        let padding = (b0 & 0x20) != 0;
        let has_extension = (b0 & 0x10) != 0;
        let mut len = 12 + (b0 & 0x0F) as usize * 4;
        if has_extension {
            if buf.len() < len + 4 {
                return None;
            }
            // The length field counts 32-bit words *after* the 4-byte extension header.
            let ext_words = u16::from_be_bytes([buf[len + 2], buf[len + 3]]) as usize;
            len = len.checked_add(4 + ext_words * 4).filter(|l| *l <= buf.len())?;
        }
        if buf.len() < len {
            return None;
        }
        Some(RtpHeader {
            padding,
            seq: u16::from_be_bytes([buf[2], buf[3]]),
            len,
        })
    }
}

/// Returns the codec payload after the RTP header and any trailing padding.
///
/// Padding is stripped here, once, because the padding count lives in the *last*
/// byte of the packet. Leaving it in place appends the pad bytes to the tail of
/// every single-NAL packet and injects them into reassembled fragments, which is
/// a plausible source of the `slice without a first slice segment` failures the
/// HEVC path reports.
pub fn strip_rtp_framing(packet: &[u8], header_len: usize, padding: bool) -> Option<&[u8]> {
    let mut payload = packet.get(header_len..)?;
    if padding {
        let pad_len = *payload.last()? as usize;
        if pad_len == 0 || pad_len > payload.len() {
            return None; // malformed padding: reject rather than corrupt the NAL
        }
        payload = &payload[..payload.len() - pad_len];
    }
    (!payload.is_empty()).then_some(payload)
}

/// Prefixes a raw NALU with a 4-byte Annex-B start code.
fn annexb(nalu: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nalu.len() + 4);
    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
    out.extend_from_slice(nalu);
    out
}

/// Splits an aggregation payload into its Annex-B NALUs.
///
/// Stops at the first malformed size field so a corrupt tail can neither
/// over-read the packet nor spin.
fn split_aggregate(mut body: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while body.len() >= 2 {
        let nalu_size = u16::from_be_bytes([body[0], body[1]]) as usize;
        body = &body[2..];
        if nalu_size == 0 || nalu_size > body.len() {
            break;
        }
        out.push(annexb(&body[..nalu_size]));
        body = &body[nalu_size..];
    }
    out
}

/// Appends to `fu_buffer`, invalidating it if the cap would be exceeded so a
/// runaway sender cannot exhaust memory.
fn push_capped(fu_buffer: &mut Vec<u8>, data: &[u8], stats: &mut RtpStats) {
    if fu_buffer.len() + data.len() > MAX_FU_BUFFER {
        fu_buffer.clear();
        stats.overflow_drops += 1;
        return;
    }
    fu_buffer.extend_from_slice(data);
}

pub struct RtspSession {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    pub codec: RtspCodec,
    pub sps_pps: Vec<Vec<u8>>,
    fu_buffer: Vec<u8>,
    packet_buf: Vec<u8>,
    /// Overflow from aggregation packets (STAP-A / AP): one RTP packet can carry
    /// several NALUs, so extras wait here for subsequent calls.
    pending: VecDeque<Vec<u8>>,
    /// Last sequence number seen, used to detect packets skipped by the sender.
    last_seq: Option<u16>,
    /// Depacketizer health counters.
    pub stats: RtpStats,
}

impl RtspSession {
    pub fn connect(phone_ip: &str, port: u16) -> Result<Self, String> {
        // Bounded dial: the blocking TcpStream::connect inherits the OS timeout
        // (seconds to tens of seconds on dead IPs), which stalled every transport
        // switch behind an invisible wait. 1.5 s cannot false-positive a healthy
        // endpoint — LAN connects complete in milliseconds.
        const DIAL_TIMEOUT: Duration = Duration::from_millis(1500);
        let addr_str = format!("{}:{}", phone_ip, port);
        let addr = addr_str
            .to_socket_addrs()
            .map_err(|e| format!("TCP resolve {} failed: {}", addr_str, e))?
            .next()
            .ok_or_else(|| format!("TCP resolve {} failed: no address", addr_str))?;
        let stream = TcpStream::connect_timeout(&addr, DIAL_TIMEOUT)
            .map_err(|e| format!("TCP connect to {} failed: {}", addr_str, e))?;
        let _ = stream.set_nodelay(true);
        set_low_latency_socket_buffer(&stream);
        stream
            .set_read_timeout(Some(Duration::from_millis(2000)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_millis(2000)))
            .map_err(|e| e.to_string())?;

        let reader_stream = stream.try_clone().map_err(|e| e.to_string())?;
        let _ = reader_stream.set_nodelay(true);
        set_low_latency_socket_buffer(&reader_stream);

        let mut session = Self {
            stream,
            reader: BufReader::new(reader_stream),
            codec: RtspCodec::H264,
            sps_pps: Vec::new(),
            fu_buffer: Vec::new(),
            packet_buf: Vec::with_capacity(2048),
            pending: VecDeque::new(),
            last_seq: None,
            stats: RtpStats::default(),
        };

        session.handshake(phone_ip, port)?;
        Ok(session)
    }

    fn send_request(&mut self, req: &str) -> Result<String, String> {
        self.stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("Write failed: {}", e))?;

        let mut response = String::new();
        let mut content_length = 0usize;

        // Read response headers
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if trimmed.to_lowercase().starts_with("content-length:") {
                let parts: Vec<&str> = trimmed.split(':').collect();
                if parts.len() == 2 {
                    content_length = parts[1].trim().parse().unwrap_or(0);
                }
            }
            response.push_str(&line);
        }

        // Read body if content-length > 0
        if content_length > 0 {
            let mut body_buf = vec![0u8; content_length];
            self.reader
                .read_exact(&mut body_buf)
                .map_err(|e| format!("Reading body failed: {}", e))?;
            response.push_str(&String::from_utf8_lossy(&body_buf));
        }

        Ok(response)
    }

    fn handshake(&mut self, phone_ip: &str, port: u16) -> Result<(), String> {
        let base_url = format!("rtsp://{}:{}/live", phone_ip, port);

        // 1. OPTIONS
        let req_options = format!(
            "OPTIONS {} RTSP/1.0\r\nCSeq: 1\r\nUser-Agent: AWC-Native\r\n\r\n",
            base_url
        );
        let resp = self.send_request(&req_options)?;
        if !resp.contains("200 OK") {
            return Err(format!("OPTIONS failed: {}", resp));
        }

        // 2. DESCRIBE
        let req_describe = format!(
            "DESCRIBE {} RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\nUser-Agent: AWC-Native\r\n\r\n",
            base_url
        );
        let desc_resp = self.send_request(&req_describe)?;
        if !desc_resp.contains("200 OK") {
            return Err(format!("DESCRIBE failed: {}", desc_resp));
        }

        // Detect codec from SDP
        if desc_resp.contains("H265") || desc_resp.contains("h265") {
            self.codec = RtspCodec::H265;
            println!("[RTSP] Detected stream codec: H.265 / HEVC");
        } else {
            self.codec = RtspCodec::H264;
            println!("[RTSP] Detected stream codec: H.264 / AVC");
        }

        self.parse_sdp_sps_pps(&desc_resp);

        // Parse track control URL
        let mut track_url = format!("{}/streamid=0", base_url);
        for line in desc_resp.lines() {
            if line.starts_with("a=control:") {
                let track = line["a=control:".len()..].trim();
                if track.starts_with("rtsp://") {
                    track_url = track.to_string();
                } else if !track.is_empty() && track != "*" {
                    track_url = format!("{}/{}", base_url, track);
                }
                break;
            }
        }

        // 3. SETUP (Interleaved TCP on channel 0-1)
        let req_setup = format!(
            "SETUP {} RTSP/1.0\r\nCSeq: 3\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\nUser-Agent: AWC-Native\r\n\r\n",
            track_url
        );
        let setup_resp = self.send_request(&req_setup)?;
        if !setup_resp.contains("200 OK") {
            return Err(format!("SETUP failed: {}", setup_resp));
        }

        let mut session_header = String::new();
        for line in setup_resp.lines() {
            if line.to_lowercase().starts_with("session:") {
                let s_val = line["session:".len()..].trim();
                let clean_val = s_val.split(';').next().unwrap_or(s_val).trim();
                session_header = format!("Session: {}\r\n", clean_val);
                break;
            }
        }

        // 4. PLAY
        let req_play = format!(
            "PLAY {} RTSP/1.0\r\nCSeq: 4\r\n{}Range: npt=0.000-\r\nUser-Agent: AWC-Native\r\n\r\n",
            base_url, session_header
        );
        let play_resp = self.send_request(&req_play)?;
        if !play_resp.contains("200 OK") {
            return Err(format!("PLAY failed: {}", play_resp));
        }

        // Switch reader to 50ms read timeout for live frame polling with resilience against network jitter
        let _ = self.reader.get_ref().set_read_timeout(Some(Duration::from_millis(50)));

        println!("[RTSP] Handshake complete. Streaming interleaved RTP over TCP...");
        Ok(())
    }

    fn parse_sdp_sps_pps(&mut self, sdp: &str) {
        if self.codec == RtspCodec::H265 {
            for line in sdp.lines() {
                for tag in &["sprop-vps=", "sprop-sps=", "sprop-pps="] {
                    if let Some(idx) = line.find(tag) {
                        let val = &line[idx + tag.len()..];
                        let val = val.split(';').next().unwrap_or(val).trim();
                        for part in val.split(',') {
                            if let Some(bytes) = decode_base64(part.trim()) {
                                let mut nalu = vec![0x00, 0x00, 0x00, 0x01];
                                nalu.extend_from_slice(&bytes);
                                self.sps_pps.push(nalu);
                            }
                        }
                    }
                }
            }
        } else {
            for line in sdp.lines() {
                if line.contains("sprop-parameter-sets=") {
                    if let Some(idx) = line.find("sprop-parameter-sets=") {
                        let sets_str = &line[idx + "sprop-parameter-sets=".len()..];
                        let sets_str = sets_str.split(';').next().unwrap_or(sets_str).trim();
                        for part in sets_str.split(',') {
                            if let Some(bytes) = decode_base64(part.trim()) {
                                let mut nalu = vec![0x00, 0x00, 0x00, 0x01];
                                nalu.extend_from_slice(&bytes);
                                self.sps_pps.push(nalu);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Check if more bytes are already buffered in memory
    #[inline]
    pub fn has_backlog(&self) -> bool {
        self.reader.buffer().len() > 16 * 1024
    }

    /// Returns the number of unread bytes in the reader's buffer
    #[inline]
    pub fn reader_buffer_len(&self) -> usize {
        self.reader.buffer().len()
    }

    /// Reads next NAL unit from TCP interleaved stream ($ channel 0).
    ///
    /// Aggregation packets (STAP-A / AP) fan out: the first NALU is returned
    /// and the rest are queued in `pending` for subsequent calls, so no NALU
    /// is ever silently dropped.
    pub fn read_next_nalu(&mut self) -> Result<Option<Vec<u8>>, String> {
        if let Some(nalu) = self.pending.pop_front() {
            return Ok(Some(nalu));
        }
        loop {
            let mut magic = [0u8; 1];
            if let Err(e) = self.reader.read_exact(&mut magic) {
                if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock {
                    return Ok(None);
                }
                return Err(e.to_string());
            }

            if magic[0] != b'$' {
                // Skip inter-packet garbage: RTSP status lines, keepalive text, or
                // the tail of a desynchronised length field.
                continue;
            }

            let mut header = [0u8; 3];
            self.reader
                .read_exact(&mut header)
                .map_err(|e| format!("Interleaved header read failed: {}", e))?;
            let channel = header[0];
            let length = u16::from_be_bytes([header[1], header[2]]) as usize;

            if self.packet_buf.len() != length {
                self.packet_buf.resize(length, 0);
            }
            self.reader
                .read_exact(&mut self.packet_buf)
                .map_err(|e| format!("Interleaved payload read failed: {}", e))?;

            // We only process channel 0 (RTP video data); ignore channel 1 (RTCP)
            if channel != 0 {
                continue;
            }

            // Borrow the packet immutably while `depacketize_payload` takes the
            // mutable session state, so the two cannot overlap.
            let packet = std::mem::take(&mut self.packet_buf);
            let result = self.consume_packet(&packet);
            self.packet_buf = packet; // move back, preserving capacity

            if let Some(nalu) = result {
                return Ok(Some(nalu));
            }
        }
    }

    /// Handles one complete interleaved RTP packet, updating statistics and
    /// fragmentation state. Returns a NALU when one became available.
    fn consume_packet(&mut self, packet: &[u8]) -> Option<Vec<u8>> {
        let Some(rtp) = RtpHeader::parse(packet) else {
            self.stats.malformed_packets += 1;
            return None;
        };

        // A skipped packet inside a fragmentation unit means the partial NALU can
        // never be completed correctly. Drop it rather than handing the decoder a
        // corrupt NALU, which is what produced `slice without a first slice segment`.
        let contiguous = match self.last_seq {
            Some(prev) => prev.wrapping_add(1) == rtp.seq,
            None => true,
        };
        if !contiguous {
            self.stats.seq_gaps += 1;
            if !self.fu_buffer.is_empty() {
                self.fu_buffer.clear();
                self.stats.dropped_partial_nalus += 1;
            }
        }
        self.last_seq = Some(rtp.seq);

        if rtp.padding {
            self.stats.padded_packets += 1;
        }
        let Some(payload) = strip_rtp_framing(packet, rtp.len, rtp.padding) else {
            self.stats.malformed_packets += 1;
            return None;
        };

        depacketize_payload(
            self.codec,
            payload,
            &mut self.fu_buffer,
            &mut self.pending,
            &mut self.stats,
        )
    }
}

/// RFC 6184 (H.264) / RFC 7798 (H.265) depacketization of a single RTP payload.
///
/// Returns one complete Annex-B NALU, or `None` when the packet was a fragment
/// continuation needing further packets, or an unhandled packetization format.
/// Aggregation-packet overflow is appended to `pending`.
pub fn depacketize_payload(
    codec: RtspCodec,
    payload: &[u8],
    fu_buffer: &mut Vec<u8>,
    pending: &mut VecDeque<Vec<u8>>,
    stats: &mut RtpStats,
) -> Option<Vec<u8>> {
    match codec {
        RtspCodec::H264 => depacketize_h264(payload, fu_buffer, pending, stats),
        RtspCodec::H265 => depacketize_h265(payload, fu_buffer, pending, stats),
    }
}

/// Fans an aggregation packet's NALUs out across `pending`, returning the first.
fn drain_aggregate(
    inner: Vec<Vec<u8>>,
    pending: &mut VecDeque<Vec<u8>>,
    stats: &mut RtpStats,
) -> Option<Vec<u8>> {
    let mut first = None;
    for nalu in inner {
        if first.is_none() {
            first = Some(nalu);
        } else if pending.len() < MAX_PENDING_NALUS {
            pending.push_back(nalu);
        } else {
            stats.overflow_drops += 1;
        }
    }
    first
}

/// RFC 7798 HEVC depacketization.
fn depacketize_h265(
    payload: &[u8],
    fu_buffer: &mut Vec<u8>,
    pending: &mut VecDeque<Vec<u8>>,
    stats: &mut RtpStats,
) -> Option<Vec<u8>> {
    if payload.len() < 2 {
        return None;
    }
    let nal_type = (payload[0] >> 1) & 0x3F;

    // Types 0..47: single NAL unit packet
    if nal_type <= 47 {
        return Some(annexb(payload));
    }

    // Type 48: AP (aggregation packet) - fan out every inner NALU.
    if nal_type == 48 {
        return drain_aggregate(split_aggregate(&payload[2..]), pending, stats);
    }

    // Type 49: FU (fragmentation unit)
    if nal_type == 49 && payload.len() >= 3 {
        let fu_header = payload[2];
        let is_start = (fu_header & 0x80) != 0;
        let is_end = (fu_header & 0x40) != 0;
        let original_nal_type = fu_header & 0x3F;

        let byte0 = (payload[0] & 0x81) | (original_nal_type << 1);
        let byte1 = payload[1];

        if is_start {
            fu_buffer.clear();
            fu_buffer.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, byte0, byte1]);
            push_capped(fu_buffer, &payload[3..], stats);
        } else if !fu_buffer.is_empty() {
            push_capped(fu_buffer, &payload[3..], stats);
        } else {
            // Continuation without a start fragment: the head was lost.
            return None;
        }

        if is_end && !fu_buffer.is_empty() {
            // Covers both the multi-packet FU and the single-fragment case.
            return Some(std::mem::take(fu_buffer));
        }
    }
    None
}

/// RFC 6184 H.264 depacketization.
fn depacketize_h264(
    payload: &[u8],
    fu_buffer: &mut Vec<u8>,
    pending: &mut VecDeque<Vec<u8>>,
    stats: &mut RtpStats,
) -> Option<Vec<u8>> {
    if payload.is_empty() {
        return None;
    }
    let nal_type = payload[0] & 0x1F;

    // Single NAL unit packet (types 1-23)
    if (1..=23).contains(&nal_type) {
        return Some(annexb(payload));
    }

    // STAP-A packet (type 24) - fan out every inner NALU.
    if nal_type == 24 {
        return drain_aggregate(split_aggregate(&payload[1..]), pending, stats);
    }

    // FU-A packet (type 28)
    if nal_type == 28 && payload.len() >= 2 {
        let fu_indicator = payload[0];
        let fu_header = payload[1];
        let is_start = (fu_header & 0x80) != 0;
        let is_end = (fu_header & 0x40) != 0;
        let original_nal_type = fu_header & 0x1F;
        let reconstructed_nal_header = (fu_indicator & 0xE0) | original_nal_type;

        if is_start {
            fu_buffer.clear();
            fu_buffer.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, reconstructed_nal_header]);
            push_capped(fu_buffer, &payload[2..], stats);
        } else if !fu_buffer.is_empty() {
            push_capped(fu_buffer, &payload[2..], stats);
        } else {
            // Continuation without a start fragment: the head was lost.
            return None;
        }

        if is_end && !fu_buffer.is_empty() {
            // Covers both the multi-packet FU and the single-fragment case.
            return Some(std::mem::take(fu_buffer));
        }
    }
    None
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut map = [255u8; 256];
    for (i, &b) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate() {
        map[b as usize] = i as u8;
    }

    let bytes = input.trim().as_bytes();
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;

    for &b in bytes {
        if b == b'=' {
            break;
        }
        let v = map[b as usize];
        if v == 255 {
            continue;
        }
        buf = (buf << 6) | (v as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }

    Some(out)
}
