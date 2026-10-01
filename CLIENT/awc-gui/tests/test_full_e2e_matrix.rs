use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use awc_gui::stream::rtsp::InProcessRtspDecoder;
use awc_gui::stream::rtsp_client::RtspCodec;

#[cfg(test)]
mod tests {
    use super::*;

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

    fn send_setting(payload: &str) -> bool {
        let mut stream = match TcpStream::connect("127.0.0.1:8080") {
            Ok(s) => s,
            Err(_) => return false,
        };
        let _ = stream.set_read_timeout(Some(Duration::from_millis(3000)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(3000)));

        let req = format!(
            "POST /settings HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            payload.len(),
            payload
        );
        if stream.write_all(req.as_bytes()).is_err() {
            return false;
        }

        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        true
    }

    #[derive(Debug, Clone)]
    struct StreamStats {
        codec: String,
        resolution: (u32, u32),
        preview_resolution: (u32, u32),
        frames_decoded: usize,
        fps: f64,
        avg_decode_ms: f64,
        p95_decode_ms: f64,
        max_decode_ms: f64,
    }

    fn sample_rtsp_stream(target_frames: usize, timeout_secs: u64) -> Result<StreamStats, String> {
        let mut stream = TcpStream::connect("127.0.0.1:8554")
            .map_err(|e| format!("Failed to connect to RTSP 8554: {}", e))?;
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
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).unwrap();
            sdp = String::from_utf8_lossy(&body).to_string();
        }

        let is_hevc = sdp.contains("H265") || sdp.contains("h265");
        let codec_enum = if is_hevc { RtspCodec::H265 } else { RtspCodec::H264 };

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

        // 3. PLAY
        let req_play = format!("PLAY rtsp://127.0.0.1:8554/live RTSP/1.0\r\nCSeq: 3\r\nSession: {}\r\nRange: npt=0.000-\r\n\r\n", session_id);
        stream.write_all(req_play.as_bytes()).unwrap();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 { break; }
            if line == "\r\n" || line == "\n" { break; }
        }

        reader.get_ref().set_read_timeout(Some(Duration::from_millis(500))).unwrap();

        let mut decoder = InProcessRtspDecoder::new(codec_enum).map_err(|e| format!("Decoder create error: {}", e))?;

        if is_hevc {
            for line in sdp.lines() {
                for tag in &["sprop-vps=", "sprop-sps=", "sprop-pps="] {
                    if let Some(idx) = line.find(tag) {
                        let val = &line[idx + tag.len()..];
                        let val = val.split(';').next().unwrap_or(val).trim();
                        for part in val.split(',') {
                            if let Some(bytes) = decode_b64(part.trim()) {
                                let mut nal = vec![0x00, 0x00, 0x00, 0x01];
                                nal.extend_from_slice(&bytes);
                                decoder.decode_nalu_ignore(&nal);
                            }
                        }
                    }
                }
            }
        } else {
            for line in sdp.lines() {
                if let Some(idx) = line.find("sprop-parameter-sets=") {
                    let val = &line[idx + "sprop-parameter-sets=".len()..];
                    let val = val.split(';').next().unwrap_or(val).trim();
                    for part in val.split(',') {
                        if let Some(bytes) = decode_b64(part.trim()) {
                            let mut nal = vec![0x00, 0x00, 0x00, 0x01];
                            nal.extend_from_slice(&bytes);
                            decoder.decode_nalu_ignore(&nal);
                        }
                    }
                }
            }
        }

        let mut fu_buf = Vec::new();
        let mut frames_decoded = 0;
        let mut nv12_buf = Vec::new();
        let mut rgba_buf = Vec::new();
        let mut decode_latencies = Vec::new();
        let mut final_res = (0, 0);
        let mut prev_res = (0, 0);

        let mut first_frame_time: Option<Instant> = None;
        let mut last_frame_time: Option<Instant> = None;
        let mut warmup_frames = 0usize;

        let start_test = Instant::now();
        while frames_decoded < target_frames && start_test.elapsed() < Duration::from_secs(timeout_secs) {
            let mut magic = [0u8; 1];
            if reader.read_exact(&mut magic).is_err() {
                continue;
            }
            if magic[0] != b'$' {
                continue;
            }
            let mut hdr = [0u8; 3];
            if reader.read_exact(&mut hdr).is_err() {
                break;
            }
            let channel = hdr[0];
            let length = u16::from_be_bytes([hdr[1], hdr[2]]) as usize;
            let mut packet = vec![0u8; length];
            if reader.read_exact(&mut packet).is_err() {
                break;
            }

            if channel != 0 || packet.len() < 12 {
                continue;
            }

            let payload = &packet[12..];
            if payload.is_empty() {
                continue;
            }

            let nal_to_decode: Option<Vec<u8>> = if is_hevc {
                let nal_type = (payload[0] >> 1) & 0x3F;
                if nal_type == 49 && payload.len() >= 3 {
                    let fu_header = payload[2];
                    let is_start = (fu_header & 0x80) != 0;
                    let is_end = (fu_header & 0x40) != 0;
                    let orig_type = fu_header & 0x3F;

                    let byte0 = (payload[0] & 0x81) | (orig_type << 1);
                    let byte1 = payload[1];

                    if is_start {
                        fu_buf.clear();
                        fu_buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, byte0, byte1]);
                        fu_buf.extend_from_slice(&payload[3..]);
                        None
                    } else if !fu_buf.is_empty() {
                        fu_buf.extend_from_slice(&payload[3..]);
                        if is_end {
                            Some(std::mem::take(&mut fu_buf))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else if nal_type <= 39 {
                    let mut nal = vec![0x00, 0x00, 0x00, 0x01];
                    nal.extend_from_slice(payload);
                    Some(nal)
                } else {
                    None
                }
            } else {
                let nal_type = payload[0] & 0x1F;
                if (1..=23).contains(&nal_type) {
                    let mut nal = vec![0x00, 0x00, 0x00, 0x01];
                    nal.extend_from_slice(payload);
                    Some(nal)
                } else if nal_type == 28 && payload.len() >= 2 {
                    let fu_indicator = payload[0];
                    let fu_header = payload[1];
                    let is_start = (fu_header & 0x80) != 0;
                    let is_end = (fu_header & 0x40) != 0;
                    let orig_type = fu_header & 0x1F;
                    let reconstructed = (fu_indicator & 0xE0) | orig_type;

                    if is_start {
                        fu_buf.clear();
                        fu_buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, reconstructed]);
                        fu_buf.extend_from_slice(&payload[2..]);
                        None
                    } else if !fu_buf.is_empty() {
                        fu_buf.extend_from_slice(&payload[2..]);
                        if is_end {
                            Some(std::mem::take(&mut fu_buf))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            if let Some(nal) = nal_to_decode {
                let t0 = Instant::now();
                if let Ok(Some(res)) = decoder.decode_into(&nal, &mut nv12_buf, &mut rgba_buf) {
                    let dec_ms = t0.elapsed().as_secs_f64() * 1000.0;
                    decode_latencies.push(dec_ms);

                    warmup_frames += 1;
                    if warmup_frames >= 3 {
                        if first_frame_time.is_none() {
                            first_frame_time = Some(Instant::now());
                        }
                        last_frame_time = Some(Instant::now());
                    }

                    final_res = (res.src_w, res.src_h);
                    prev_res = (res.prev_w, res.prev_h);
                    frames_decoded += 1;
                }
            }
        }

        // TEARDOWN
        let req_teardown = format!("TEARDOWN rtsp://127.0.0.1:8554/live RTSP/1.0\r\nCSeq: 4\r\nSession: {}\r\n\r\n", session_id);
        let _ = stream.write_all(req_teardown.as_bytes());

        if frames_decoded == 0 {
            return Err("No frames could be decoded from stream".to_string());
        }

        let steady_frames = frames_decoded.saturating_sub(2);
        let fps = if steady_frames > 1 {
            if let (Some(t_start), Some(t_end)) = (first_frame_time, last_frame_time) {
                let dur = t_end.duration_since(t_start).as_secs_f64();
                if dur > 0.0 {
                    (steady_frames - 1) as f64 / dur
                } else {
                    0.0
                }
            } else {
                0.0
            }
        } else {
            0.0
        };

        decode_latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let avg_dec = decode_latencies.iter().sum::<f64>() / decode_latencies.len() as f64;
        let p95_idx = ((decode_latencies.len() as f64 * 0.95).floor() as usize).min(decode_latencies.len() - 1);
        let p95_dec = decode_latencies[p95_idx];
        let max_dec = *decode_latencies.last().unwrap_or(&0.0);

        Ok(StreamStats {
            codec: if is_hevc { "H.265 (HEVC)".to_string() } else { "H.264 (AVC)".to_string() },
            resolution: final_res,
            preview_resolution: prev_res,
            frames_decoded,
            fps,
            avg_decode_ms: avg_dec,
            p95_decode_ms: p95_dec,
            max_decode_ms: max_dec,
        })
    }

    #[test]

    #[ignore = "requires a live phone on ADB (adb forward tcp:8080/tcp:8554)"]
    fn test_full_release_matrix_on_hardware() {
        println!("\n==========================================================================");
        println!("  FULL RESOLUTION LADDER BENCHMARK: H.264 (AVC) vs H.265 (HEVC)");
        println!("  TARGET HARDWARE: Samsung Galaxy M51 (SM-M515F) -> Host Ryzen 7 (16 threads)");
        println!("==========================================================================\n");

        struct TestCase {
            label: &'static str,
            camera: &'static str,
            res: &'static str,
        }

        let test_cases = vec![
            TestCase { label: "480p", camera: "back", res: "640x480" },
            TestCase { label: "720p", camera: "back", res: "1280x720" },
            TestCase { label: "1080p", camera: "back", res: "1920x1080" },
            TestCase { label: "1440p (2K)", camera: "back", res: "2560x1440" },
            TestCase { label: "4K UHD (Rear)", camera: "back", res: "3840x2160" },
            TestCase { label: "4K UHD (Front)", camera: "front", res: "3840x2160" },
        ];

        let mut h264_results = Vec::new();
        let mut h265_results = Vec::new();

        // 1. BENCHMARK H.264
        println!(">>> [SECTION 1/2] BENCHMARKING H.264 (AVC) ACROSS ALL RESOLUTIONS <<<");
        for tc in &test_cases {
            print!("  -> Testing H.264 {} ({} cam)... ", tc.label, tc.camera);
            let payload = format!(r#"{{"camera":"{}","video_codec":"h264","resolution_str":"{}"}}"#, tc.camera, tc.res);
            assert!(send_setting(&payload));
            std::thread::sleep(Duration::from_millis(2500));
            match sample_rtsp_stream(25, 6) {
                Ok(stats) => {
                    println!("OK: {:.2} FPS | Avg: {:.2}ms (p95: {:.2}ms) | Dim: {}x{}",
                        stats.fps, stats.avg_decode_ms, stats.p95_decode_ms, stats.resolution.0, stats.resolution.1);
                    h264_results.push((tc.label, stats));
                }
                Err(e) => {
                    println!("ERROR: {}", e);
                }
            }
        }

        // Reset back camera
        let _ = send_setting(r#"{"camera":"back"}"#);
        std::thread::sleep(Duration::from_millis(1500));

        // 2. BENCHMARK H.265
        println!("\n>>> [SECTION 2/2] BENCHMARKING H.265 (HEVC) ACROSS ALL RESOLUTIONS <<<");
        for tc in &test_cases {
            print!("  -> Testing H.265 {} ({} cam)... ", tc.label, tc.camera);
            let payload = format!(r#"{{"camera":"{}","video_codec":"h265","resolution_str":"{}"}}"#, tc.camera, tc.res);
            assert!(send_setting(&payload));
            std::thread::sleep(Duration::from_millis(2500));
            match sample_rtsp_stream(25, 6) {
                Ok(stats) => {
                    println!("OK: {:.2} FPS | Avg: {:.2}ms (p95: {:.2}ms) | Dim: {}x{}",
                        stats.fps, stats.avg_decode_ms, stats.p95_decode_ms, stats.resolution.0, stats.resolution.1);
                    h265_results.push((tc.label, stats));
                }
                Err(e) => {
                    println!("ERROR: {}", e);
                }
            }
        }

        // Reset to back camera 1080p
        let _ = send_setting(r#"{"camera":"back","video_codec":"h264","resolution_str":"1920x1080"}"#);

        println!("\n=========================================================================================================");
        println!("                                  COMPREHENSIVE BENCHMARK COMPARISON TABLE");
        println!("=========================================================================================================");
        println!("{:<16} | {:<22} | {:<22} | {:<16}", "Resolution", "H.264 (FPS / Avg Lat)", "H.265 (FPS / Avg Lat)", "Delta Latency");
        println!("-----------------+------------------------+------------------------+-------------------------------------");

        for i in 0..test_cases.len() {
            let label = test_cases[i].label;
            let h264_str = if let Some((_, s)) = h264_results.get(i) {
                format!("{:>5.2} FPS / {:>5.2}ms", s.fps, s.avg_decode_ms)
            } else {
                "N/A".to_string()
            };

            let h265_str = if let Some((_, s)) = h265_results.get(i) {
                format!("{:>5.2} FPS / {:>5.2}ms", s.fps, s.avg_decode_ms)
            } else {
                "N/A".to_string()
            };

            let delta = match (h264_results.get(i), h265_results.get(i)) {
                (Some((_, s4)), Some((_, s5))) => {
                    let diff = s5.avg_decode_ms - s4.avg_decode_ms;
                    if diff > 0.0 {
                        format!("H.264 is {:>4.1}x faster", s5.avg_decode_ms / s4.avg_decode_ms)
                    } else {
                        format!("H.265 is {:>4.1}x faster", s4.avg_decode_ms / s5.avg_decode_ms)
                    }
                }
                _ => "-".to_string(),
            };

            println!("{:<16} | {:<22} | {:<22} | {:<16}", label, h264_str, h265_str, delta);
        }

        println!("=========================================================================================================\n");
    }
}
