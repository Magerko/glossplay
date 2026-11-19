//! A throwaway HTTP server for tests: answers every request with the next canned body
//! and records what it received.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Read, Write};
use std::marker::PhantomData;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

pub struct Received {
    pub path: String,
    pub headers: String,
    pub body: String,
}

// On Windows, connections made in parallel to freshly created local listeners now and then
// hang. The tests take hundredths of a second, so I run all of them that touch the network
// one at a time.
static NET_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    // Nesting depth on this thread, and the lock held while it is above zero. A test may
    // start two servers, so the lock has to be re-entrant for the thread that owns it.
    static HELD: RefCell<(usize, Option<MutexGuard<'static, ()>>)> = const { RefCell::new((0, None)) };
}

/// Keeps other tests off the network while it lives. The lock is released when the last
/// guard of the thread goes away.
pub struct NetGuard(PhantomData<*const ()>);

pub fn serial() -> NetGuard {
    HELD.with_borrow_mut(|(depth, guard)| {
        if *depth == 0 {
            // A test that failed while holding the lock must not fail all the others.
            *guard = Some(NET_LOCK.lock().unwrap_or_else(PoisonError::into_inner));
        }
        *depth += 1;
    });
    NetGuard(PhantomData)
}

impl Drop for NetGuard {
    fn drop(&mut self) {
        HELD.with_borrow_mut(|(depth, guard)| {
            *depth -= 1;
            if *depth == 0 {
                *guard = None;
            }
        });
    }
}

pub struct TestServer {
    pub url: String,
    pub received: Arc<Mutex<Vec<Received>>>,
    _net: NetGuard,
}

impl TestServer {
    /// Replies cycle through `responses`; each is (status, body).
    pub fn start(responses: Vec<(u16, String)>) -> TestServer {
        Self::start_with_headers(responses, String::new())
    }

    /// `extra_headers` is raw header text, each line ending in CRLF, added to every reply.
    pub fn start_with_headers(responses: Vec<(u16, String)>, extra_headers: String) -> TestServer {
        let net = serial();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        let received = Arc::new(Mutex::new(Vec::new()));
        let log = received.clone();
        let responses = Arc::new(responses);
        let extra_headers = Arc::new(extra_headers);
        std::thread::spawn(move || {
            // One thread per connection: a client that connects and stays silent must not
            // block the requests that follow it.
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let (log, responses, extra) =
                    (log.clone(), responses.clone(), extra_headers.clone());
                std::thread::spawn(move || {
                    let _ = serve(stream, &log, &responses, &extra);
                });
            }
        });
        TestServer {
            url,
            received,
            _net: net,
        }
    }

    pub fn count(&self) -> usize {
        self.received.lock().unwrap().len()
    }

    /// Serves `body` to every GET, answering `Range: bytes=N-` with 206 when `ranges` is on.
    /// `cut_first_at` makes the first reply stop after that many body bytes, like a dropped
    /// connection.
    pub fn start_ranged(body: Vec<u8>, ranges: bool, cut_first_at: Option<usize>) -> TestServer {
        let net = serial();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let received = Arc::new(Mutex::new(Vec::new()));
        let log = received.clone();
        let body = Arc::new(body);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let (log, body) = (log.clone(), body.clone());
                std::thread::spawn(move || {
                    let _ = serve_ranged(stream, &log, &body, ranges, cut_first_at);
                });
            }
        });
        TestServer {
            url,
            received,
            _net: net,
        }
    }
}

fn serve_ranged(
    mut stream: TcpStream,
    log: &Mutex<Vec<Received>>,
    body: &[u8],
    ranges: bool,
    cut_first_at: Option<usize>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let (mut headers, mut from) = (String::new(), None);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("range: bytes=") {
            from = v.trim().trim_end_matches('-').parse::<usize>().ok();
        }
        headers.push_str(&line);
    }
    let first = {
        let mut log = log.lock().unwrap();
        log.push(Received {
            path: request_line.split(' ').nth(1).unwrap_or("").to_owned(),
            headers,
            body: String::new(),
        });
        log.len() == 1
    };
    let len = body.len();
    let (head, slice) = match from.filter(|_| ranges) {
        Some(n) if n >= len => (
            format!(
                "HTTP/1.1 416 X\r\nContent-Range: bytes */{len}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
            &body[..0],
        ),
        Some(n) => (
            format!(
                "HTTP/1.1 206 X\r\nContent-Range: bytes {n}-{}/{len}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                len - 1,
                len - n
            ),
            &body[n..],
        ),
        None => (
            format!("HTTP/1.1 200 X\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"),
            body,
        ),
    };
    let slice = match cut_first_at {
        Some(cut) if first => &slice[..cut.min(slice.len())],
        _ => slice,
    };
    let mut reply = head.into_bytes();
    reply.extend_from_slice(slice);
    stream.write_all(&reply)?;
    stream.flush()?;
    stream.shutdown(Shutdown::Write)?;
    let _ = reader.read(&mut [0; 64]);
    Ok(())
}

fn serve(
    mut stream: TcpStream,
    log: &Mutex<Vec<Received>>,
    responses: &[(u16, String)],
    extra_headers: &str,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut headers = String::new();
    let mut content_length = 0;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
        headers.push_str(&line);
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size_line = String::new();
            reader.read_line(&mut size_line)?;
            let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or(""), 16)
                .unwrap_or(0);
            if size == 0 {
                loop {
                    let mut trailer = String::new();
                    if reader.read_line(&mut trailer)? == 0 || trailer.trim().is_empty() {
                        break;
                    }
                }
                break;
            }
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk)?;
            body.extend_from_slice(&chunk[..size]);
        }
    } else {
        body.resize(content_length, 0);
        reader.read_exact(&mut body)?;
    }
    // Index and push share one lock so numbering follows handled requests even when
    // connections are served concurrently.
    let (status, reply) = {
        let mut log = log.lock().unwrap();
        let i = log.len();
        log.push(Received {
            path: request_line.split(' ').nth(1).unwrap_or("").to_owned(),
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
        &responses[i % responses.len()]
    };
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
    // One write: several small ones can stall on Nagle plus delayed ACK for 40 ms each.
    stream.write_all(head.as_bytes())?;
    stream.flush()?;
    // Closing a socket that still holds unread input makes Windows send RST, which can
    // discard the reply; so I half-close and wait for the client to close its side.
    stream.shutdown(Shutdown::Write)?;
    let _ = reader.read(&mut [0; 64]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_chunked_bodies_and_closes_cleanly() {
        let srv = TestServer::start(vec![(200, "ok".into())]);
        let mut s = TcpStream::connect(srv.url.trim_start_matches("http://")).unwrap();
        s.write_all(
            b"POST /p HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
        )
        .unwrap();
        let mut reply = String::new();
        s.read_to_string(&mut reply).unwrap();
        assert!(reply.ends_with("\r\n\r\nok"), "{reply}");
        assert_eq!(srv.received.lock().unwrap()[0].body, "abcde");
    }
}
