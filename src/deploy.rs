use crate::{
    artifact,
    config::App,
    process::{args, Executor},
    safety::{self, Lock},
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Journal {
    pub destination: PathBuf,
    pub backup: PathBuf,
    pub staged: PathBuf,
    pub had_previous: bool,
}

/// This function is platform-independent and testable. Callers must hold the
/// deployment lock and place stage/backup on the destination filesystem.
pub fn transaction(
    destination: &Path,
    staged: &Path,
    backup: &Path,
    journal: &Path,
    validate: impl Fn(&Path) -> Result<()>,
    running: impl Fn() -> Result<bool>,
) -> Result<()> {
    for p in [destination, staged, backup, journal] {
        safety::no_symlinks(p)?;
    }
    if journal.exists() {
        bail!(
            "unfinished deployment journal {}; run recover",
            journal.display()
        );
    }
    if backup.exists() {
        bail!("backup already exists");
    }
    validate(staged)?;
    if running()? {
        bail!("installed application is running; quit it manually before promotion");
    }
    if destination.exists() {
        validate(destination)?;
    }
    let record = Journal {
        destination: destination.into(),
        staged: staged.into(),
        backup: backup.into(),
        had_previous: destination.exists(),
    };
    safety::create_new(journal, &serde_json::to_vec_pretty(&record)?)?;
    safety::sync_dir(journal.parent().unwrap())?;
    let outcome = (|| {
        if running()? {
            bail!("application started during promotion; aborting");
        }
        if record.had_previous {
            fs::rename(destination, backup).context("move installed app to backup")?;
            safety::sync_dir(backup.parent().unwrap())?;
            safety::sync_dir(destination.parent().unwrap())?;
        }
        fs::rename(staged, destination).context("install staged app")?;
        safety::sync_dir(destination.parent().unwrap())?;
        validate(destination).context("post-install validation")?;
        Ok(())
    })();
    match outcome {
        Ok(()) => {
            fs::remove_file(journal)?;
            safety::sync_dir(journal.parent().unwrap())?;
            Ok(())
        }
        Err(error) => {
            if let Err(recovery) = restore(&record, journal, &running) {
                bail!("installation failed: {error:#}; recovery also failed: {recovery:#}; preserve journal {}", journal.display());
            }
            Err(error)
        }
    }
}

fn restore(record: &Journal, journal: &Path, running: &impl Fn() -> Result<bool>) -> Result<()> {
    if running()? {
        bail!("application running; recovery refused");
    }
    for p in [&record.destination, &record.staged, &record.backup, journal] {
        safety::no_symlinks(p)?;
    }
    if record.had_previous {
        if record.backup.exists() {
            if record.destination.exists() {
                if record.staged.exists() {
                    bail!("ambiguous recovery: both staged and installed apps exist");
                }
                fs::rename(&record.destination, &record.staged)
                    .context("retain rejected installation")?;
            }
            fs::rename(&record.backup, &record.destination)
                .context("restore previous application")?;
        } else if !record.destination.exists() || !record.staged.exists() {
            bail!("ambiguous recovery: expected previous app or backup missing");
        }
    } else if record.destination.exists() {
        if record.staged.exists() {
            bail!("ambiguous first-install recovery");
        }
        fs::rename(&record.destination, &record.staged)?;
    }
    safety::sync_dir(record.destination.parent().unwrap())?;
    fs::remove_file(journal)?;
    safety::sync_dir(journal.parent().unwrap())?;
    Ok(())
}

pub fn is_running(app: &App, exec: &impl Executor) -> Result<bool> {
    // NSWorkspace catches renamed bundles with the same bundle ID; ps below
    // supplements it with command paths across users.
    let script = "ObjC.import('AppKit'); JSON.stringify(ObjC.unwrap($.NSWorkspace.sharedWorkspace.runningApplications).map(a => ({id: ObjC.unwrap(a.bundleIdentifier), path: a.bundleURL ? ObjC.unwrap(a.bundleURL.path) : ''})))";
    let output = exec.output(
        &args(&["/usr/bin/osascript", "-l", "JavaScript", "-e", script]),
        Path::new("/"),
    )?;
    let apps: Vec<serde_json::Value> = serde_json::from_str(&output)
        .context("cannot inspect running macOS applications; refusing deployment")?;
    if apps.iter().any(|a| {
        a["id"].as_str() == Some(&app.bundle_id)
            || a["path"].as_str() == app.production_app.to_str()
    }) {
        return Ok(true);
    }
    // ps -ww avoids truncation. Inspect all processes, not just this user's.
    let processes = exec.output(
        &args(&["/bin/ps", "-axo", "command=", "-ww"]),
        Path::new("/"),
    )?;
    let prefix = format!("{}/", app.production_app.display());
    Ok(processes.lines().any(|line| line.contains(&prefix)))
}

pub fn backup_root(app: &App) -> PathBuf {
    Path::new("/Applications/.aruvici-backups").join(&app.name)
}
pub fn list_backups(app: &App) -> Result<Vec<PathBuf>> {
    let root = backup_root(app);
    safety::no_symlinks(&root)?;
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut paths = vec![];
    for entry in fs::read_dir(root)? {
        let p = entry?.path();
        safety::no_symlinks(&p)?;
        if p.extension().and_then(|s| s.to_str()) == Some("app") && p.is_dir() {
            paths.push(p);
        }
    }
    paths.sort();
    Ok(paths)
}
pub fn promote(
    app: &App,
    zip: &Path,
    hash: &str,
    dry_run: bool,
    exec: &impl Executor,
    confirm: impl FnOnce() -> Result<()>,
) -> Result<serde_json::Value> {
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        bail!("deployment is forbidden inside GitHub Actions; promote from an interactive session");
    }
    app.require_isolation()?;
    let tmp = safety::tempdir()?;
    let input = tmp.path().join("input.zip");
    safety::no_symlinks(zip)?;
    fs::copy(zip, &input)?;
    artifact::checksum(&input, hash)?;
    let name = app.production_app.file_name().unwrap();
    artifact::extract(
        &input,
        name.to_str().context("invalid bundle name")?,
        tmp.path(),
    )?;
    let extracted = tmp.path().join(name);
    artifact::inherit_quarantine(zip, &extracted)?;
    let version = artifact::bundle(app, &extracted, exec)?;
    safety::no_symlinks(&app.production_app)?;
    let root = backup_root(app);
    safety::no_symlinks(&root)?;
    if root.join("transaction.json").exists() {
        bail!("unfinished transaction; run recover before promotion");
    }
    if is_running(app, exec)? {
        bail!("{} is running; quit it manually", app.name);
    }
    let details = serde_json::json!({"destination":app.production_app,"sha256":hash,"version":version,"signing_identity":app.signing.identity,"signature":"verified","dry_run":dry_run});
    if dry_run {
        return Ok(details);
    }
    confirm()?; // Last step before the first /Applications mutation.
    fs::create_dir_all(&root)?;
    let _lock = Lock::acquire(&root.join("deploy.lock"), false)?;
    let stage = root.join(format!("stage-{}", safety::stamp()));
    fs::create_dir(&stage)?;
    let staged = stage.join(name);
    exec.run(
        &args(&[
            "/usr/bin/ditto",
            extracted.to_str().unwrap(),
            staged.to_str().unwrap(),
        ]),
        &root,
        &[],
    )?;
    let backup = root.join(format!("{}.app", safety::stamp()));
    transaction(
        &app.production_app,
        &staged,
        &backup,
        &root.join("transaction.json"),
        |p| artifact::bundle(app, p, exec).map(|_| ()),
        || is_running(app, exec),
    )?;
    Ok(serde_json::json!({"promotion":details,"backup":backup,"status":"installed"}))
}

/// Install a verified test candidate below a manager-owned preview root. Unlike
/// `promote`, this never writes to /Applications and has no access to production
/// application data. It is used only after a target explicitly opted into Preview.
/// A running candidate is left untouched and reported as deferred.
pub fn install_preview(
    app: &App,
    zip: &Path,
    hash: &str,
    destination: &Path,
    preview_root: &Path,
    exec: &impl Executor,
) -> Result<serde_json::Value> {
    safety::absolute(preview_root)?;
    safety::absolute(destination)?;
    if !destination.starts_with(preview_root)
        || destination.extension().and_then(|s| s.to_str()) != Some("app")
        || destination.starts_with("/Applications")
    {
        bail!("preview destination must be an .app below the configured preview root");
    }
    safety::no_symlinks(zip)?;
    safety::no_symlinks(destination)?;
    let mut candidate = app.clone();
    candidate.production_app = destination.into();
    if is_running(&candidate, exec)? {
        return Ok(
            serde_json::json!({"status":"deferred","reason":"preview application is running","destination":destination}),
        );
    }
    let tmp = safety::tempdir()?;
    let input = tmp.path().join("input.zip");
    fs::copy(zip, &input)?;
    artifact::checksum(&input, hash)?;
    let name = destination
        .file_name()
        .context("preview destination has no filename")?;
    artifact::extract(
        &input,
        name.to_str().context("invalid preview bundle name")?,
        tmp.path(),
    )?;
    let extracted = tmp.path().join(name);
    artifact::inherit_quarantine(zip, &extracted)?;
    artifact::bundle(&candidate, &extracted, exec)?;

    let root = preview_root.join(&app.name);
    safety::no_symlinks(&root)?;
    fs::create_dir_all(root.join("backups"))?;
    fs::create_dir_all(
        destination
            .parent()
            .context("preview destination has no parent")?,
    )?;
    if root.join("transaction.json").exists() {
        bail!("unfinished preview transaction; recover it before installing another candidate");
    }
    let _lock = Lock::acquire(&root.join("preview.lock"), false)?;
    let had_previous = destination.exists();
    let stage = root.join(format!("stage-{}", safety::stamp()));
    fs::create_dir(&stage)?;
    let staged = stage.join(name);
    exec.run(
        &args(&[
            "/usr/bin/ditto",
            extracted.to_str().context("non-UTF8 extracted preview")?,
            staged.to_str().context("non-UTF8 preview stage")?,
        ]),
        &root,
        &[],
    )?;
    let backup = root
        .join("backups")
        .join(format!("{}.app", safety::stamp()));
    transaction(
        destination,
        &staged,
        &backup,
        &root.join("transaction.json"),
        |path| artifact::bundle(&candidate, path, exec).map(|_| ()),
        || is_running(&candidate, exec),
    )?;
    Ok(
        serde_json::json!({"status":"installed","destination":destination,"backup":if had_previous { Some(backup) } else { None::<PathBuf> }}),
    )
}

pub fn rollback(
    app: &App,
    backup_name: &str,
    dry_run: bool,
    exec: &impl Executor,
    confirm: impl FnOnce() -> Result<()>,
) -> Result<serde_json::Value> {
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        bail!("rollback is forbidden inside GitHub Actions");
    }
    if !crate::config::token(backup_name) || !backup_name.ends_with(".app") {
        bail!("backup must be a filename from list-backups");
    }
    let root = backup_root(app);
    let source = root.join(backup_name);
    safety::no_symlinks(&root)?;
    artifact::bundle(app, &source, exec)?;
    if is_running(app, exec)? {
        bail!("application is running");
    }
    if root.join("transaction.json").exists() {
        bail!("unfinished transaction; run recover");
    }
    if dry_run {
        return Ok(
            serde_json::json!({"restore":source,"destination":app.production_app,"dry_run":true}),
        );
    }
    confirm()?;
    let _lock = Lock::acquire(&root.join("deploy.lock"), false)?;
    let stage = root.join(format!("stage-{}", safety::stamp()));
    fs::create_dir(&stage)?;
    let staged = stage.join(app.production_app.file_name().unwrap());
    exec.run(
        &args(&[
            "/usr/bin/ditto",
            source.to_str().unwrap(),
            staged.to_str().unwrap(),
        ]),
        &root,
        &[],
    )?;
    let backup = root.join(format!("{}.app", safety::stamp()));
    transaction(
        &app.production_app,
        &staged,
        &backup,
        &root.join("transaction.json"),
        |p| artifact::bundle(app, p, exec).map(|_| ()),
        || is_running(app, exec),
    )?;
    Ok(serde_json::json!({"restored":source,"backup":backup}))
}

pub fn recover(
    app: &App,
    exec: &impl Executor,
    confirm: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        bail!("recovery is forbidden inside GitHub Actions");
    }
    let root = backup_root(app);
    let journal = root.join("transaction.json");
    safety::no_symlinks(&journal)?;
    let record: Journal = serde_json::from_slice(&fs::read(&journal)?)?;
    // Journals are untrusted; constrain every recovery path to this app.
    if record.destination != app.production_app
        || record.backup.parent() != Some(&root)
        || record.backup.extension().and_then(|s| s.to_str()) != Some("app")
        || record.staged.file_name() != app.production_app.file_name()
        || record.staged.parent().and_then(|p| p.parent()) != Some(&root)
        || !record
            .staged
            .parent()
            .and_then(|p| p.file_name())
            .unwrap_or_default()
            .to_string_lossy()
            .starts_with("stage-")
    {
        bail!("unsafe recovery journal");
    }
    for p in [&record.backup, &record.staged, &record.destination] {
        safety::absolute(p)?;
    }
    if record.backup.exists() {
        artifact::bundle(app, &record.backup, exec)?;
    }
    if is_running(app, exec)? {
        bail!("application is running");
    }
    confirm()?;
    let _lock = Lock::acquire(&root.join("deploy.lock"), false)?;
    // Fail if another operation replaced the journal while awaiting confirmation.
    if serde_json::to_value(&record)?
        != serde_json::from_slice::<serde_json::Value>(&fs::read(&journal)?)?
    {
        bail!("journal changed; retry recovery");
    }
    restore(&record, &journal, &|| is_running(app, exec))?;
    if record.had_previous {
        artifact::bundle(app, &app.production_app, exec)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_after_each_rename_restores_prior_state() {
        for installed in [false, true] {
            let temp = safety::tempdir().unwrap();
            let destination = temp.path().join("App.app");
            let staged = temp.path().join("staged.app");
            let backup = temp.path().join("backup.app");
            let journal = temp.path().join("journal.json");
            fs::create_dir(&backup).unwrap();
            fs::write(backup.join("version"), "old").unwrap();
            let new_path = if installed { &destination } else { &staged };
            fs::create_dir(new_path).unwrap();
            fs::write(new_path.join("version"), "new").unwrap();
            let record = Journal {
                destination: destination.clone(),
                staged: staged.clone(),
                backup,
                had_previous: true,
            };
            safety::create_new(&journal, &serde_json::to_vec(&record).unwrap()).unwrap();
            restore(&record, &journal, &|| Ok(false)).unwrap();
            assert_eq!(
                fs::read_to_string(destination.join("version")).unwrap(),
                "old"
            );
            assert_eq!(fs::read_to_string(staged.join("version")).unwrap(), "new");
            assert!(!journal.exists());
        }
    }
    #[test]
    fn recovery_refuses_running_app_and_preserves_journal() {
        let temp = safety::tempdir().unwrap();
        let journal = temp.path().join("journal.json");
        let record = Journal {
            destination: temp.path().join("App.app"),
            staged: temp.path().join("stage.app"),
            backup: temp.path().join("backup.app"),
            had_previous: false,
        };
        safety::create_new(&journal, &serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(restore(&record, &journal, &|| Ok(true)).is_err());
        assert!(journal.exists());
    }
}
