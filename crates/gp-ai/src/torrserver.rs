//! A client for TorrServer, which turns a torrent into an HTTP stream that the player and
//! ffmpeg read like any other web video, seeking included.

use crate::torznab::{hash_of_magnet, percent_encode};
use crate::{AiError, http};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// How long a new torrent may take to tell which files it has.
pub const FILES_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentFile {
    /// TorrServer's file id, counted from 1.
    pub index: u32,
    pub path: String,
    pub size: u64,
}

impl TorrentFile {
    pub fn name(&self) -> &str {
        self.path.rsplit(['/', '\\']).next().unwrap_or(&self.path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Torrent {
    pub hash: String,
    pub name: String,
    pub files: Vec<TorrentFile>,
    /// The server had this torrent before I added it, so it is not mine to remove.
    pub was_there: bool,
}

/// What to add: a link the server fetches itself, or a `.torrent` file uploaded to it.
pub enum Source<'a> {
    Link(&'a str),
    File(&'a [u8]),
}

/// The configured address with no trailing slash.
pub fn base(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}

pub fn version(base: &str) -> Result<String, AiError> {
    let text = http::get(&format!("{base}/echo"))?;
    Ok(text.trim().to_owned())
}

fn torrents(base: &str, body: &Value) -> Result<String, AiError> {
    http::post_json_text(&format!("{base}/torrents"), body)
}

fn json_of(text: &str) -> Result<Value, AiError> {
    serde_json::from_str(text).map_err(|e| {
        AiError::BadResponse(format!(
            "{e}: {}",
            text.chars().take(200).collect::<String>()
        ))
    })
}

/// Info hashes of the torrents the server holds now.
pub fn list(base: &str) -> Result<Vec<String>, AiError> {
    let v = json_of(&torrents(base, &json!({"action": "list"}))?)?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["hash"].as_str().map(str::to_ascii_lowercase))
                .collect()
        })
        .unwrap_or_default())
}

/// Adds a magnet link, info hash or `.torrent` URL without keeping it in the server's
/// database; returns its info hash.
pub fn add(base: &str, link: &str) -> Result<String, AiError> {
    let v = json_of(&torrents(
        base,
        &json!({"action": "add", "link": link, "save_to_db": false}),
    )?)?;
    hash_in(&v)
}

/// Hands a `.torrent` file over; returns its info hash.
pub fn upload(base: &str, torrent: &[u8]) -> Result<String, AiError> {
    let v = http::post_file(
        &format!("{base}/torrent/upload"),
        "file",
        "upload.torrent",
        torrent,
        &[],
    )?;
    // One status per uploaded file, or just the one.
    hash_in(v.as_array().and_then(|a| a.first()).unwrap_or(&v))
}

fn hash_in(v: &Value) -> Result<String, AiError> {
    v["hash"]
        .as_str()
        .filter(|h| !h.is_empty())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| AiError::BadResponse("the server did not say which torrent it added".into()))
}

fn get(base: &str, hash: &str) -> Result<Value, AiError> {
    json_of(&torrents(base, &json!({"action": "get", "hash": hash}))?)
}

/// The files of a torrent the server already holds; nothing is added.
pub fn info(base: &str, hash: &str) -> Result<Vec<TorrentFile>, AiError> {
    Ok(files_of(&get(base, hash)?))
}

pub fn remove(base: &str, hash: &str) -> Result<(), AiError> {
    torrents(base, &json!({"action": "rem", "hash": hash})).map(|_| ())
}

/// Adds the torrent and waits until it knows its files; see `files_with`.
pub fn files(base: &str, link: &str) -> Result<Torrent, AiError> {
    files_with(base, Source::Link(link), FILES_TIMEOUT, &|| false)
}

/// Adds the torrent and waits up to `timeout` for its list of files, which comes only once
/// peers have sent the metadata. A torrent added here and given up on is removed again.
/// `cancelled` is asked between polls.
pub fn files_with(
    base: &str,
    source: Source,
    timeout: Duration,
    cancelled: &dyn Fn() -> bool,
) -> Result<Torrent, AiError> {
    let before = list(base)?;
    let hash = match source {
        Source::Link(link) => add(base, link)?,
        Source::File(bytes) => upload(base, bytes)?,
    };
    let was_there = before.contains(&hash);
    let give_up = |e: AiError| {
        if !was_there {
            let _ = remove(base, &hash);
        }
        Err(e)
    };
    let started = Instant::now();
    loop {
        let v = match get(base, &hash) {
            Ok(v) => v,
            Err(e) => return give_up(e),
        };
        let files = files_of(&v);
        if !files.is_empty() {
            let name = ["title", "name"]
                .iter()
                .filter_map(|k| v[*k].as_str())
                .find(|s| !s.trim().is_empty())
                .unwrap_or("")
                .to_owned();
            return Ok(Torrent {
                hash,
                name,
                files,
                was_there,
            });
        }
        if cancelled() {
            return give_up(AiError::Rejected("cancelled".into()));
        }
        if started.elapsed() >= timeout {
            return give_up(AiError::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn files_of(v: &Value) -> Vec<TorrentFile> {
    v["file_stats"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| {
                    Some(TorrentFile {
                        index: u32::try_from(f["id"].as_u64()?).ok()?,
                        path: f["path"].as_str()?.to_owned(),
                        size: f["length"].as_u64().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The URL that plays file `index` of the torrent. `name` only makes the URL readable and
/// carries the file extension; the server ignores it.
pub fn stream_url(base: &str, link: &str, index: u32, name: &str) -> String {
    let name = if name.is_empty() { "video" } else { name };
    format!(
        "{base}/stream/{}?link={}&index={index}&play",
        percent_encode(name),
        percent_encode(link)
    )
}

/// The torrent (info hash) and file of a stream URL of the server at `base`; `None` for any
/// other URL.
pub fn parse_stream_url(base: &str, url: &str) -> Option<(String, u32)> {
    let base = self::base(base);
    if base.is_empty() {
        return None;
    }
    let rest = url.strip_prefix(&base)?;
    let (path, query) = rest.split_once('?')?;
    if !path.starts_with("/stream") {
        return None;
    }
    let param = |name: &str| {
        query
            .split('&')
            .find_map(|p| p.strip_prefix(name)?.strip_prefix('='))
    };
    let link = percent_decode(param("link")?);
    let hash = if link.len() == 40 && link.bytes().all(|b| b.is_ascii_hexdigit()) {
        link.to_ascii_lowercase()
    } else {
        hash_of_magnet(&link)?
    };
    let index = param("index")?.parse().ok()?;
    Some((hash, index))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::TestServer;

    const HASH: &str = "08ada5a7a6183aae1e09d831df6748d566095a10";

    fn file_stats() -> String {
        format!(
            r#"{{"hash":"{HASH}","name":"Sintel","stat":3,"file_stats":[
                {{"id":1,"path":"Sintel/Sintel.en.srt","length":1514}},
                {{"id":2,"path":"Sintel/Sintel.mp4","length":129241752}}]}}"#
        )
    }

    #[test]
    fn version_comes_from_echo() {
        let srv = TestServer::start(vec![(200, "MatriX.147\n".into())]);
        assert_eq!(version(&srv.url).unwrap(), "MatriX.147");
        assert_eq!(srv.received.lock().unwrap()[0].path, "/echo");
    }

    #[test]
    fn files_add_without_saving_and_wait_for_the_list() {
        let srv = TestServer::start(vec![
            (200, "[]".into()),
            (200, format!(r#"{{"hash":"{HASH}","stat":0}}"#)),
            (200, format!(r#"{{"hash":"{HASH}","stat":1}}"#)),
            (200, file_stats()),
        ]);
        let t = files(&srv.url, "magnet:?xt=urn:btih:08ADA5A7").unwrap();
        assert_eq!(t.hash, HASH);
        assert_eq!(t.name, "Sintel");
        assert!(!t.was_there);
        assert_eq!(
            t.files,
            vec![
                TorrentFile {
                    index: 1,
                    path: "Sintel/Sintel.en.srt".into(),
                    size: 1514
                },
                TorrentFile {
                    index: 2,
                    path: "Sintel/Sintel.mp4".into(),
                    size: 129_241_752
                },
            ]
        );
        assert_eq!(t.files[1].name(), "Sintel.mp4");
        let got = srv.received.lock().unwrap();
        assert!(got.iter().all(|r| r.path == "/torrents"));
        let add: Value = serde_json::from_str(&got[1].body).unwrap();
        assert_eq!(add["action"], "add");
        assert_eq!(add["save_to_db"], false);
        assert_eq!(add["link"], "magnet:?xt=urn:btih:08ADA5A7");
        let get: Value = serde_json::from_str(&got[2].body).unwrap();
        assert_eq!(
            (get["action"].as_str(), get["hash"].as_str()),
            (Some("get"), Some(HASH))
        );
    }

    #[test]
    fn a_torrent_already_there_is_marked() {
        let srv = TestServer::start(vec![
            (200, format!(r#"[{{"hash":"{}"}}]"#, HASH.to_uppercase())),
            (200, format!(r#"{{"hash":"{HASH}"}}"#)),
            (200, file_stats()),
        ]);
        assert!(files(&srv.url, HASH).unwrap().was_there);
    }

    #[test]
    fn a_torrent_without_metadata_times_out_and_is_removed() {
        let srv = TestServer::start(vec![
            (200, "[]".into()),
            (200, format!(r#"{{"hash":"{HASH}","stat":1}}"#)),
        ]);
        let e = files_with(
            &srv.url,
            Source::Link(HASH),
            Duration::from_millis(500),
            &|| false,
        )
        .unwrap_err();
        assert_eq!(e, AiError::TimedOut);
        let got = srv.received.lock().unwrap();
        let last: Value = serde_json::from_str(&got.last().unwrap().body).unwrap();
        assert_eq!(
            (last["action"].as_str(), last["hash"].as_str()),
            (Some("rem"), Some(HASH))
        );
    }

    #[test]
    fn an_uploaded_file_gives_its_hash() {
        let srv = TestServer::start(vec![
            (200, "[]".into()),
            (200, format!(r#"[{{"hash":"{HASH}"}}]"#)),
            (200, file_stats()),
        ]);
        let t = files_with(
            &srv.url,
            Source::File(b"d4:infod4:name1:xee"),
            FILES_TIMEOUT,
            &|| false,
        )
        .unwrap();
        assert_eq!(t.hash, HASH);
        let got = srv.received.lock().unwrap();
        assert_eq!(got[1].path, "/torrent/upload");
        assert!(
            got[1]
                .headers
                .to_ascii_lowercase()
                .contains("multipart/form-data; boundary=")
        );
        assert!(
            got[1].body.contains("name=\"file\"") && got[1].body.contains("d4:infod4:name1:xee")
        );
    }

    #[test]
    fn an_unreachable_server_is_a_network_error() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let base = format!("http://127.0.0.1:{port}");
        assert!(matches!(version(&base), Err(AiError::Network(_))));
        assert!(matches!(files(&base, HASH), Err(AiError::Network(_))));
    }

    #[test]
    fn remove_sends_rem() {
        let srv = TestServer::start(vec![(200, "".into())]);
        remove(&srv.url, HASH).unwrap();
        let body: Value = serde_json::from_str(&srv.received.lock().unwrap()[0].body).unwrap();
        assert_eq!(body, json!({"action": "rem", "hash": HASH}));
    }

    #[test]
    fn stream_urls_round_trip() {
        let base = "http://192.168.1.5:8090";
        let magnet = format!("magnet:?xt=urn:btih:{}&dn=Sintel Film", HASH.to_uppercase());
        let url = stream_url(base, &magnet, 2, "Sintel (2010).mp4");
        assert!(
            url.starts_with(
                "http://192.168.1.5:8090/stream/Sintel%20%282010%29.mp4?link=magnet%3A%3Fxt%3D"
            ),
            "{url}"
        );
        assert!(url.ends_with("&index=2&play"), "{url}");
        assert_eq!(
            parse_stream_url(&format!("{base}/"), &url),
            Some((HASH.into(), 2))
        );
        let by_hash = stream_url(base, HASH, 7, "");
        assert_eq!(parse_stream_url(base, &by_hash), Some((HASH.into(), 7)));
        assert_eq!(parse_stream_url("http://other:8090", &url), None);
        assert_eq!(parse_stream_url("", &url), None);
        assert_eq!(parse_stream_url(base, "http://192.168.1.5:8090/echo"), None);
    }
}
