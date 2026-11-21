use anyhow::{bail, Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Component, Path},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn stamp() -> String {
    format!(
        "{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        std::process::id()
    )
}

pub fn absolute(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        || path.components().count() < 3
    {
        bail!(
            "expected a specific absolute path without dot components: {}",
            path.display()
        );
    }
    no_symlinks(path)
}

pub fn relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("unsafe relative path: {}", path.display());
    }
    Ok(())
}

pub fn no_symlinks(path: &Path) -> Result<()> {
    let mut prefix = std::path::PathBuf::new();
    for c in path.components() {
        prefix.push(c);
        match fs::symlink_metadata(&prefix) {
            Ok(m) if m.file_type().is_symlink() => {
                bail!("symlink not permitted: {}", prefix.display())
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e).with_context(|| format!("inspect {}", prefix.display())),
        }
    }
    Ok(())
}

/// Open each path component relative to an already opened directory. This
/// prevents a component being replaced by a symlink between inspection and use.
pub fn open_regular_no_symlinks(path: &Path) -> Result<File> {
    absolute(path)?;
    let parts: Vec<_> = path.components().skip(1).collect();
    let mut directory = File::open("/")?;
    for (index, part) in parts.iter().enumerate() {
        let Component::Normal(name) = part else {
            bail!("unsafe artifact path");
        };
        let name = std::ffi::CString::new(name.as_bytes()).context("invalid artifact path")?;
        let last = index + 1 == parts.len();
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if last {
                libc::O_NONBLOCK
            } else {
                libc::O_DIRECTORY
            };
        // SAFETY: directory and name stay live during openat; a successful fd
        // is immediately transferred to File exactly once.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("open artifact without symlinks");
        }
        let opened = unsafe { File::from_raw_fd(fd) };
        if last {
            let metadata = opened.metadata()?;
            if !metadata.is_file() || metadata.nlink() != 1 {
                bail!("artifact must be a regular file without hard links");
            }
            return Ok(opened);
        }
        directory = opened;
    }
    bail!("artifact path has no file")
}

pub fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a) || {
        let a = a.to_string_lossy().to_lowercase();
        let b = b.to_string_lossy().to_lowercase();
        a == b || a.starts_with(&(b.clone() + "/")) || b.starts_with(&(a + "/"))
    }
}

pub struct Lock(File);
impl Lock {
    pub fn acquire(path: &Path, wait: bool) -> Result<Self> {
        no_symlinks(path)?;
        fs::create_dir_all(path.parent().context("lock has no parent")?)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let flags = libc::LOCK_EX | if wait { 0 } else { libc::LOCK_NB };
        // SAFETY: flock receives a live file descriptor owned by this guard.
        if unsafe { libc::flock(file.as_raw_fd(), flags) } != 0 {
            bail!(
                "operation locked at {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            );
        }
        Ok(Self(file))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        // SAFETY: the owned descriptor is still live.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub fn create_new(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    no_symlinks(path)?;
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)?;
    f.sync_all()?;
    Ok(())
}

pub fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

/// Canonicalize the OS-provided temp root, not an untrusted configured path.
pub fn tempdir() -> Result<tempfile::TempDir> {
    Ok(tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?)
}
