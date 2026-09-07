use crate::{
    config::App,
    process::{args, Executor},
    safety,
};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::PermissionsExt,
    path::Path,
};

pub fn sha256(path: &Path) -> Result<String> {
    safety::no_symlinks(path)?;
    let mut f = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub fn checksum(path: &Path, expected: &str) -> Result<()> {
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("expected SHA-256 must contain exactly 64 hexadecimal characters");
    }
    if sha256(path)? != expected.to_lowercase() {
        bail!("SHA-256 mismatch: {}", path.display());
    }
    Ok(())
}

/// Deliberately restricted ZIP subset. No symlinks, special files, duplicates,
/// case aliases, dot paths, backslashes, or entries outside the expected app.
pub fn extract(zip: &Path, expected: &str, dest: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(zip)?)?;
    if archive.len() > 100_000 {
        bail!("archive has too many entries");
    }
    let mut names = HashSet::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        let name = entry.name().trim_end_matches('/');
        let path = Path::new(name);
        safety::relative(path)?;
        if name.contains('\\')
            || name.contains('\0')
            || path
                .components()
                .any(|c| c.as_os_str().to_string_lossy().contains(':'))
            || path.components().next().map(|c| c.as_os_str())
                != Some(std::ffi::OsStr::new(expected))
        {
            bail!("unsafe archive entry: {name}");
        }
        if !names.insert(name.to_lowercase()) {
            bail!("duplicate/case-aliased archive entry: {name}");
        }
        let kind = entry.unix_mode().unwrap_or(0) & 0o170000;
        if kind != 0 && kind != 0o100000 && kind != 0o040000 {
            bail!("symlink or special ZIP entry unsupported: {name}");
        }
        total = total
            .checked_add(entry.size())
            .context("archive size overflow")?;
        if total > 8 * 1024 * 1024 * 1024 {
            bail!("archive exceeds 8 GiB extraction limit");
        }
    }
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let path = dest.join(entry.name());
        safety::no_symlinks(&path)?;
        if entry.is_dir() {
            fs::create_dir_all(&path)?;
            continue;
        }
        fs::create_dir_all(path.parent().context("entry has no parent")?)?;
        let mode = entry.unix_mode().unwrap_or(0o644) & 0o777;
        let expected_size = entry.size();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let copied = std::io::copy(&mut (&mut entry).take(expected_size + 1), &mut file)?;
        if copied != expected_size {
            bail!("ZIP expanded size mismatch");
        }
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
    }
    Ok(())
}

pub fn pack(bundle: &Path, destination: &Path) -> Result<()> {
    let root = bundle.parent().context("bundle has no parent")?;
    let mut zip = zip::ZipWriter::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?,
    );
    fn add(zip: &mut zip::ZipWriter<File>, path: &Path, root: &Path) -> Result<()> {
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() || !(meta.is_dir() || meta.is_file()) {
            bail!(
                "only regular files/directories supported in bundles: {}",
                path.display()
            );
        }
        let name = path
            .strip_prefix(root)?
            .to_str()
            .context("bundle path is not UTF-8")?;
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(meta.permissions().mode() & 0o777);
        if meta.is_dir() {
            zip.add_directory(format!("{name}/"), opts)?;
            let mut children = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            children.sort_by_key(|e| e.file_name());
            for child in children {
                add(zip, &child.path(), root)?;
            }
        } else {
            zip.start_file(name, opts)?;
            std::io::copy(&mut File::open(path)?, zip)?;
        }
        Ok(())
    }
    add(&mut zip, bundle, root)?;
    zip.finish()?.sync_all()?;
    Ok(())
}

pub fn bundle(app: &App, path: &Path, exec: &impl Executor) -> Result<String> {
    safety::no_symlinks(path)?;
    if !path.is_dir() {
        bail!("missing app bundle: {}", path.display());
    }
    let plist = path.join("Contents/Info.plist");
    safety::no_symlinks(&plist)?;
    let field = |key: &str| {
        exec.output(
            &args(&[
                "/usr/bin/plutil",
                "-extract",
                key,
                "raw",
                "-o",
                "-",
                plist.to_str().context("non-UTF8 plist path")?,
            ]),
            path,
        )
    };
    let id = field("CFBundleIdentifier")?;
    if id != app.bundle_id {
        bail!(
            "bundle identifier mismatch: expected {}, got {id}",
            app.bundle_id
        );
    }
    let version = field("CFBundleShortVersionString")?;
    if version.is_empty() {
        bail!("bundle version is empty");
    }
    let exe = field("CFBundleExecutable")?;
    if !crate::config::token(&exe) {
        bail!("unsafe bundle executable name");
    }
    let exe = path.join("Contents/MacOS").join(exe);
    safety::no_symlinks(&exe)?;
    let arches = exec.output(
        &args(&[
            "/usr/bin/lipo",
            "-archs",
            exe.to_str().context("non-UTF8 executable path")?,
        ]),
        path,
    )?;
    let wanted = if app.architecture.starts_with("aarch64") {
        "arm64"
    } else {
        "x86_64"
    };
    if !arches.split_whitespace().any(|s| s == wanted) {
        bail!("bundle does not contain {wanted}: {arches}");
    }
    let p = path.to_str().context("non-UTF8 bundle path")?;
    exec.run(
        &args(&[
            "/usr/bin/codesign",
            "--verify",
            "--deep",
            "--strict",
            "--verbose=2",
            p,
        ]),
        path,
        &[],
    )?;
    // codesign's designated requirement checks publisher policy, not just integrity.
    if app.signing.identity == "-" {
        // Ad-hoc and Developer ID signatures are both integrity-valid in this mode.
    } else {
        let team = app.signing.team_id.as_ref().context("missing team ID")?;
        let requirement = format!("anchor apple generic and certificate leaf[subject.OU] = \"{team}\" and certificate leaf[field.1.2.840.113635.100.6.1.13] exists");
        exec.run(
            &args(&[
                "/usr/bin/codesign",
                "--verify",
                "--strict",
                "-R",
                &requirement,
                p,
            ]),
            path,
            &[],
        )?;
    }
    if app.signing.notary_profile.is_some() {
        exec.run(
            &args(&["/usr/bin/xcrun", "stapler", "validate", p]),
            path,
            &[],
        )?;
        exec.run(
            &args(&["/usr/sbin/spctl", "--assess", "--type", "execute", p]),
            path,
            &[],
        )?;
    }
    Ok(version)
}

pub fn verify(app: &App, zip: &Path, hash: &str, exec: &impl Executor) -> Result<String> {
    let temp = safety::tempdir()?;
    // Snapshot before hashing/extracting so changing the supplied ZIP cannot
    // swap the bytes between these operations.
    safety::no_symlinks(zip)?;
    let snapshot = temp.path().join("input.zip");
    fs::copy(zip, &snapshot)?;
    checksum(&snapshot, hash)?;
    extract(
        &snapshot,
        app.production_app
            .file_name()
            .unwrap()
            .to_str()
            .context("invalid app filename")?,
        temp.path(),
    )?;
    inherit_quarantine(
        zip,
        &temp.path().join(app.production_app.file_name().unwrap()),
    )?;
    bundle(
        app,
        &temp.path().join(app.production_app.file_name().unwrap()),
        exec,
    )
}

/// Preserve a downloaded archive's quarantine marker on its extracted bundle.
/// ZIP payloads don't carry the source file's extended attributes themselves.
pub fn inherit_quarantine(source: &Path, bundle: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let source = CString::new(source.as_os_str().as_bytes())?;
        let bundle = CString::new(bundle.as_os_str().as_bytes())?;
        let attr = c"com.apple.quarantine";
        let mut value = vec![0u8; 4096];
        // SAFETY: C strings and the writable buffer remain valid across the call.
        let size = unsafe {
            libc::getxattr(
                source.as_ptr(),
                attr.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        if size < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOATTR) {
                return Ok(());
            }
            return Err(error).context("read archive quarantine metadata");
        }
        // SAFETY: getxattr returned the initialized byte count in value.
        if unsafe {
            libc::setxattr(
                bundle.as_ptr(),
                attr.as_ptr(),
                value.as_ptr().cast(),
                size as usize,
                0,
                libc::XATTR_NOFOLLOW,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("preserve archive quarantine on bundle");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (source, bundle);
    Ok(())
}
