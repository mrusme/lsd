use localsend::util::filename;
use std::collections::HashSet;
use std::fs::Metadata;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_DUPLICATES: u32 = 10_000;

pub const STDIN: &str = "-";

pub fn is_stdin(path: &Path) -> bool {
    path.as_os_str() == STDIN
}

pub fn regular_file(path: &Path) -> Result<Metadata, String> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(metadata),
        Ok(metadata) if metadata.is_dir() => Err(format!("Not a file: {}", path.display())),
        Ok(_) => Err(format!("Not a regular file: {}", path.display())),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            Err(format!("No such file: {}", path.display()))
        }
        Err(err) => Err(format!("Could not read {}: {err}", path.display())),
    }
}

#[allow(clippy::cast_precision_loss)]
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} B"),
        _ => format!("{value:.1} {}", UNITS[unit]),
    }
}

pub fn format_duration(duration: Duration) -> String {
    let secs = Duration::from_secs_f64(seconds(duration)).as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

pub fn count_files(count: usize) -> String {
    match count {
        1 => "1 file".to_string(),
        count => format!("{count} files"),
    }
}

pub fn seconds(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 100.0).round() / 100.0
}

pub fn unique_path(dir: &Path, file_name: &str, taken: &HashSet<PathBuf>) -> Option<PathBuf> {
    let name = filename::sanitize_path(file_name, filename::Rules::current());
    let free = |candidate: &PathBuf| !taken.contains(candidate) && !candidate.exists();

    let candidate = dir.join(&name);
    if free(&candidate) {
        return Some(candidate);
    }

    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name.as_str(), String::new()),
    };
    (1..=MAX_DUPLICATES)
        .map(|index| dir.join(format!("{stem} ({index}){extension}")))
        .find(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free(dir: &Path, name: &str) -> PathBuf {
        unique_path(dir, name, &HashSet::new()).unwrap()
    }

    #[test]
    fn a_free_name_is_used_as_is() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            free(dir.path(), "report.pdf"),
            dir.path().join("report.pdf")
        );
    }

    #[test]
    fn an_existing_file_gets_a_counter_before_the_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("report.pdf"), b"x").unwrap();
        assert_eq!(
            free(dir.path(), "report.pdf"),
            dir.path().join("report (1).pdf")
        );
        std::fs::write(dir.path().join("report (1).pdf"), b"x").unwrap();
        assert_eq!(
            free(dir.path(), "report.pdf"),
            dir.path().join("report (2).pdf")
        );
    }

    #[test]
    fn a_name_without_an_extension_gets_the_counter_appended() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README"), b"x").unwrap();
        assert_eq!(free(dir.path(), "README"), dir.path().join("README (1)"));
    }

    #[test]
    fn a_dotfile_keeps_its_leading_dot() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".bashrc"), b"x").unwrap();
        assert_eq!(free(dir.path(), ".bashrc"), dir.path().join(".bashrc (1)"));
    }

    #[test]
    fn a_reserved_path_is_skipped_even_though_no_file_exists_yet() {
        let dir = tempfile::tempdir().unwrap();
        let mut taken = HashSet::new();

        let first = unique_path(dir.path(), "same.txt", &taken).unwrap();
        assert_eq!(first, dir.path().join("same.txt"));
        taken.insert(first);

        let second = unique_path(dir.path(), "same.txt", &taken).unwrap();
        assert_eq!(second, dir.path().join("same (1).txt"));
        taken.insert(second);

        let third = unique_path(dir.path(), "same.txt", &taken).unwrap();
        assert_eq!(third, dir.path().join("same (2).txt"));
    }

    #[test]
    fn a_traversing_name_cannot_escape_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = free(dir.path(), "../../etc/passwd");
        assert_eq!(path, dir.path().join("passwd"));
        assert!(path.starts_with(dir.path()));

        let nested = free(dir.path(), "a/b/c.txt");
        assert_eq!(nested, dir.path().join("c.txt"));
    }

    #[test]
    fn an_exhausted_directory_yields_no_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut taken = HashSet::new();
        taken.insert(dir.path().join("x.txt"));
        for index in 1..=MAX_DUPLICATES {
            taken.insert(dir.path().join(format!("x ({index}).txt")));
        }
        assert!(unique_path(dir.path(), "x.txt", &taken).is_none());
    }

    #[test]
    fn a_path_is_classified_before_it_is_offered() {
        let dir = tempfile::tempdir().unwrap();

        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"hello").unwrap();
        assert_eq!(regular_file(&file).unwrap().len(), 5);

        assert_eq!(
            regular_file(dir.path()).unwrap_err(),
            format!("Not a file: {}", dir.path().display())
        );

        let missing = dir.path().join("nope.txt");
        assert_eq!(
            regular_file(&missing).unwrap_err(),
            format!("No such file: {}", missing.display())
        );
    }

    #[test]
    fn byte_sizes_use_decimal_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1000), "1.0 KB");
        assert_eq!(format_bytes(2_000_012), "2.0 MB");
        assert_eq!(format_bytes(1_500_000_000_000_000), "1500.0 TB");
    }

    #[test]
    fn file_counts_are_pluralized() {
        assert_eq!(count_files(0), "0 files");
        assert_eq!(count_files(1), "1 file");
        assert_eq!(count_files(2), "2 files");
    }

    #[test]
    #[allow(clippy::duration_suboptimal_units)]
    fn durations_switch_unit_at_a_minute_and_an_hour() {
        assert_eq!(format_duration(Duration::from_secs(0)), "0s");
        assert_eq!(format_duration(Duration::from_secs(59)), "59s");
        assert_eq!(format_duration(Duration::from_secs(60)), "1m 0s");
        assert_eq!(format_duration(Duration::from_secs(3599)), "59m 59s");
        assert_eq!(format_duration(Duration::from_secs(3600)), "1h 0m");
        assert_eq!(format_duration(Duration::from_secs(7380)), "2h 3m");
    }
}
