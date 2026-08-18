//! Local file content hashing. Used to detect that a local file changed
//! since the last sync, independent of whatever hash algorithm a provider
//! reports for the remote side (Drive: md5, OneDrive: quickXorHash) — see
//! state_db.rs and sync_engine.rs.

use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::Path;

pub fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_content_hashes_equal() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, b"hello world").unwrap();
        std::fs::write(&b, b"hello world").unwrap();
        assert_eq!(hash_file(&a).unwrap(), hash_file(&b).unwrap());
    }

    #[test]
    fn different_content_hashes_differ() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, b"hello world").unwrap();
        std::fs::write(&b, b"hello there").unwrap();
        assert_ne!(hash_file(&a).unwrap(), hash_file(&b).unwrap());
    }
}
