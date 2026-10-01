#[cfg(test)]
mod tests {
    use std::io::{Read, Write, BufRead, BufReader};
    use std::net::TcpStream;
    use std::time::{Duration, Instant};
    use rusty_h265::decoder::Decoder;

    fn decode_b64(input: &str) -> Option<Vec<u8>> {
        let mut map = [255u8; 256];
        for (i, &b) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate() {
            map[b as usize] = i as u8;
        }
        let bytes = input.trim().as_bytes();
        let mut out = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0;
        for &b in bytes {
            if b == b'=' { break; }
            let v = map[b as usize];
            if v == 255 { continue; }
            buf = (buf << 6) | (v as u32);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        Some(out)
    }

    #[test]

    #[ignore = "requires a live phone on ADB (adb forward tcp:8080/tcp:8554)"]
    fn test_live_hevc_decoding() {
        let mut stream = match TcpStream::connect("127.0.0.1:8554") {
            Ok(s) => s,
            Err(e) => {
                println!("Cannot connect to RTSP server: {}", e);
                return;
            }
        };
        stream.set_nodelay(true).unwrap();
        stream.set_read_timeout(Some(Duration::from_millis(2000))).unwrap();
        stream.set_write_timeout(Some(Duration::from_millis(2000))).unwrap();

        // 1. DESCRIBE
        let req = "DESCRIBE rtsp://127.0.0.1:8554/live RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n";
        stream.write_all(req.as_bytes()).unwrap();

        let reader_stream = stream.try_clone().unwrap();
        let mut reader = BufReader::new(reader_stream);
        let mut sdp = String::new();
        let mut content_length = 0;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 { break; }
            if line.to_lowercase().starts_with("content-length:") {
                content_length = line["content-length:".len()..].trim().parse().unwrap_or(0);
            }
            if line == "\r\n" || line == "\n" { break; }
        }
        if content_length > 0 {
            let mut buf = vec![0u8; content_length];
            reader.read_exact(&mut buf).unwrap();
            sdp = String::from_utf8_lossy(&buf).to_string();
        }
        println!("[SDP received] {} bytes", sdp.len());

        let is_hevc = sdp.contains("H265") || sdp.contains("hevc");
        println!("Is HEVC: {}", is_hevc);

        // Extract control URL for track 0
        let mut control_url = "rtsp://127.0.0.1:8554/live/streamid=0".to_string();
        for line in sdp.lines() {
            if line.starts_with("a=control:") {
                let track = line["a=control:".len()..].trim();
                if track.starts_with("rtsp://") {
                    control_url = track.to_string();
                } else if !track.is_empty() && track != "*" {
                    control_url = format!("rtsp://127.0.0.1:8554/live/{}", track);
                }
                break;
            }
        }
        println!("Control URL: {}", control_url);

        // 2. SETUP
        let req_setup = format!("SETUP {} RTSP/1.0\r\nCSeq: 2\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n", control_url);
        stream.write_all(req_setup.as_bytes()).unwrap();
        let mut session_id = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 { break; }
            if line.to_lowercase().starts_with("session:") {
                session_id = line["session:".len()..].split(';').next().unwrap_or("").trim().to_string();
            }
            if line == "\r\n" || line == "\n" { break; }
        }
        println!("Session ID: {}", session_id);

        // 3. PLAY
        let req_play = format!("PLAY rtsp://127.0.0.1:8554/live RTSP/1.0\r\nCSeq: 3\r\nSession: {}\r\nRange: npt=0.000-\r\n\r\n", session_id);
        stream.write_all(req_play.as_bytes()).unwrap();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 { break; }
            if line == "\r\n" || line == "\n" { break; }
        }

        reader.get_ref().set_read_timeout(Some(Duration::from_millis(500))).unwrap();

        let mut dec = Decoder::new();

        // Feed VPS/SPS/PPS from SDP
        for line in sdp.lines() {
            for tag in &["sprop-vps=", "sprop-sps=", "sprop-pps="] {
                if let Some(idx) = line.find(tag) {
                    let val = &line[idx + tag.len()..];
                    let val = val.split(';').next().unwrap_or(val).trim();
                    for part in val.split(',') {
                        if let Some(bytes) = decode_b64(part.trim()) {
                            let mut nal = vec![0x00, 0x00, 0x00, 0x01];
                            nal.extend_from_slice(&bytes);
                            let _ = dec.push_annexb(&nal, None);
                            println!("Fed SDP param {}: {} bytes", tag, bytes.len());
                        }
                    }
                }
            }
        }

        let mut fu_buf = Vec::new();
        let mut frames_decoded = 0;
        let mut yuv_buf = Vec::new();
        let mut decode_latencies = Vec::new();
        let mut yuv_latencies = Vec::new();
        let start = Instant::now();

        for _ in 0..1000 {
            let mut magic = [0u8; 1];
            if let Err(_) = reader.read_exact(&mut magic) {
                continue;
            }
            if magic[0] != b'$' { continue; }

            let mut hdr = [0u8; 3];
            if reader.read_exact(&mut hdr).is_err() { break; }
            let channel = hdr[0];
            let len = u16::from_be_bytes([hdr[1], hdr[2]]) as usize;

            let mut pkt = vec![0u8; len];
            if reader.read_exact(&mut pkt).is_err() { break; }
            if channel != 0 || pkt.len() < 12 { continue; }

            let payload = &pkt[12..];
            let nal_type = (payload[0] >> 1) & 0x3F;

            let nal_to_decode = if nal_type <= 47 {
                let mut nal = vec![0x00, 0x00, 0x00, 0x01];
                nal.extend_from_slice(payload);
                Some(nal)
            } else if nal_type == 49 && payload.len() >= 3 {
                let fu_header = payload[2];
                let start_bit = (fu_header & 0x80) != 0;
                let end_bit = (fu_header & 0x40) != 0;
                let inner_nal_type = fu_header & 0x3F;

                if start_bit {
                    fu_buf.clear();
                    fu_buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                    let h0 = (payload[0] & 0x81) | (inner_nal_type << 1);
                    let h1 = payload[1];
                    fu_buf.push(h0);
                    fu_buf.push(h1);
                    fu_buf.extend_from_slice(&payload[3..]);
                    None
                } else if !fu_buf.is_empty() {
                    fu_buf.extend_from_slice(&payload[3..]);
                    if end_bit {
                        let nal = std::mem::take(&mut fu_buf);
                        Some(nal)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(nal) = nal_to_decode {
                let t0 = Instant::now();
                if let Ok(_) = dec.push_annexb(&nal, None) {
                    if let Ok(f) = dec.next_frame() {
                        let dec_ms = t0.elapsed().as_secs_f64() * 1000.0;
                        decode_latencies.push(dec_ms);

                        let t_yuv = Instant::now();
                        yuv_buf.clear();
                        f.write_yuv(&mut yuv_buf);
                        let yuv_ms = t_yuv.elapsed().as_secs_f64() * 1000.0;
                        yuv_latencies.push(yuv_ms);

                        frames_decoded += 1;
                        println!(
                            "Frame #{:>2} | {:>4}x{:<4} | Decode: {:>5.2} ms | YUV export: {:>5.2} ms | Total: {:>5.2} ms | YUV bytes: {}",
                            frames_decoded, f.width, f.height, dec_ms, yuv_ms, dec_ms + yuv_ms, yuv_buf.len()
                        );
                        if frames_decoded >= 20 {
                            break;
                        }
                    }
                }
            }
        }

        // Send TEARDOWN cleanly
        let req_teardown = format!("TEARDOWN rtsp://127.0.0.1:8554/live RTSP/1.0\r\nCSeq: 4\r\nSession: {}\r\n\r\n", session_id);
        let _ = stream.write_all(req_teardown.as_bytes());

        let total_secs = start.elapsed().as_secs_f64();
        let avg_dec: f64 = decode_latencies.iter().sum::<f64>() / decode_latencies.len() as f64;
        let avg_yuv: f64 = yuv_latencies.iter().sum::<f64>() / yuv_latencies.len() as f64;
        println!("\n================ [H.265 LIVE 4K BENCHMARK RESULTS] ================");
        println!("Decoded Frames: {}", frames_decoded);
        println!("Wall Clock Time: {:.2}s", total_secs);
        println!("Average HEVC Decode Latency: {:.2} ms per 4K frame", avg_dec);
        println!("Average YUV Planar Extraction: {:.2} ms per 4K frame", avg_yuv);
        println!("Average Total Latency: {:.2} ms per 4K frame", avg_dec + avg_yuv);
        println!("Effective Stream Frame Rate: {:.2} FPS", frames_decoded as f64 / total_secs);
        println!("====================================================================\n");

        assert!(frames_decoded >= 10, "Should have decoded at least 10 HEVC frames");
    }
}
