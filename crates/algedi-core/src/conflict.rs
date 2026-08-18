//! Conflict copy naming and records. Never overwrite silently: when both
//! sides changed since the last known sync, we keep both versions.
//! See PROMPT-ALGEDI.md sec. 5.3.

use chrono::{DateTime, Local};
use std::path::{Path, PathBuf};

/// Builds the path for a conflicting copy, following the same convention as
/// the Google Drive / Dropbox desktop clients:
/// `nome (conflito de <hostname> em <data>).ext`
pub fn conflicting_file_name(original: &Path, hostname: &str, when: DateTime<Local>) -> PathBuf {
    let stem = original
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("arquivo");
    let ext = original.extension().and_then(|e| e.to_str());

    let stamp = when.format("%Y-%m-%d %H-%M-%S");
    let conflict_stem = format!("{stem} (conflito de {hostname} em {stamp})");

    let mut new_path = original.to_path_buf();
    match ext {
        Some(ext) => new_path.set_file_name(format!("{conflict_stem}.{ext}")),
        None => new_path.set_file_name(conflict_stem),
    }
    new_path
}

#[derive(Debug, Clone)]
pub struct Conflict {
    pub id: uuid::Uuid,
    pub pair_id: crate::PairId,
    pub path: PathBuf,
    pub conflicting_copy_path: PathBuf,
    pub detected_at: DateTime<chrono::Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn appends_conflict_marker_before_extension() {
        let when = Local.with_ymd_and_hms(2026, 8, 18, 14, 30, 0).unwrap();
        let result =
            conflicting_file_name(Path::new("/home/user/Docs/report.docx"), "desktop-01", when);
        assert_eq!(
            result,
            PathBuf::from(
                "/home/user/Docs/report (conflito de desktop-01 em 2026-08-18 14-30-00).docx"
            )
        );
    }

    #[test]
    fn handles_files_without_extension() {
        let when = Local.with_ymd_and_hms(2026, 8, 18, 9, 0, 0).unwrap();
        let result = conflicting_file_name(Path::new("/home/user/README"), "laptop", when);
        assert_eq!(
            result,
            PathBuf::from("/home/user/README (conflito de laptop em 2026-08-18 09-00-00)")
        );
    }
}
