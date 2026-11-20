//! A client for Torznab, the search protocol of Jackett, Prowlarr and similar servers. The
//! user brings the server; Glossplay knows no sources of its own.

use crate::{AiError, http};

/// Overrides the key written in the config, like the keys of the translation services.
pub const ENV_KEY: &str = "GLOSSPLAY_TORZNAB_KEY";

/// The aggregate endpoint of Jackett, assumed when the address has no path at all.
const JACKETT_ALL: &str = "/api/v2.0/indexers/all/results/torznab/api";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Magnet(String),
    /// A `.torrent` file, or a proxy of the search server that redirects to a magnet link.
    TorrentUrl(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub title: String,
    pub size: Option<u64>,
    pub seeders: Option<u32>,
    pub peers: Option<u32>,
    /// `YYYY-MM-DD` when the date could be read, the server's text otherwise.
    pub published: Option<String>,
    pub indexer: Option<String>,
    /// Lowercase hex, when the server gave it or the magnet link carries it.
    pub info_hash: Option<String>,
    pub link: Link,
}

/// What a link turned out to be once its proxy was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Magnet(String),
    /// The `.torrent` file itself: the streaming server may not reach the search server's
    /// address (`127.0.0.1` of another machine), so the file is handed over as it is.
    Torrent(Vec<u8>),
    Url(String),
}

pub fn resolve_key(config_key: &str, env: &dyn Fn(&str) -> Option<String>) -> String {
    env(ENV_KEY)
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| config_key.trim().to_owned())
}

/// The endpoint as the user wrote it, made ready for requests: search parameters copied
/// along with it are dropped, and a key found in it is returned separately.
pub fn endpoint(raw: &str) -> (String, Option<String>) {
    let raw = raw.trim();
    let (base, query) = raw.split_once('?').unwrap_or((raw, ""));
    let (mut key, mut keep) = (None, Vec::new());
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        match name.to_ascii_lowercase().as_str() {
            "apikey" => key = Some(percent_decode(value)).filter(|k| !k.is_empty()),
            "t" | "q" | "limit" | "offset" | "cat" | "extended" | "attrs" => {}
            _ => keep.push(pair),
        }
    }
    let mut base = base.trim_end_matches('/').to_owned();
    let after_scheme = base.split_once("://").map_or(base.as_str(), |(_, r)| r);
    if !after_scheme.contains('/') {
        base.push_str(JACKETT_ALL);
    }
    if !keep.is_empty() {
        base = format!("{base}?{}", keep.join("&"));
    }
    (base, key)
}

/// `url` without the values of its key parameters, fit for messages and logs.
pub fn redact(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_owned();
    };
    let pairs: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, _)) if is_secret(name) => format!("{name}=***"),
            _ => pair.to_owned(),
        })
        .collect();
    format!("{base}?{}", pairs.join("&"))
}

fn is_secret(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("apikey") || name.contains("passkey") || name == "key" || name == "token"
}

/// Whatever went wrong, the key must not reach a message: some errors quote the URL.
fn scrub(e: AiError, key: &str) -> AiError {
    let clean = |s: String| {
        let s = if key.len() >= 4 {
            s.replace(key, "***")
        } else {
            s
        };
        s.split(' ').map(redact).collect::<Vec<_>>().join(" ")
    };
    match e {
        AiError::Network(s) => AiError::Network(clean(s)),
        AiError::BadResponse(s) => AiError::BadResponse(clean(s)),
        AiError::Rejected(s) => AiError::Rejected(clean(s)),
        AiError::Http { status, body } => AiError::Http {
            status,
            body: clean(body),
        },
        other => other,
    }
}

fn request(endpoint: &str, key: &str, params: &[(&str, &str)]) -> Result<String, AiError> {
    let mut query = params.to_vec();
    if !key.is_empty() {
        query.push(("apikey", key));
    }
    let run = || {
        let (status, body) = http::get_with_query(endpoint, &query)?;
        if let Some(e) = server_error(&body) {
            return Err(e);
        }
        match status {
            200..300 => Ok(body),
            401 | 403 => Err(AiError::BadKey),
            _ => Err(AiError::Http {
                status,
                body: body.chars().take(300).collect(),
            }),
        }
    };
    run().map_err(|e| scrub(e, key))
}

/// Searches every source behind `endpoint`; the best seeded come first.
pub fn search(
    endpoint: &str,
    apikey: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<Release>, AiError> {
    // No `limit` goes to the server: it would cut the list before I sort it by seeders.
    let body = request(endpoint, apikey, &[("t", "search"), ("q", query)])?;
    let mut found = parse_search(&body)?;
    found.truncate(limit);
    Ok(found)
}

/// Asks the server what it can do; returns how many sources stand behind the endpoint.
pub fn check(endpoint: &str, apikey: &str) -> Result<usize, AiError> {
    let caps = request(endpoint, apikey, &[("t", "caps")])?;
    let doc = parse_xml(&caps)?;
    if doc.root_element().tag_name().name() != "caps" {
        return Err(AiError::BadResponse("not a Torznab answer".into()));
    }
    // Jackett's aggregate endpoint lists what it searches; any other is one source.
    let listed = request(
        endpoint,
        apikey,
        &[("t", "indexers"), ("configured", "true")],
    )
    .ok()
    .and_then(|xml| {
        let doc = parse_xml(&xml).ok()?;
        let n = doc
            .descendants()
            .filter(|n| n.has_tag_name("indexer"))
            .count();
        (n > 0).then_some(n)
    });
    Ok(listed.unwrap_or(1))
}

/// Finds out what a link really points at. A proxy link of the search server is asked once,
/// without following its redirect, since that usually leads to a `magnet:` address.
pub fn resolve(link: &Link) -> Result<Resolved, AiError> {
    let url = match link {
        Link::Magnet(m) => return Ok(Resolved::Magnet(m.clone())),
        Link::TorrentUrl(u) => u.clone(),
    };
    let mut current = url.clone();
    for _ in 0..4 {
        let reply = http::get_single(&current).map_err(|e| scrub(e, ""))?;
        match (reply.status, reply.location) {
            (300..400, Some(next)) if next.starts_with("magnet:") => {
                return Ok(Resolved::Magnet(next));
            }
            (300..400, Some(next))
                if next.starts_with("http://") || next.starts_with("https://") =>
            {
                current = next;
            }
            (200..300, _) if reply.body.first() == Some(&b'd') => {
                return Ok(Resolved::Torrent(reply.body));
            }
            (status @ (400..), _) => {
                return Err(AiError::Http {
                    status,
                    body: String::new(),
                });
            }
            _ => break,
        }
    }
    Ok(Resolved::Url(url))
}

fn parse_xml(text: &str) -> Result<roxmltree::Document<'_>, AiError> {
    roxmltree::Document::parse(text)
        .map_err(|e| AiError::BadResponse(format!("not a Torznab answer ({e})")))
}

/// `<error code="100" description="Invalid API Key"/>`, the way Torznab reports failures.
fn server_error(body: &str) -> Option<AiError> {
    let start = body.trim_start();
    let start = start.strip_prefix('\u{feff}').unwrap_or(start);
    if !start.starts_with('<') || !body.contains("<error") {
        return None;
    }
    let doc = roxmltree::Document::parse(body).ok()?;
    let root = doc.root_element();
    if root.tag_name().name() != "error" {
        return None;
    }
    let code: u32 = root.attribute("code").and_then(|c| c.parse().ok())?;
    let text = root
        .attribute("description")
        .unwrap_or("")
        .trim()
        .to_owned();
    Some(match code {
        100 => AiError::BadKey,
        _ if text.is_empty() => AiError::Rejected(format!("error {code}")),
        _ => AiError::Rejected(format!("{text} ({code})")),
    })
}

pub fn parse_search(xml: &str) -> Result<Vec<Release>, AiError> {
    // A feed that uses the torznab prefix without declaring it is still worth reading.
    let undeclared;
    let doc = match roxmltree::Document::parse(xml) {
        Ok(doc) => doc,
        Err(roxmltree::Error::UnknownNamespace(..)) => {
            undeclared = xml
                .replace("<torznab:attr", "<attr")
                .replace("<newznab:attr", "<attr");
            parse_xml(&undeclared)?
        }
        Err(e) => return Err(AiError::BadResponse(format!("not a Torznab answer ({e})"))),
    };
    if doc.root_element().tag_name().name() != "rss" {
        return Err(AiError::BadResponse("not a Torznab answer".into()));
    }
    let mut found: Vec<Release> = doc
        .descendants()
        .filter(|n| n.has_tag_name("item"))
        .filter_map(read_item)
        .collect();
    // Several sources often list the same torrent; the best seeded copy stays.
    found.sort_by_key(|r| std::cmp::Reverse(r.seeders.unwrap_or(0)));
    let mut seen = std::collections::HashSet::new();
    found.retain(|r| r.info_hash.as_ref().is_none_or(|h| seen.insert(h.clone())));
    Ok(found)
}

fn read_item(item: roxmltree::Node) -> Option<Release> {
    let child = |name: &str| {
        item.children()
            .find(|n| n.is_element() && n.tag_name().name() == name)
    };
    let text = |name: &str| {
        child(name)
            .and_then(|n| n.text())
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
    };
    let attr = |name: &str| {
        item.children()
            .filter(|n| n.is_element() && n.tag_name().name() == "attr")
            .find(|n| n.attribute("name") == Some(name))
            .and_then(|n| n.attribute("value"))
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let title = text("title")?;
    let enclosure = child("enclosure").and_then(|n| n.attribute("url").map(str::to_owned));
    let link_text = text("link");
    let candidates = [link_text.clone(), enclosure.clone()];
    let magnet = attr("magneturl")
        .filter(|m| m.starts_with("magnet:"))
        .or_else(|| {
            candidates
                .iter()
                .flatten()
                .find(|u| u.starts_with("magnet:"))
                .cloned()
        });
    let info_hash = attr("infohash")
        .filter(|h| is_hash(h))
        .or_else(|| magnet.as_deref().and_then(hash_of_magnet))
        .map(|h| h.to_ascii_lowercase());
    let web = candidates
        .iter()
        .flatten()
        .find(|u| u.starts_with("http://") || u.starts_with("https://"))
        .cloned();
    let link = match (magnet, &info_hash, web) {
        (Some(m), _, _) => Link::Magnet(m),
        (None, Some(h), _) => Link::Magnet(magnet_of(h, &title)),
        (None, None, Some(u)) => Link::TorrentUrl(u),
        (None, None, None) => return None,
    };
    let number = |s: Option<String>| s.and_then(|v| v.parse::<u64>().ok());
    let size = number(text("size"))
        .or_else(|| number(attr("size")))
        .or_else(|| {
            number(child("enclosure").and_then(|n| n.attribute("length").map(str::to_owned)))
        })
        .filter(|&s| s > 0);
    let count = |name: &str| attr(name).and_then(|v| v.parse::<u32>().ok());
    Some(Release {
        size,
        seeders: count("seeders"),
        peers: count("peers").or_else(|| {
            // `leechers` is the other half of the swarm when `peers` is missing.
            Some(count("seeders")? + count("leechers")?)
        }),
        published: text("pubDate").map(|d| iso_date(&d).unwrap_or(d)),
        indexer: text("jackettindexer").or_else(|| text("prowlarrindexer")),
        info_hash,
        link,
        title,
    })
}

fn is_hash(h: &str) -> bool {
    h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The info hash of `magnet:?xt=urn:btih:<hex>&…`; base32 hashes are not converted.
pub fn hash_of_magnet(magnet: &str) -> Option<String> {
    let lower = magnet.to_ascii_lowercase();
    let at = lower.find("urn:btih:")? + "urn:btih:".len();
    let hash: String = lower[at..]
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    is_hash(&hash).then_some(hash)
}

fn magnet_of(hash: &str, title: &str) -> String {
    format!("magnet:?xt=urn:btih:{hash}&dn={}", percent_encode(title))
}

/// "Sat, 11 Apr 2015 21:34:00 -0600" → "2015-04-11".
fn iso_date(rfc2822: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let words: Vec<&str> = rfc2822
        .split([' ', ','])
        .filter(|w| !w.is_empty())
        .collect();
    let at = words.iter().position(|w| w.parse::<u32>().is_ok())?;
    let day: u32 = words[at].parse().ok()?;
    let month_word = words.get(at + 1)?.to_ascii_lowercase();
    let month = MONTHS.iter().position(|m| month_word.starts_with(m))? + 1;
    let year: u32 = words.get(at + 2)?.parse().ok()?;
    ((1..=31).contains(&day) && year > 1900).then(|| format!("{year:04}-{month:02}-{day:02}"))
}

pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
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
            (b'+', _) => {
                out.push(b' ');
                i += 1;
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

    const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="1.0" xmlns:atom="http://www.w3.org/2005/Atom" xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <title>Example Indexer</title>
    <item>
      <title>Sintel 2010 720p &amp; Extras</title>
      <guid>https://indexer.test/t/1</guid>
      <jackettindexer id="example">Example Indexer</jackettindexer>
      <pubDate>Sat, 11 Apr 2015 21:34:00 -0600</pubDate>
      <size>388895872</size>
      <link>magnet:?xt=urn:btih:9fb267cff5ae5603f07a347676ec3bf3e35f75e1&amp;dn=Sintel</link>
      <torznab:attr name="seeders" value="12" />
      <torznab:attr name="peers" value="15" />
    </item>
    <item>
      <title>Big Buck Bunny 1080p</title>
      <link>https://indexer.test/download.php?torrent=11515&amp;passkey=123456</link>
      <size>2538463390</size>
      <enclosure url="https://indexer.test/download.php?torrent=11515&amp;passkey=123456" length="2538463390" type="application/x-bittorrent" />
      <torznab:attr name="category" value="2000" />
      <torznab:attr name="seeders" value="40" />
      <torznab:attr name="peers" value="44" />
      <torznab:attr name="infohash" value="63E07FF523710CA268567DAD344CE1E0E6B7E8A3" />
    </item>
    <item>
      <title>Elephants Dream</title>
      <link>http://127.0.0.1:9117/dl/example/?jackett_apikey=secret&amp;path=abc</link>
    </item>
    <item>
      <title>Sintel 2010 720p copy</title>
      <link>http://127.0.0.1:9117/dl/other/?path=x</link>
      <torznab:attr name="magneturl" value="magnet:?xt=urn:btih:9FB267CFF5AE5603F07A347676EC3BF3E35F75E1&amp;dn=Sintel&amp;tr=udp%3A%2F%2Ftracker.test%3A1337" />
      <torznab:attr name="seeders" value="3" />
    </item>
  </channel>
</rss>"#;

    #[test]
    fn a_feed_becomes_releases_sorted_by_seeders() {
        let found = parse_search(FEED).unwrap();
        let titles: Vec<&str> = found.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Big Buck Bunny 1080p",
                "Sintel 2010 720p & Extras",
                "Elephants Dream"
            ],
            "the less seeded copy of the same torrent is gone"
        );
        let bunny = &found[0];
        assert_eq!(bunny.size, Some(2_538_463_390));
        assert_eq!((bunny.seeders, bunny.peers), (Some(40), Some(44)));
        assert_eq!(
            bunny.info_hash.as_deref(),
            Some("63e07ff523710ca268567dad344ce1e0e6b7e8a3")
        );
        assert!(
            matches!(&bunny.link, Link::Magnet(m) if m.starts_with("magnet:?xt=urn:btih:63e07ff5") && m.contains("dn=Big%20Buck%20Bunny")),
            "an info hash beats an http link: {:?}",
            bunny.link
        );
        let sintel = &found[1];
        assert_eq!(sintel.published.as_deref(), Some("2015-04-11"));
        assert_eq!(sintel.indexer.as_deref(), Some("Example Indexer"));
        assert!(matches!(&sintel.link, Link::Magnet(m) if m.ends_with("dn=Sintel")));
        let dream = &found[2];
        assert_eq!(
            (
                dream.size,
                dream.seeders,
                dream.peers,
                &dream.published,
                &dream.indexer
            ),
            (None, None, None, &None, &None),
            "missing attributes stay empty"
        );
        assert!(matches!(&dream.link, Link::TorrentUrl(u) if u.contains("/dl/example/")));
    }

    #[test]
    fn magneturl_comes_first() {
        let xml = FEED.replace(
            r#"<torznab:attr name="seeders" value="12" />"#,
            r#"<torznab:attr name="seeders" value="1" />"#,
        );
        let found = parse_search(&xml).unwrap();
        let sintel = found
            .iter()
            .find(|r| r.title.starts_with("Sintel"))
            .unwrap();
        assert!(
            matches!(&sintel.link, Link::Magnet(m) if m.contains("tr=udp")),
            "{:?}",
            sintel.link
        );
    }

    #[test]
    fn an_undeclared_prefix_is_tolerated() {
        let xml = r#"<rss><channel><item><title>Sintel</title>
            <torznab:attr name="infohash" value="08ada5a7a6183aae1e09d831df6748d566095a10"/>
            <torznab:attr name="seeders" value="7"/></item></channel></rss>"#;
        let found = parse_search(xml).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].seeders, Some(7));
    }

    #[test]
    fn an_empty_feed_is_an_empty_list() {
        let srv = TestServer::start(vec![(
            200,
            r#"<?xml version="1.0"?><rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><title>x</title></channel></rss>"#.into(),
        )]);
        let found = search(&format!("{}/api", srv.url), "key1", "nothing here", 50).unwrap();
        assert!(found.is_empty());
        let path = &srv.received.lock().unwrap()[0].path;
        assert!(path.starts_with("/api?"), "{path}");
        assert!(path.contains("t=search"), "{path}");
        assert!(
            path.contains("q=nothing%20here") || path.contains("q=nothing+here"),
            "{path}"
        );
        assert!(path.contains("apikey=key1"), "{path}");
    }

    #[test]
    fn search_goes_to_the_server_and_cuts_the_list() {
        let srv = TestServer::start(vec![(200, FEED.into())]);
        let found = search(&format!("{}/api", srv.url), "", "sintel", 2).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].title, "Big Buck Bunny 1080p");
        let path = &srv.received.lock().unwrap()[0].path;
        assert!(!path.contains("apikey"), "no key, no parameter: {path}");
    }

    #[test]
    fn errors_are_explained_and_keep_the_key_out() {
        let srv = TestServer::start(vec![(
            200,
            r#"<?xml version="1.0" encoding="UTF-8"?><error code="100" description="Invalid API Key" />"#.into(),
        )]);
        assert_eq!(
            search(&format!("{}/api", srv.url), "wrongkey", "x", 10),
            Err(AiError::BadKey)
        );
        let srv = TestServer::start(vec![(
            400,
            r#"<error code="203" description="Function Not Available: imdbid is not supported"/>"#
                .into(),
        )]);
        let e = search(&format!("{}/api", srv.url), "k", "x", 10).unwrap_err();
        assert!(
            matches!(&e, AiError::Rejected(t) if t.contains("Function Not Available") && t.contains("203")),
            "{e:?}"
        );
        let srv = TestServer::start(vec![(401, "".into())]);
        assert_eq!(
            check(&format!("{}/api", srv.url), "k"),
            Err(AiError::BadKey)
        );
        let srv = TestServer::start(vec![(500, "boom with secretkey123 inside".into())]);
        let e = search(&format!("{}/api", srv.url), "secretkey123", "x", 10).unwrap_err();
        assert!(!format!("{e}").contains("secretkey123"), "{e}");
        let srv = TestServer::start(vec![(200, "<html><body>Login</body></html>".into())]);
        assert!(matches!(
            search(&format!("{}/api", srv.url), "k", "x", 10),
            Err(AiError::BadResponse(_))
        ));
    }

    #[test]
    fn an_unreachable_server_is_a_network_error_without_the_key() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let e = search(
            &format!("http://127.0.0.1:{port}/api"),
            "hiddenkey",
            "x",
            10,
        )
        .unwrap_err();
        assert!(matches!(e, AiError::Network(_)), "{e:?}");
        assert!(!e.to_string().contains("hiddenkey"));
    }

    #[test]
    fn check_counts_the_sources() {
        let caps = r#"<?xml version="1.0"?><caps><server title="x"/><searching><search available="yes" supportedParams="q"/></searching></caps>"#;
        let indexers = r#"<?xml version="1.0"?><indexers><indexer id="a" configured="true"><title>A</title></indexer><indexer id="b" configured="true"><title>B</title></indexer></indexers>"#;
        let srv = TestServer::start(vec![(200, caps.into()), (200, indexers.into())]);
        assert_eq!(check(&format!("{}/api", srv.url), "k"), Ok(2));
        let srv = TestServer::start(vec![
            (200, caps.into()),
            (
                200,
                r#"<error code="202" description="No such function"/>"#.into(),
            ),
        ]);
        assert_eq!(check(&format!("{}/api", srv.url), "k"), Ok(1));
    }

    #[test]
    fn a_proxy_link_that_redirects_to_a_magnet_gives_the_magnet() {
        let magnet = "magnet:?xt=urn:btih:08ada5a7a6183aae1e09d831df6748d566095a10&dn=Sintel";
        let srv = TestServer::start_with_headers(
            vec![(302, "".into())],
            format!("Location: {magnet}\r\n"),
        );
        let link = Link::TorrentUrl(format!("{}/dl/example/?path=abc", srv.url));
        assert_eq!(resolve(&link), Ok(Resolved::Magnet(magnet.into())));
        assert_eq!(srv.count(), 1);
    }

    #[test]
    fn a_torrent_file_is_kept_and_anything_else_stays_a_url() {
        let srv = TestServer::start(vec![(200, "d8:announce3:x:ye".into())]);
        let link = Link::TorrentUrl(format!("{}/file.torrent", srv.url));
        assert_eq!(
            resolve(&link),
            Ok(Resolved::Torrent(b"d8:announce3:x:ye".to_vec()))
        );
        let srv = TestServer::start(vec![(200, "<html/>".into())]);
        let url = format!("{}/page", srv.url);
        assert_eq!(
            resolve(&Link::TorrentUrl(url.clone())),
            Ok(Resolved::Url(url))
        );
        let m = Link::Magnet("magnet:?xt=urn:btih:x".into());
        assert_eq!(
            resolve(&m),
            Ok(Resolved::Magnet("magnet:?xt=urn:btih:x".into()))
        );
    }

    #[test]
    fn the_endpoint_is_cleaned_up() {
        assert_eq!(
            endpoint("http://127.0.0.1:9117"),
            (format!("http://127.0.0.1:9117{JACKETT_ALL}"), None)
        );
        assert_eq!(
            endpoint(" http://host:9696/1/api?t=search&q=x&apikey=ab%2Bc "),
            ("http://host:9696/1/api".into(), Some("ab+c".into()))
        );
        assert_eq!(
            endpoint("http://host/api?apikey=&cat=2000&extra=1"),
            ("http://host/api?extra=1".into(), None)
        );
    }

    #[test]
    fn keys_are_redacted_and_the_environment_wins() {
        assert_eq!(
            redact("http://h/dl/x/?jackett_apikey=abc&path=p&apikey=d"),
            "http://h/dl/x/?jackett_apikey=***&path=p&apikey=***"
        );
        assert_eq!(redact("http://h/api"), "http://h/api");
        let env = |name: &str| (name == ENV_KEY).then(|| "from-env".to_owned());
        assert_eq!(resolve_key("from-config", &env), "from-env");
        assert_eq!(resolve_key(" from-config ", &|_| None), "from-config");
        assert_eq!(
            resolve_key("from-config", &|_| Some(String::new())),
            "from-config"
        );
    }

    #[test]
    fn dates_and_hashes_are_read() {
        assert_eq!(
            iso_date("Mon, 14 Jul 2025 14:37:06 +0300").as_deref(),
            Some("2025-07-14")
        );
        assert_eq!(iso_date("yesterday"), None);
        assert_eq!(
            hash_of_magnet("magnet:?xt=urn:btih:08ADA5A7A6183AAE1E09D831DF6748D566095A10&dn=x")
                .as_deref(),
            Some("08ada5a7a6183aae1e09d831df6748d566095a10")
        );
        assert_eq!(hash_of_magnet("magnet:?xt=urn:btih:SHORT"), None);
    }
}
