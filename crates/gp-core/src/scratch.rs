//! A scratch directory for tests, removed again when dropped, so a failing test leaves
//! nothing behind in the user's temp folder either.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct TempDir(PathBuf);

impl TempDir {
    /// A new empty directory in the system temp folder; `prefix` says which test made it.
    pub fn new(prefix: &str) -> TempDir {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("cannot create a temporary directory");
        TempDir(path)
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directory_goes_away_with_its_contents() {
        let kept;
        {
            let dir = TempDir::new("gp-scratch");
            std::fs::create_dir_all(dir.join("a/b")).unwrap();
            std::fs::write(dir.join("a/b/c.txt"), "x").unwrap();
            assert!(dir.is_dir());
            kept = dir.to_path_buf();
            assert_ne!(*TempDir::new("gp-scratch"), *kept, "every one is new");
        }
        assert!(!kept.exists());
    }

    #[test]
    fn a_failing_test_cleans_up_too() {
        let path = std::panic::catch_unwind(|| {
            let dir = TempDir::new("gp-scratch-panic");
            std::fs::write(dir.join("f"), "x").unwrap();
            std::panic::panic_any(dir.to_path_buf());
        })
        .unwrap_err()
        .downcast::<PathBuf>()
        .unwrap();
        assert!(!path.exists());
    }
}
