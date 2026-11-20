use xxhash_rust::xxh3::Xxh3;

pub const SAMPLE_LEN: u64 = 64 * 1024;

/// Survives renaming and moving the file, and I do not have to hash gigabytes.
pub fn file_key(size: u64, head: &[u8], tail: &[u8]) -> String {
    let mut h = Xxh3::new();
    h.update(&size.to_le_bytes());
    h.update(head);
    h.update(tail);
    format!("f:{:016x}", h.digest())
}

pub fn url_key(url: &str) -> String {
    format!("u:{url}")
}

/// A file of a torrent played through a streaming server: the same file whatever server
/// address or file name the stream URL carries.
pub fn torrent_key(info_hash: &str, file_index: u32) -> String {
    format!("t:{}:{file_index}", info_hash.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_depends_on_content_and_size() {
        let a = file_key(10, b"head", b"tail");
        assert_eq!(a, file_key(10, b"head", b"tail"));
        assert_ne!(a, file_key(11, b"head", b"tail"));
        assert_ne!(a, file_key(10, b"heaD", b"tail"));
        assert!(a.starts_with("f:") && a.len() == 18);
    }

    #[test]
    fn torrent_key_ignores_hash_case() {
        assert_eq!(torrent_key("ABCDEF", 3), torrent_key("abcdef", 3));
        assert_ne!(torrent_key("abcdef", 3), torrent_key("abcdef", 4));
    }
}
