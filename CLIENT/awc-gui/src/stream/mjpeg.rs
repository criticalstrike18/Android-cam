use std::io::Read;

pub fn find_marker(buffer: &[u8], marker: &[u8]) -> Option<usize> {
    if buffer.len() < marker.len() {
        return None;
    }
    for i in 0..=(buffer.len() - marker.len()) {
        if buffer[i..i + marker.len()] == *marker {
            return Some(i);
        }
    }
    None
}

const SOI: [u8; 2] = [0xFF, 0xD8];
const EOI: [u8; 2] = [0xFF, 0xD9];

pub fn read_next_jpeg(
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
    chunk: &mut [u8],
) -> Option<Vec<u8>> {
    const HEADER_MARKER: [u8; 4] = [13, 10, 13, 10]; // \r\n\r\n
    loop {
        let header_end = loop {
            if let Some(pos) = find_marker(buffer, &HEADER_MARKER) {
                break pos;
            }
            match reader.read(chunk) {
                Ok(0) => return None,
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                Err(_) => return None,
            }
        };

        let header_text = String::from_utf8_lossy(&buffer[0..header_end]);
        let content_length: Option<usize> = header_text
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .and_then(|v| v.trim().parse().ok());

        let payload_start = header_end + 4;

        if let Some(len) = content_length {
            let frame_end = payload_start + len;
            while buffer.len() < frame_end {
                match reader.read(chunk) {
                    Ok(0) => return None,
                    Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                    Err(_) => return None,
                }
            }

            let slice = &buffer[payload_start..frame_end];
            // Validate SOI marker to ensure no chunk headers/desync
            if let Some(soi_offset) = find_marker(slice, &SOI) {
                let actual_start = payload_start + soi_offset;
                let actual_slice = &buffer[actual_start..frame_end];
                let jpeg = actual_slice.to_vec();
                buffer.drain(..frame_end);
                return Some(jpeg);
            } else {
                // Malformed header / boundary desync: drain and retry
                buffer.drain(..frame_end);
                continue;
            }
        } else {
            // Fallback: search for EOI marker if Content-Length wasn't provided
            if let Some(soi_pos) = find_marker(&buffer[payload_start..], &SOI) {
                let actual_soi = payload_start + soi_pos;
                let eoi_pos = loop {
                    if let Some(pos) = find_marker(&buffer[actual_soi..], &EOI) {
                        break actual_soi + pos + 2;
                    }
                    match reader.read(chunk) {
                        Ok(0) => return None,
                        Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                        Err(_) => return None,
                    }
                };
                let jpeg = buffer[actual_soi..eoi_pos].to_vec();
                buffer.drain(..eoi_pos);
                return Some(jpeg);
            } else {
                buffer.drain(..payload_start);
                continue;
            }
        }
    }
}
