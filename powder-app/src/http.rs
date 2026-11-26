//! Minimal HTTP/1.1 server for the local interface: one thread per connection, one request per
//! connection (Connection: close). Small and predictable - no keep-alive state to go wrong.
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;

pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>, // lower-case names
    pub body: Vec<u8>,
    stream: TcpStream,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(|s| s.as_str())
    }

    fn head(&mut self, status: u16, headers: &[(&str, String)], len: Option<u64>) -> std::io::Result<()> {
        let reason = match status {
            200 => "OK",
            206 => "Partial Content",
            403 => "Forbidden",
            404 => "Not Found",
            416 => "Range Not Satisfiable",
            _ => "OK",
        };
        let mut h = format!("HTTP/1.1 {status} {reason}\r\nConnection: close\r\nCache-Control: no-store\r\n");
        for (k, v) in headers {
            h.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(l) = len {
            h.push_str(&format!("Content-Length: {l}\r\n"));
        }
        h.push_str("\r\n");
        self.stream.write_all(h.as_bytes())
    }

    pub fn respond(mut self, status: u16, ctype: &str, body: &[u8]) {
        if self.head(status, &[("Content-Type", ctype.to_string())], Some(body.len() as u64)).is_ok() && self.method != "HEAD" {
            let _ = self.stream.write_all(body);
        }
        let _ = self.stream.flush();
    }

    pub fn status(self, code: u16) {
        self.respond(code, "text/plain", b"");
    }

    /// Stream a file with HTTP Range support (video seeking).
    pub fn file(mut self, path: &Path) {
        let Ok(mut f) = std::fs::File::open(path) else { return self.status(404) };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let ctype = mime_guess::from_path(path).first_or_octet_stream().to_string();
        let range = self.header("range").and_then(|r| r.strip_prefix("bytes=")).map(|s| s.to_string());
        let (status, start, end) = match range {
            Some(r) => {
                let (a, b) = r.split_once('-').unwrap_or((&r, ""));
                let start: u64 = if a.is_empty() { len.saturating_sub(b.parse().unwrap_or(0)) } else { a.parse().unwrap_or(0) };
                if len == 0 || start >= len {
                    let _ = self.head(416, &[("Content-Range", format!("bytes */{len}"))], Some(0));
                    return;
                }
                let end: u64 = if a.is_empty() || b.is_empty() { len - 1 } else { b.parse().unwrap_or(len - 1).min(len - 1) };
                (206, start, end)
            }
            None => (200, 0, len.saturating_sub(1)),
        };
        let n = if len == 0 { 0 } else { end - start + 1 };
        let mut hs = vec![("Content-Type", ctype), ("Accept-Ranges", "bytes".to_string())];
        if status == 206 {
            hs.push(("Content-Range", format!("bytes {start}-{end}/{len}")));
        }
        if self.head(status, &hs, Some(n)).is_err() || self.method == "HEAD" {
            return;
        }
        let _ = f.seek(SeekFrom::Start(start));
        let _ = std::io::copy(&mut f.take(n), &mut self.stream);
        let _ = self.stream.flush();
    }
}

fn read_request(stream: TcpStream) -> Option<Request> {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
    let mut r = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let url = parts.next()?.to_string();
    let mut headers = HashMap::new();
    loop {
        let mut l = String::new();
        if r.read_line(&mut l).ok()? == 0 {
            break;
        }
        let l = l.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0u8; len.min(512 * 1024 * 1024)];
    if len > 0 {
        r.read_exact(&mut body).ok()?;
    }
    let _ = stream.set_read_timeout(None);
    Some(Request { method, url, headers, body, stream })
}

/// Listen on 127.0.0.1:port (0 = any free port) and call `handler` for every request on its own thread.
pub fn serve<F>(port: u16, handler: F) -> std::io::Result<u16>
where
    F: Fn(Request) + Send + Sync + 'static,
{
    let l = TcpListener::bind(("127.0.0.1", port))?;
    let port = l.local_addr()?.port();
    let h = std::sync::Arc::new(handler);
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let h = h.clone();
            std::thread::spawn(move || {
                let _ = s.set_nodelay(true);
                if let Some(req) = read_request(s) {
                    h(req);
                }
            });
        }
    });
    Ok(port)
}
