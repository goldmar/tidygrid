//! Every write is preceded by a copy of what was there.

use std::fs::{File, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde_json::Value;

pub type Result<T> = std::result::Result<T, String>;

const APPLY: &str = "apply";
const RESTORE: &str = "restore";

pub fn directory() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".tidygrid").join("backups")
}

fn slug(serial: &str) -> String {
    serial
        .chars()
        .map(|char| {
            if char.is_alphanumeric() || char == '-' || char == '_' {
                char
            } else {
                '-'
            }
        })
        .collect()
}

/// Civil date from days since the epoch — Howard Hinnant's algorithm.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// UTC, seconds, sortable — the file name is also the ordering.
fn stamp_at(now: u64) -> String {
    let (days, seconds) = ((now / 86_400) as i64, now % 86_400);
    let (hour, minute, second) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    let (year, month, day) = civil(days);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

fn stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}-{:09}", stamp_at(now.as_secs()), now.subsec_nanos())
}

fn ensure_private_directory(directory: &Path) -> Result<()> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("could not make the backup folder: {error}"))?;
    let metadata = std::fs::symlink_metadata(directory)
        .map_err(|error| format!("could not inspect the backup folder: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("the backup path is not a private directory".to_string());
    }
    std::fs::set_permissions(directory, Permissions::from_mode(0o700))
        .map_err(|error| format!("could not protect the backup folder: {error}"))
}

fn save_in(
    directory: &Path,
    state: &Value,
    serial: &str,
    kind: &str,
    timestamp: &str,
) -> Result<PathBuf> {
    ensure_private_directory(directory)?;
    let mut body = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("could not write the backup: {error}"))?;
    body.push(b'\n');

    for suffix in 0..1000 {
        let suffix = if suffix == 0 {
            String::new()
        } else {
            format!("-{suffix}")
        };
        let path = directory.join(format!(
            "{timestamp}-{kind}-{}-{pid}{suffix}.json",
            slug(serial),
            pid = std::process::id()
        ));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("could not create the backup: {error}")),
        };
        if let Err(error) = file.write_all(&body).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(format!("could not finish the backup: {error}"));
        }
        File::open(directory)
            .and_then(|folder| folder.sync_all())
            .map_err(|error| format!("could not publish the backup durably: {error}"))?;
        return Ok(path);
    }

    Err("could not choose a unique backup filename".to_string())
}

pub fn lock_device(serial: &str) -> Result<File> {
    let root = directory()
        .parent()
        .ok_or_else(|| "could not resolve the TidyGrid data folder".to_string())?
        .to_path_buf();
    ensure_private_directory(&root)?;
    let path = root.join(format!("{}.lock", slug(serial)));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("could not open the device lock: {error}"))?;
    file.set_permissions(Permissions::from_mode(0o600))
        .map_err(|error| format!("could not protect the device lock: {error}"))?;
    file.try_lock_exclusive()
        .map_err(|_| "another TidyGrid write is already in progress for this iPhone".to_string())?;
    Ok(file)
}

pub fn save(state: &Value, serial: &str, kind: &str) -> Result<PathBuf> {
    save_in(&directory(), state, serial, kind, &stamp())
}

pub fn save_before_write(state: &Value, serial: &str) -> Result<PathBuf> {
    save(state, serial, APPLY)
}

pub fn mark_restore(state: &Value, serial: &str) -> Result<PathBuf> {
    save(state, serial, RESTORE)
}

#[cfg(test)]
mod tests {
    use super::{civil, save_in, slug, stamp_at};
    use serde_json::json;

    /// The date is worked out by hand, so it is checked against days whose
    /// answers are known — including both kinds of century.
    #[test]
    fn the_calendar_is_right() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(59), (1970, 3, 1));
        assert_eq!(civil(11_016), (2000, 2, 29)); // 2000 is a leap year
        assert_eq!(civil(19_782), (2024, 2, 29));
        assert_eq!(civil(20_754), (2026, 10, 28));
    }

    #[test]
    fn the_stamp_sorts_and_carries_the_time() {
        assert_eq!(stamp_at(0), "19700101T000000Z");
        assert_eq!(stamp_at(19_782 * 86_400 + 3_661), "20240229T010101Z");
        assert!(stamp_at(1) < stamp_at(86_400));
    }

    #[test]
    fn a_serial_with_odd_characters_is_slugged() {
        assert_eq!(slug("0000-8140_ABC"), "0000-8140_ABC");
        assert_eq!(slug("a/b c:d"), "a-b-c-d");
    }

    #[test]
    fn backups_never_overwrite_a_same_timestamp_file() {
        let directory = std::env::temp_dir().join(format!(
            "tidygrid-backup-test-{}-{}",
            std::process::id(),
            stamp_at(0)
        ));
        let first = save_in(&directory, &json!(["first"]), "phone", "apply", "same").unwrap();
        let second = save_in(&directory, &json!(["second"]), "phone", "apply", "same").unwrap();

        assert_ne!(first, second);
        assert_eq!(
            std::fs::read_to_string(first).unwrap(),
            "[\n  \"first\"\n]\n"
        );
        assert_eq!(
            std::fs::read_to_string(second).unwrap(),
            "[\n  \"second\"\n]\n"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
