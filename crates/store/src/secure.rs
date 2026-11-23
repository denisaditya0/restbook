//! Create the files and directories this app owns with a private mode.
//!
//! The history database, a JSONL history and its snapshot, `restbook.local.json`,
//! and the app's log, settings, and layout can each hold a credential or a
//! response body. A plain `File::create` takes its mode from the umask (0644 on
//! a default install), so on Unix a file this module creates is `0600` and a
//! directory it creates is `0700`. Windows has no mode to set — the user account
//! is the boundary — so every helper is a plain passthrough there.
//!
//! These helpers only tighten what the app owns. An existing directory keeps its
//! mode, because a workspace root is a user's own checkout and changing its
//! permissions is not this app's decision.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

#[cfg(unix)]
const FILE_MODE: u32 = 0o600;
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;

/// Create `path` and any missing parents, then restrict the leaf directory to
/// `0700`. An existing directory is left exactly as it is.
pub fn create_dir(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || path.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(path)?;
    restrict_dir(path)
}

/// Create `path`, truncating an existing file, with `0600` on Unix.
pub fn create(path: &Path) -> io::Result<File> {
    open(path, false)
}

/// Create `path` only when it does not exist, with `0600` on Unix.
pub fn create_new(path: &Path) -> io::Result<File> {
    open(path, true)
}

/// Open `path` for appending, creating it `0600` on Unix when absent.
pub fn open_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    apply_mode(&mut options);
    let file = options.open(path)?;
    restrict_file(&file)?;
    Ok(file)
}

/// Write `contents` to `path` with `0600` on Unix.
pub fn write(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let mut file = create(path)?;
    file.write_all(contents.as_ref())
}

/// Restrict a file that was created by something other than this module, such
/// as SQLCipher creating its database.
pub fn restrict_path(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn open(path: &Path, exclusive: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).truncate(true);
    if exclusive {
        options.create_new(true);
    } else {
        options.create(true);
    }
    apply_mode(&mut options);
    let file = options.open(path)?;
    // `mode` only applies to a file this call created; an older file may still
    // be 0644, so tighten whatever was opened.
    restrict_file(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn apply_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(FILE_MODE);
}

#[cfg(not(unix))]
fn apply_mode(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn restrict_file(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(FILE_MODE))
}

#[cfg(not(unix))]
fn restrict_file(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(DIR_MODE))
}

#[cfg(not(unix))]
fn restrict_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::{create, create_dir, create_new, open_append, restrict_path};
    use std::fs;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("restbook-secure-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mode(path: &PathBuf) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_created_file_is_private() {
        let dir = temp_dir("file");
        let path = dir.join("secret");
        let mut file = create(&path).unwrap();
        file.write_all(b"x").unwrap();
        drop(file);
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn an_appended_file_is_private_and_an_old_mode_is_tightened() {
        let dir = temp_dir("append");
        let path = dir.join("history.jsonl");
        fs::write(&path, b"old\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let mut file = open_append(&path).unwrap();
        file.write_all(b"new\n").unwrap();
        drop(file);

        assert_eq!(mode(&path), 0o600);
        assert_eq!(fs::read_to_string(&path).unwrap(), "old\nnew\n");
    }

    #[test]
    fn create_new_refuses_an_existing_file() {
        let dir = temp_dir("new");
        let path = dir.join("marker");
        drop(create_new(&path).unwrap());
        assert!(create_new(&path).is_err());
    }

    #[test]
    fn a_created_directory_is_private() {
        let dir = temp_dir("dir");
        let path = dir.join("data");
        create_dir(&path).unwrap();
        assert_eq!(mode(&path), 0o700);
    }

    #[test]
    fn an_existing_directory_keeps_its_mode() {
        let dir = temp_dir("keep");
        let path = dir.join("checkout");
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

        create_dir(&path).unwrap();

        assert_eq!(mode(&path), 0o755);
    }

    #[test]
    fn an_externally_created_file_can_be_restricted() {
        let dir = temp_dir("restrict");
        let path = dir.join("history.db");
        fs::write(&path, b"x").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        restrict_path(&path).unwrap();

        assert_eq!(mode(&path), 0o600);
    }
}
