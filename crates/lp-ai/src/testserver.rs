//! A throwaway HTTP server for tests: answers every request with the next canned body
//! and records what it received.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

pub struct Received {
    pub path: String,
    pub headers: String,
    pub body: String,
}

pub struct TestServer {
    pub url: String,
    pub received: Arc<Mutex<Vec<Received>>>,
}

impl TestServer {
    /// Replies cycle through `responses`; each is (status, body).
    pub fn start(responses: Vec<(u16, String)>) -> TestServer {
        Self::start_with_headers(responses, String::new())
    }

    /// `extra_headers` is raw header text, each line ending in CRLF, added to every reply.
    pub fn start_with_headers(responses: Vec<(u16, String)>, extra_headers: String) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let received = Arc::new(Mutex::new(Vec::new()));
        let log = received.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
                    continue;
                }
                let mut headers = String::new();
                let mut content_length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap();
                    }
                    headers.push_str(&line);
                }
                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).unwrap();
                let i = log.lock().unwrap().len();
                log.lock().unwrap().push(Received {
                    path: request_line.split(' ').nth(1).unwrap_or("").to_owned(),
                    headers,
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                let (status, reply) = &responses[i % responses.len()];
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .ok();
            }
        });
        TestServer { url, received }
    }

    pub fn count(&self) -> usize {
        self.received.lock().unwrap().len()
    }
}
