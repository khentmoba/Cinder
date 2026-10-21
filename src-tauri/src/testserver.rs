//! A small scripted HTTP server, used by the tests to stand in for a provider.
//!
//! It answers with a list of prepared replies, in order, so a test can say
//! "the first call fails, the second one works" and then check that the
//! fetcher really tried twice.


use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

/// One scripted answer.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub body: String,
    /// How long the server waits before answering.
    pub delay_ms: u64,
}

impl Reply {
    pub fn ok(body: &str) -> Self {
        Self { status: 200, body: body.to_string(), delay_ms: 0 }
    }
    pub fn status(status: u16, body: &str) -> Self {
        Self { status, body: body.to_string(), delay_ms: 0 }
    }
    pub fn slow(body: &str, delay_ms: u64) -> Self {
        Self { status: 200, body: body.to_string(), delay_ms }
    }
}

/// What the server saw.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

/// A server that answers with prepared replies.
pub struct MockServer {
    port: u16,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl MockServer {
    /// Start a server that answers with `replies`, in order.
    ///
    /// When the list runs out the server answers 404, which is a failure the
    /// fetcher does not retry.
    pub fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the test server should bind");
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let queue = Arc::new(Mutex::new(replies.into_iter().collect::<Vec<_>>().into_iter()));

        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let reply = {
                    let mut q = queue.lock().unwrap();
                    q.next()
                };
                let Some(reply) = reply else {
                    // Nothing left to say: 404, so the caller stops retrying.
                    let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    let _ = stream.flush();
                    continue;
                };
                let request = match read_request(&mut stream) {
                    Some(r) => r,
                    None => continue,
                };
                log.lock().unwrap().push(request);
                if reply.delay_ms > 0 {
                    thread::sleep(std::time::Duration::from_millis(reply.delay_ms));
                }
                let head = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    reply.status,
                    reason(reply.status),
                    reply.body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(reply.body.as_bytes());
                let _ = stream.flush();
            }
        });

        Self { port, requests: seen }
    }

    /// The base URL of the server.
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    /// A client that trusts the test server.
    pub fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("the test client should build")
    }

    /// The requests the server has seen so far.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

/// The reason phrase for a status code.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

/// Read one request off the connection.
fn read_request(stream: &mut std::net::TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut headers = HashMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((k, v)) = header.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok()?;
    }

    Some(Request { method, path, headers, body: String::from_utf8_lossy(&body).to_string() })
}
