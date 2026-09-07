use crate::{
    artifact,
    config::{App, Registry},
    history::History,
    process::{args, Executor},
    safety::{self, Lock},
};
use anyhow::{bail, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn status(app: &App, repo: &Path, exec: &impl Executor) -> Result<String> {
    safety::absolute(repo)?;
    let origin = exec.output(&args(&["git", "remote", "get-url", "origin"]), repo)?;
    let normalized = origin.trim_end_matches(".git");
    if normalized != format!("https://github.com/{}", app.github)
        && normalized != format!("git@github.com:{}", app.github)
    {
        bail!(
            "repository origin {origin} does not match approved GitHub repository {}",
            app.github
        );
    }
    exec.output(
        &args(&[
            "git",
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
        ]),
        repo,
    )
}
pub fn resolve(app: &App, repo: &Path, reference: &str, exec: &impl Executor) -> Result<String> {
    if !status(app, repo, exec)?.is_empty() {
        bail!(
            "{} has dirty/untracked files; commit or stash explicitly before building",
            repo.display()
        );
    }
    let commit = exec.output(
        &args(&[
            "git",
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ]),
        repo,
    )?;
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("unexpected Git commit hash");
    }
    Ok(commit)
}

pub fn run(
    registry: &Registry,
    app: &App,
    repo: &Path,
    reference: &str,
    tests_only: bool,
    exec: &impl Executor,
) -> Result<PathBuf> {
    let _global = Lock::acquire(&registry.state_dir.join("locks/release.lock"), true)?;
    run_locked(registry, app, repo, reference, tests_only, exec)
}
pub fn run_locked(
    registry: &Registry,
    app: &App,
    repo: &Path,
    reference: &str,
    tests_only: bool,
    exec: &impl Executor,
) -> Result<PathBuf> {
    app.require_isolation()?;
    let _app = Lock::acquire(
        &registry.state_dir.join(format!("locks/{}.lock", app.name)),
        false,
    )?;
    let history = History::open(&registry.state_dir)?;
    let id = safety::stamp();
    let kind = if tests_only { "test" } else { "build" };
    let mut details = serde_json::json!({"build_id":id,"tests":"not_run","signing":"not_run","commit":null,"version":null,"sha256":null});
    history.event(&app.name, kind, "started", details.clone())?;
    let result = (|| {
        let commit = resolve(app, repo, reference, exec)?;
        details["commit"] = commit.clone().into();
        // Override repositories (CI checkout) cannot overlap state or any data/app path.
        for a in &registry.apps {
            for p in [&a.production_app, &a.production_data, &a.dev_data] {
                if safety::overlaps(repo, p) {
                    bail!("build repository overlaps protected path {}", p.display());
                }
            }
        }
        if safety::overlaps(repo, &registry.state_dir) {
            bail!("build repository overlaps manager state");
        }
        let work_root = registry.state_dir.join("work");
        safety::no_symlinks(&work_root)?;
        fs::create_dir_all(&work_root)?;
        let work = tempfile::Builder::new()
            .prefix("build-")
            .tempdir_in(&work_root)?;
        let checkout = work.path().join("checkout");
        exec.run(
            &args(&[
                "git",
                "clone",
                "--no-local",
                "--no-hardlinks",
                "--no-checkout",
                "--",
                repo.to_str().context("non-UTF8 repo")?,
                checkout.to_str().unwrap(),
            ]),
            work.path(),
            &[],
        )?;
        exec.run(
            &args(&[
                "git",
                "-c",
                "core.hooksPath=/dev/null",
                "checkout",
                "--detach",
                &commit,
            ]),
            &checkout,
            &[],
        )?;
        if checkout.join(".gitmodules").exists() {
            bail!("submodules are not supported; vendor audited dependencies or extend clone policy explicitly");
        }
        let env = vec![
            ("CI".into(), "true".into()),
            (
                "ARUVICI_TEST_DATA".into(),
                work.path().join("test-data").display().to_string(),
            ),
        ];
        exec.run(&app.install_command, &checkout, &env)
            .context("npm dependency installation")?;
        exec.run(&app.frontend_command, &checkout, &env)
            .context("TypeScript/frontend build")?;
        details["tests"] = "running".into();
        if let Err(e) = exec.run(&app.test_command, &checkout, &env) {
            details["tests"] = "failed".into();
            return Err(e.context("Rust tests"));
        }
        details["tests"] = "passed".into();
        if tests_only {
            return Ok(PathBuf::new());
        }
        let override_config = serde_json::json!({"build":{"devUrl":null,"beforeDevCommand":"","beforeBuildCommand":""},"bundle":{"macOS":{"signingIdentity":app.signing.identity}}});
        let mut cmd = app.tauri_command.clone();
        cmd.extend(args(&[
            "build",
            "--target",
            &app.architecture,
            "--bundles",
            "app",
            "--config",
            &override_config.to_string(),
        ]));
        exec.run(&cmd, &checkout, &env)
            .context("Tauri release build/signing")?;
        let bundle = checkout.join(&app.artifact);
        safety::no_symlinks(&bundle)?;
        // Notary validation happens after stapling. Check signature/team first.
        let mut pre_notary = app.clone();
        pre_notary.signing.notary_profile = None;
        details["version"] = artifact::bundle(&pre_notary, &bundle, exec)?.into();
        details["signing"] = if app.signing.identity == "-" {
            "adhoc_verified"
        } else {
            "developer_id_verified"
        }
        .into();
        if let Some(profile) = &app.signing.notary_profile {
            let submission = work.path().join("notary.zip");
            artifact::pack(&bundle, &submission)?;
            exec.run(
                &args(&[
                    "/usr/bin/xcrun",
                    "notarytool",
                    "submit",
                    submission.to_str().unwrap(),
                    "--keychain-profile",
                    profile,
                    "--wait",
                ]),
                &checkout,
                &[],
            )?;
            exec.run(
                &args(&[
                    "/usr/bin/xcrun",
                    "stapler",
                    "staple",
                    bundle.to_str().unwrap(),
                ]),
                &checkout,
                &[],
            )?;
            artifact::bundle(app, &bundle, exec)?;
            details["signing"] = "notarized_stapled_verified".into();
        }
        let artifacts = registry.state_dir.join("artifacts").join(&app.name);
        safety::no_symlinks(&artifacts)?;
        fs::create_dir_all(&artifacts)?;
        let staged = tempfile::Builder::new()
            .prefix("pending-")
            .tempdir_in(&artifacts)?;
        let zip = staged.path().join("app.zip");
        artifact::pack(&bundle, &zip)?;
        let hash = artifact::sha256(&zip)?;
        // Roundtrip validates exactly the deliverable, including archive policies.
        artifact::verify(app, &zip, &hash, exec)?;
        details["sha256"] = hash.clone().into();
        details["app"] = app.name.clone().into();
        safety::create_new(
            &staged.path().join("app.zip.sha256"),
            format!("{hash}  app.zip\n").as_bytes(),
        )?;
        safety::create_new(
            &staged.path().join("manifest.json"),
            &serde_json::to_vec_pretty(&details)?,
        )?;
        let destination = artifacts.join(&id);
        if destination.exists() {
            bail!("build ID collision");
        }
        fs::rename(staged.path(), &destination)?;
        safety::sync_dir(&artifacts)?;
        details["artifact"] = destination.join("app.zip").display().to_string().into();
        Ok(destination)
    })();
    match &result {
        Ok(_) => history.event(&app.name, kind, "complete", details)?,
        Err(e) => {
            details["error"] = format!("{e:#}").into();
            history.event(&app.name, kind, "failed", details)?;
        }
    }
    result
}

pub fn drain(registry: &Registry, exec: &impl Executor) -> Result<()> {
    let _global = Lock::acquire(&registry.state_dir.join("locks/release.lock"), true)?;
    let history = History::open(&registry.state_dir)?;
    let mut failed = false;
    while let Some((id, name, commit)) = history.claim()? {
        let result = registry
            .app(&name)
            .and_then(|app| run_locked(registry, app, &app.repository, &commit, false, exec));
        failed |= result.is_err();
        history.finish(id, result.err().map(|e| format!("{e:#}")))?;
        if crate::process::interrupted() {
            bail!("worker interrupted");
        }
    }
    if failed {
        bail!("one or more queued builds failed; inspect history");
    }
    Ok(())
}

pub fn clean(registry: &Registry, app: &App, keep: usize, dry_run: bool) -> Result<Vec<PathBuf>> {
    let _lock = Lock::acquire(
        &registry.state_dir.join(format!("locks/{}.lock", app.name)),
        false,
    )?;
    let root = registry.state_dir.join("artifacts").join(&app.name);
    safety::no_symlinks(&root)?;
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut paths = vec![];
    for entry in fs::read_dir(&root)? {
        let p = entry?.path();
        safety::no_symlinks(&p)?;
        let name = p.file_name().unwrap().to_string_lossy();
        if !name
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            || !p.is_dir()
        {
            continue;
        }
        let manifest = p.join("manifest.json");
        safety::no_symlinks(&manifest)?;
        if !manifest.is_file() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&manifest)?)?;
        if value["app"].as_str() != Some(&app.name) || value["build_id"].as_str() != Some(&name) {
            bail!("artifact ownership mismatch: {}", p.display());
        }
        artifact::checksum(
            &p.join("app.zip"),
            value["sha256"].as_str().context("missing artifact hash")?,
        )?;
        paths.push(p);
    }
    paths.sort();
    paths.reverse();
    let selected: Vec<_> = paths.into_iter().skip(keep).collect();
    if !dry_run {
        let trash = registry.state_dir.join("trash").join(&app.name);
        safety::no_symlinks(&trash)?;
        fs::create_dir_all(&trash)?;
        for p in &selected {
            let dest = trash.join(p.file_name().unwrap());
            if dest.exists() {
                bail!("trash destination exists: {}", dest.display());
            }
            fs::rename(p, &dest)?;
        }
    }
    Ok(selected)
}
