use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct WebSocketClient {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
}

impl WebSocketClient {
    pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<Self, String> {
        let addr = format!("{}:{}", host, port);
        let stream = match std::net::ToSocketAddrs::to_socket_addrs(&addr) {
            Ok(mut addrs) => match addrs.next() {
                Some(sa) => TcpStream::connect_timeout(&sa, timeout)
                    .map_err(|e| format!("Connect to {} timed out: {}", addr, e))?,
                None => return Err(format!("Could not resolve {}", addr)),
            },
            Err(e) => return Err(format!("Invalid address {}: {}", addr, e)),
        };

        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_millis(1000)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_millis(1000)))
            .map_err(|e| e.to_string())?;

        let handshake = format!(
            "GET /ws HTTP/1.1\r\n\
             Host: {}:{}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n",
            host, port
        );

        let mut stream_clone = stream.try_clone().map_err(|e| e.to_string())?;
        stream_clone
            .write_all(handshake.as_bytes())
            .map_err(|e| format!("Handshake write error: {}", e))?;

        let mut reader = BufReader::new(stream_clone);
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|e| format!("Handshake read error: {}", e))?;

        if !status_line.contains("101") {
            return Err(format!("WebSocket handshake rejected: {}", status_line.trim()));
        }

        // Read remaining handshake headers until empty line
        loop {
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .map_err(|e| format!("Header read error: {}", e))?;
            if bytes == 0 || line == "\r\n" || line == "\n" {
                break;
            }
        }

        // Set low read timeout on the reader's stream for non-blocking poll loop in worker
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(60)))
            .map_err(|e| e.to_string())?;

        Ok(Self {
            stream,
            reader,
        })
    }

    pub fn send_text(&mut self, text: &str) -> Result<(), String> {
        let payload = text.as_bytes();
        let payload_len = payload.len();
        let mut frame = Vec::with_capacity(payload_len + 14);

        // FIN + Text opcode (0x1)
        frame.push(0x81);

        let mask_key: [u8; 4] = [0x1B, 0x3F, 0x7A, 0x9C];

        if payload_len <= 125 {
            frame.push(0x80 | (payload_len as u8));
        } else if payload_len <= 65535 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(payload_len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(payload_len as u64).to_be_bytes());
        }

        frame.extend_from_slice(&mask_key);

        for (i, &b) in payload.iter().enumerate() {
            frame.push(b ^ mask_key[i % 4]);
        }

        self.stream
            .write_all(&frame)
            .map_err(|e| format!("WS write failed: {}", e))?;
        self.stream.flush().map_err(|e| format!("WS flush failed: {}", e))
    }

    pub fn read_text(&mut self) -> Result<Option<String>, String> {
        let mut hdr = [0u8; 2];
        match self.reader.read_exact(&mut hdr) {
            Ok(()) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok(None);
            }
            Err(e) => return Err(format!("WS read error: {}", e)),
        }

        let opcode = hdr[0] & 0x0F;
        let is_masked = (hdr[1] & 0x80) != 0;
        let mut payload_len = (hdr[1] & 0x7F) as usize;

        if opcode == 0x8 {
            return Err("WS closed by server".to_string());
        }

        if payload_len == 126 {
            let mut ext = [0u8; 2];
            self.reader
                .read_exact(&mut ext)
                .map_err(|e| format!("WS read len16 error: {}", e))?;
            payload_len = u16::from_be_bytes(ext) as usize;
        } else if payload_len == 127 {
            let mut ext = [0u8; 8];
            self.reader
                .read_exact(&mut ext)
                .map_err(|e| format!("WS read len64 error: {}", e))?;
            payload_len = u64::from_be_bytes(ext) as usize;
        }

        let mask = if is_masked {
            let mut m = [0u8; 4];
            self.reader
                .read_exact(&mut m)
                .map_err(|e| format!("WS read mask error: {}", e))?;
            Some(m)
        } else {
            None
        };

        let mut data = vec![0u8; payload_len];
        self.reader
            .read_exact(&mut data)
            .map_err(|e| format!("WS read payload error: {}", e))?;

        if let Some(m) = mask {
            for (i, b) in data.iter_mut().enumerate() {
                *b ^= m[i % 4];
            }
        }

        if opcode == 0x9 {
            // Ping frame -> reply with masked Pong (opcode 0xA) with identical payload (RFC 6455 5.5.3)
            let p_len = data.len();
            let mut pong = Vec::with_capacity(p_len + 14);
            pong.push(0x8A);
            let mask_key: [u8; 4] = [0x5A, 0x3B, 0x1C, 0x7E];
            if p_len <= 125 {
                pong.push(0x80 | (p_len as u8));
            } else if p_len <= 65535 {
                pong.push(0x80 | 126);
                pong.extend_from_slice(&(p_len as u16).to_be_bytes());
            } else {
                pong.push(0x80 | 127);
                pong.extend_from_slice(&(p_len as u64).to_be_bytes());
            }
            pong.extend_from_slice(&mask_key);
            for (i, &b) in data.iter().enumerate() {
                pong.push(b ^ mask_key[i % 4]);
            }
            let _ = self.stream.write_all(&pong);
            let _ = self.stream.flush();
            return Ok(None);
        }

        if opcode == 0x1 {
            String::from_utf8(data)
                .map(Some)
                .map_err(|e| format!("Invalid UTF-8 in WS text frame: {}", e))
        } else {
            Ok(None)
        }
    }
}
