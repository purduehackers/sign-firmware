use core::str::FromStr;

use esp_idf_svc::tls::EspAsyncTls;
use url::Url;

use crate::{convert_error, EspTlsSocket};

use super::generate_tls;

pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name_lower = name.to_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_lowercase() == name_lower)
            .map(|(_, v)| v.as_str())
    }
}

fn build_request_head(method: &str, url: &Url, headers: &[(&str, &str)]) -> String {
    let path = if let Some(q) = url.query() {
        format!("{}?{}", url.path(), q)
    } else {
        url.path().to_string()
    };
    let host = url.host_str().unwrap_or("");

    let mut req =
        format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: PHSign/1.0.0\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("Connection: close\r\n\r\n");
    req
}

async fn read_response(tls: &mut EspAsyncTls<EspTlsSocket>) -> anyhow::Result<HttpResponse> {
    // Read headers byte by byte until we hit \r\n\r\n
    let mut header_buf = Vec::with_capacity(2048);
    let mut b = [0u8; 1];

    loop {
        let n = tls.read(&mut b).await.map_err(convert_error)?;
        if n == 0 {
            anyhow::bail!("Connection closed before headers complete");
        }
        header_buf.push(b[0]);

        if header_buf.len() >= 4 && header_buf[header_buf.len() - 4..] == *b"\r\n\r\n" {
            break;
        }

        if header_buf.len() > 16384 {
            anyhow::bail!("Headers too large");
        }
    }

    let header_str = String::from_utf8(header_buf)?;
    let mut lines = header_str.split("\r\n");

    // Parse status line
    let status_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing status line"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("Missing status code"))?
        .parse::<u16>()?;

    // Parse headers
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(": ") {
            headers.push((k.to_string(), v.to_string()));
        }
    }

    // Read body based on Content-Length
    let content_length: Option<usize> = headers
        .iter()
        .find(|(k, _)| k.to_lowercase() == "content-length")
        .and_then(|(_, v)| v.parse().ok());

    let body = if let Some(len) = content_length {
        let mut body = vec![0u8; len];
        let mut read_total = 0;
        while read_total < len {
            let n = tls
                .read(&mut body[read_total..])
                .await
                .map_err(convert_error)?;
            if n == 0 {
                break;
            }
            read_total += n;
        }
        body.truncate(read_total);
        body
    } else {
        // Read until EOF
        let mut body = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = tls.read(&mut buf).await.map_err(convert_error)?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n]);
        }
        body
    };

    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

pub async fn http_get(url: &str, headers: &[(&str, &str)]) -> anyhow::Result<HttpResponse> {
    let parsed = Url::from_str(url)?;
    let mut tls = generate_tls(url).await?;

    let req = build_request_head("GET", &parsed, headers);
    tls.write_all(req.as_bytes()).await.map_err(convert_error)?;

    read_response(&mut tls).await
}

pub async fn http_post(
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> anyhow::Result<HttpResponse> {
    let parsed = Url::from_str(url)?;
    let mut tls = generate_tls(url).await?;

    let len_str = body.len().to_string();
    let mut all_headers = headers.to_vec();
    all_headers.push(("Content-Length", &len_str));

    let req = build_request_head("POST", &parsed, &all_headers);
    tls.write_all(req.as_bytes()).await.map_err(convert_error)?;
    tls.write_all(body).await.map_err(convert_error)?;

    read_response(&mut tls).await
}

/// Follows redirects for a GET request. Returns the final TLS stream and response
/// (with headers parsed but body not yet fully consumed — caller can stream from tls).
pub async fn follow_redirect(
    url: &str,
    headers: &[(&str, &str)],
) -> anyhow::Result<(EspAsyncTls<EspTlsSocket>, HttpResponse)> {
    let mut current_url = url.to_string();

    for _ in 0..5 {
        let parsed = Url::from_str(&current_url)?;
        let mut tls = generate_tls(&current_url).await?;

        let req = build_request_head("GET", &parsed, headers);
        tls.write_all(req.as_bytes()).await.map_err(convert_error)?;

        let resp = read_response(&mut tls).await?;

        if (300..400).contains(&resp.status) {
            if let Some(location) = resp.header("location") {
                current_url = location.to_string();
                continue;
            }
        }

        return Ok((tls, resp));
    }

    anyhow::bail!("Too many redirects")
}

/// Like follow_redirect but returns the TLS stream positioned at the start of the body
/// (headers already consumed). Used for streaming downloads like OTA.
pub async fn follow_redirect_stream(
    url: &str,
    headers: &[(&str, &str)],
) -> anyhow::Result<EspAsyncTls<EspTlsSocket>> {
    let mut current_url = url.to_string();

    for _ in 0..5 {
        let parsed = Url::from_str(&current_url)?;
        let tls = generate_tls(&current_url).await?;

        let req = build_request_head("GET", &parsed, headers);
        tls.write_all(req.as_bytes()).await.map_err(convert_error)?;

        // Read headers only (byte by byte until \r\n\r\n)
        let mut header_buf = Vec::with_capacity(2048);
        let mut b = [0u8; 1];
        loop {
            let n = tls.read(&mut b).await.map_err(convert_error)?;
            if n == 0 {
                anyhow::bail!("Connection closed before headers complete");
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

        if (300..400).contains(&status) {
            // Find Location header
            for line in header_str.split("\r\n") {
                if let Some(loc) = line
                    .strip_prefix("Location: ")
                    .or_else(|| line.strip_prefix("location: "))
                {
                    current_url = loc.to_string();
                    break;
                }
            }
            continue;
        }

        // Body starts here — return the stream
        return Ok(tls);
    }

    anyhow::bail!("Too many redirects")
}
