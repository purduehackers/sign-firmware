use core::str::FromStr;

use esp_idf_svc::tls::EspAsyncTls;
use url::Url;

use crate::{convert_error, EspTlsSocket};

use super::generate_tls;

#[derive(Debug)]
pub enum WsMessage {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close,
}

pub struct WebSocket {
    tls: EspAsyncTls<EspTlsSocket>,
}

impl WebSocket {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let parsed = Url::from_str(url)?;
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("No host in URL"))?;
        let path = if let Some(q) = parsed.query() {
            format!("{}?{}", parsed.path(), q)
        } else {
            parsed.path().to_string()
        };

        // Generate random 16-byte key using ESP hardware RNG
        let mut key_bytes = [0u8; 16];
        unsafe {
            esp_idf_svc::sys::esp_fill_random(key_bytes.as_mut_ptr() as *mut core::ffi::c_void, 16);
        }
        let ws_key = base64_encode(&key_bytes);

        // Use wss:// -> connect via TLS
        let tls_url = url
            .replace("ws://", "http://")
            .replace("wss://", "https://");
        let tls = generate_tls(&tls_url).await?;

        // Send WebSocket upgrade request
        let req = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {host}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {ws_key}\r\n\
             Sec-WebSocket-Version: 13\r\n\
             User-Agent: PHSign/1.0.0\r\n\
             \r\n"
        );

        tls.write_all(req.as_bytes()).await.map_err(convert_error)?;

        // Read response headers
        let mut header_buf = Vec::with_capacity(1024);
        let mut b = [0u8; 1];
        loop {
            let n = tls.read(&mut b).await.map_err(convert_error)?;
            if n == 0 {
                anyhow::bail!("Connection closed during WebSocket handshake");
            }
            header_buf.push(b[0]);
            if header_buf.len() >= 4 && header_buf[header_buf.len() - 4..] == *b"\r\n\r\n" {
                break;
            }
        }

        let header_str = String::from_utf8(header_buf)?;
        let status: u16 = header_str
            .split_whitespace()
            .nth(1)
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);

        if status != 101 {
            anyhow::bail!("WebSocket handshake failed with status {status}");
        }

        Ok(WebSocket { tls })
    }

    pub async fn send(&mut self, msg: &WsMessage) -> anyhow::Result<()> {
        let (opcode, payload) = match msg {
            WsMessage::Text(s) => (0x01, s.as_bytes().to_vec()),
            WsMessage::Binary(b) => (0x02, b.clone()),
            WsMessage::Ping(b) => (0x09, b.clone()),
            WsMessage::Pong(b) => (0x0A, b.clone()),
            WsMessage::Close => (0x08, Vec::new()),
        };

        let frame = encode_frame(opcode, &payload)?;
        self.tls.write_all(&frame).await.map_err(convert_error)?;

        Ok(())
    }

    pub async fn recv(&mut self) -> anyhow::Result<WsMessage> {
        loop {
            let msg = self.read_frame().await?;
            match &msg {
                WsMessage::Ping(data) => {
                    let pong = WsMessage::Pong(data.clone());
                    self.send(&pong).await?;
                    continue;
                }
                _ => return Ok(msg),
            }
        }
    }

    async fn read_frame(&mut self) -> anyhow::Result<WsMessage> {
        let mut header = [0u8; 2];
        read_exact(&mut self.tls, &mut header).await?;

        let opcode = header[0] & 0x0F;
        let masked = header[1] & 0x80 != 0;
        let mut payload_len = (header[1] & 0x7F) as u64;

        if payload_len == 126 {
            let mut buf = [0u8; 2];
            read_exact(&mut self.tls, &mut buf).await?;
            payload_len = u16::from_be_bytes(buf) as u64;
        } else if payload_len == 127 {
            let mut buf = [0u8; 8];
            read_exact(&mut self.tls, &mut buf).await?;
            payload_len = u64::from_be_bytes(buf);
        }

        let mask_key = if masked {
            let mut buf = [0u8; 4];
            read_exact(&mut self.tls, &mut buf).await?;
            Some(buf)
        } else {
            None
        };

        let mut payload = vec![0u8; payload_len as usize];
        read_exact(&mut self.tls, &mut payload).await?;

        if let Some(mask) = mask_key {
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[i % 4];
            }
        }

        match opcode {
            0x01 => Ok(WsMessage::Text(String::from_utf8(payload)?)),
            0x02 => Ok(WsMessage::Binary(payload)),
            0x08 => Ok(WsMessage::Close),
            0x09 => Ok(WsMessage::Ping(payload)),
            0x0A => Ok(WsMessage::Pong(payload)),
            _ => anyhow::bail!("Unknown WebSocket opcode: {opcode}"),
        }
    }

    pub async fn close(&mut self) -> anyhow::Result<()> {
        self.send(&WsMessage::Close).await
    }
}

async fn read_exact(tls: &mut EspAsyncTls<EspTlsSocket>, buf: &mut [u8]) -> anyhow::Result<()> {
    let mut offset = 0;
    while offset < buf.len() {
        let n = tls.read(&mut buf[offset..]).await.map_err(convert_error)?;
        if n == 0 {
            anyhow::bail!("Connection closed unexpectedly");
        }
        offset += n;
    }
    Ok(())
}

fn encode_frame(opcode: u8, payload: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut frame = Vec::new();

    // FIN bit + opcode
    frame.push(0x80 | opcode);

    // Mask bit (client must mask) + payload length
    let len = payload.len();
    if len < 126 {
        frame.push(0x80 | len as u8);
    } else if len <= 65535 {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(len as u64).to_be_bytes());
    }

    // Generate random mask
    let mut mask = [0u8; 4];
    unsafe {
        esp_idf_svc::sys::esp_fill_random(mask.as_mut_ptr() as *mut core::ffi::c_void, 4);
    }
    frame.extend_from_slice(&mask);

    // Masked payload
    for (i, &byte) in payload.iter().enumerate() {
        frame.push(byte ^ mask[i % 4]);
    }

    Ok(frame)
}

// Minimal base64 encode — only needs to handle 16 bytes for WebSocket key
const BASE64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(input: &[u8]) -> String {
    let mut output = String::with_capacity((input.len() + 2) / 3 * 4);

    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };

        let triple = (b0 << 16) | (b1 << 8) | b2;

        output.push(BASE64_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        output.push(BASE64_CHARS[((triple >> 12) & 0x3F) as usize] as char);

        if chunk.len() > 1 {
            output.push(BASE64_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            output.push('=');
        }

        if chunk.len() > 2 {
            output.push(BASE64_CHARS[(triple & 0x3F) as usize] as char);
        } else {
            output.push('=');
        }
    }

    output
}
