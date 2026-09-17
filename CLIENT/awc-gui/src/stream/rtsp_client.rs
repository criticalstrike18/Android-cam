use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

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

pub struct RtspSession {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    pub sps_pps: Vec<Vec<u8>>,
    fu_buffer: Vec<u8>,
    packet_buf: Vec<u8>,
}

impl RtspSession {
    pub fn connect(phone_ip: &str, port: u16) -> Result<Self, String> {
        let addr = format!("{}:{}", phone_ip, port);
        let stream = TcpStream::connect(&addr).map_err(|e| format!("TCP connect to {} failed: {}", addr, e))?;
        let _ = stream.set_nodelay(true);
        stream
            .set_read_timeout(Some(Duration::from_millis(2000)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_millis(2000)))
            .map_err(|e| e.to_string())?;

        let reader_stream = stream.try_clone().map_err(|e| e.to_string())?;
        let _ = reader_stream.set_nodelay(true);

        let mut session = Self {
            stream,
            reader: BufReader::new(reader_stream),
            sps_pps: Vec::new(),
            fu_buffer: Vec::new(),
            packet_buf: Vec::with_capacity(2048),
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
            if line.to_lowercase().starts_with("content-length:") {
                if let Some(val) = line.split(':').nth(1) {
                    content_length = val.trim().parse().unwrap_or(0);
                }
            }
            response.push_str(&line);
            if line == "\r\n" || line == "\n" {
                break;
            }
        }

        // Read response body if present
        if content_length > 0 {
            let mut body = vec![0u8; content_length];
            self.reader
                .read_exact(&mut body)
                .map_err(|e| format!("Body read failed: {}", e))?;
            response.push_str(&String::from_utf8_lossy(&body));
        }

        Ok(response)
    }

    fn handshake(&mut self, phone_ip: &str, port: u16) -> Result<(), String> {
        let base_url = format!("rtsp://{}:{}/", phone_ip, port);

        // 1. OPTIONS
        let req_options = format!("OPTIONS {} RTSP/1.0\r\nCSeq: 1\r\nUser-Agent: AWC-Native\r\n\r\n", base_url);
        let opt_resp = self.send_request(&req_options)?;
        if !opt_resp.contains("200 OK") {
            return Err(format!("OPTIONS failed: {}", opt_resp));
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

        self.parse_sdp_sps_pps(&desc_resp);

        // Extract control URL from SDP (e.g. control:streamid=0 or track0)
        let mut control_url = base_url.clone();
        for line in desc_resp.lines() {
            if line.starts_with("a=control:") {
                let track = line["a=control:".len()..].trim();
                if track.starts_with("rtsp://") {
                    control_url = track.to_string();
                } else if !track.is_empty() && track != "*" {
                    if base_url.ends_with('/') {
                        control_url = format!("{}{}", base_url, track);
                    } else {
                        control_url = format!("{}/{}", base_url, track);
                    }
                }
            }
        }

        // 3. SETUP (TCP Interleaved Mode)
        let req_setup = format!(
            "SETUP {} RTSP/1.0\r\nCSeq: 3\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\nUser-Agent: AWC-Native\r\n\r\n",
            control_url
        );
        let setup_resp = self.send_request(&req_setup)?;
        if !setup_resp.contains("200 OK") {
            return Err(format!("SETUP failed: {}", setup_resp));
        }

        let mut session_header = String::new();
        for line in setup_resp.lines() {
            if line.to_lowercase().starts_with("session:") {
                let sid = line["session:".len()..].split(';').next().unwrap_or("").trim();
                session_header = format!("Session: {}\r\n", sid);
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

        // Switch reader to ultra-short read timeout (5ms) for non-blocking live frame polling
        let _ = self.reader.get_ref().set_read_timeout(Some(Duration::from_millis(5)));

        println!("[RTSP] Handshake complete. Streaming interleaved RTP over TCP...");
        Ok(())
    }

    fn parse_sdp_sps_pps(&mut self, sdp: &str) {
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

    /// Check if more bytes are already buffered in memory
    #[inline]
    #[allow(dead_code)]
    pub fn has_buffered_data(&self) -> bool {
        !self.reader.buffer().is_empty()
    }

    /// Reads next NAL unit from TCP interleaved stream ($ channel 0).
    pub fn read_next_nalu(&mut self) -> Result<Option<Vec<u8>>, String> {
        loop {
            let mut magic = [0u8; 1];
            if let Err(e) = self.reader.read_exact(&mut magic) {
                if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock {
                    return Ok(None);
                }
                return Err(e.to_string());
            }

            if magic[0] != b'$' {
                // Ignore RTSP status/keepalive text bytes if any
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
            if channel != 0 || self.packet_buf.len() < 12 {
                continue;
            }

            // Parse RTP Header (12 bytes)
            let rtp_payload = &self.packet_buf[12..];
            if rtp_payload.is_empty() {
                continue;
            }

            let nal_type = rtp_payload[0] & 0x1F;

            // Type 1..23: Single NAL unit packet
            if (1..=23).contains(&nal_type) {
                let mut nalu = vec![0x00, 0x00, 0x00, 0x01];
                nalu.extend_from_slice(rtp_payload);
                return Ok(Some(nalu));
            }

            // Type 24: STAP-A (Single-Time Aggregation Packet)
            if nal_type == 24 {
                let mut offset = 1;
                let mut combined_nalus = Vec::new();
                while offset + 2 <= rtp_payload.len() {
                    let nalu_size = ((rtp_payload[offset] as usize) << 8) | (rtp_payload[offset + 1] as usize);
                    offset += 2;
                    if offset + nalu_size <= rtp_payload.len() {
                        combined_nalus.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                        combined_nalus.extend_from_slice(&rtp_payload[offset..offset + nalu_size]);
                        offset += nalu_size;
                    } else {
                        break;
                    }
                }
                if !combined_nalus.is_empty() {
                    return Ok(Some(combined_nalus));
                }
                continue;
            }

            // Type 28: FU-A (Fragmentation Unit)
            if nal_type == 28 {
                if rtp_payload.len() < 2 {
                    continue;
                }
                let fu_indicator = rtp_payload[0];
                let fu_header = rtp_payload[1];
                let is_start = (fu_header & 0x80) != 0;
                let is_end = (fu_header & 0x40) != 0;
                let original_nal_type = fu_header & 0x1F;
                let reconstructed_header = (fu_indicator & 0xE0) | original_nal_type;

                if is_start {
                    self.fu_buffer.clear();
                    self.fu_buffer.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, reconstructed_header]);
                    self.fu_buffer.extend_from_slice(&rtp_payload[2..]);
                } else if !self.fu_buffer.is_empty() {
                    self.fu_buffer.extend_from_slice(&rtp_payload[2..]);
                }

                if is_end && !self.fu_buffer.is_empty() {
                    let complete_nalu = std::mem::take(&mut self.fu_buffer);
                    return Ok(Some(complete_nalu));
                }
            }
        }
    }
}
