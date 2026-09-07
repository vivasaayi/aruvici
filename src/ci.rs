//! Local pipeline orchestration. Source hosting is deliberately not involved.
use crate::{
    artifact, ci_exec,
    ci_store::Store,
    config::{App, Signing},
    process::{self, args, Executor, System},
    profiles::{Plan, PlatformConfig, Target},
    safety::{self, Lock},
};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

fn approval_path(config: &PlatformConfig, target: &str) -> PathBuf {
    config
        .state_dir
        .join("ci-approvals")
        .join(format!("{target}.json"))
}
pub fn approve(config: &PlatformConfig, id: &str, expected: &str) -> Result<Value> {
    let plan = config.target(id)?.plan()?;
    if expected != plan.digest {
        bail!("plan digest changed; inspect the plan again before approval");
    }
    let path = approval_path(config, id);
    safety::no_symlinks(&path)?;
    fs::create_dir_all(path.parent().unwrap())?;
    let _lock = Lock::acquire(&config.state_dir.join("locks/approval.lock"), false)?;
    if path.exists() {
        fs::rename(
            &path,
            path.with_extension(format!("{}.json", safety::stamp())),
        )?;
    }
    let value = json!({"target":id,"digest":expected,"approved_at":safety::stamp()});
    safety::create_new(&path, &serde_json::to_vec_pretty(&value)?)?;
    Ok(value)
}
pub fn approved(config: &PlatformConfig, plan: &Plan) -> Result<()> {
    let path = approval_path(config, &plan.target_id);
    safety::no_symlinks(&path)?;
    let value: Value = serde_json::from_slice(
        &fs::read(&path)
            .context("target is not approved; inspect ci plan then ci approve --digest DIGEST")?,
    )?;
    if value["digest"].as_str() != Some(&plan.digest) {
        bail!("approved pipeline changed; review and approve the new digest");
    }
    Ok(())
}
pub fn resolve(target: &Target, reference: &str) -> Result<String> {
    safety::absolute(&target.repository)?;
    let commit = System.output(
        &args(&[
            "git",
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ]),
        &target.repository,
    )?;
    if ![40, 64].contains(&commit.len()) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid resolved commit");
    }
    Ok(commit)
}
pub fn enqueue(
    config: &PlatformConfig,
    id: &str,
    reference: &str,
    key: &str,
    context: Value,
) -> Result<i64> {
    let target = config.target(id)?;
    let plan = target.plan()?;
    approved(config, &plan)?;
    if !plan.issues.is_empty() {
        bail!("target blocked: {}", plan.issues.join("; "));
    }
    if key.is_empty() || key.len() > 256 {
        bail!("idempotency key must contain 1–256 characters");
    }
    let commit = resolve(target, reference)?;
    Store::open(&config.state_dir)?.enqueue(id, &commit, &plan.digest, key, context)
}

/// One lock shared with the legacy release builder. Call repeatedly for service mode.
pub fn drain(config: &PlatformConfig) -> Result<()> {
    let _lock = Lock::acquire(&config.state_dir.join("locks/release.lock"), false)?;
    let store = Store::open(&config.state_dir)?;
    store.recover_interrupted()?;
    while !process::interrupted() {
        let Some(job) = store.claim()? else { break };
        let id = job["id"].as_i64().context("invalid queued run")?;
        if let Err(error) = execute_job(config, &store, &job) {
            let status = if store.cancelled(id)? || process::interrupted() {
                "cancelled"
            } else {
                "failed"
            };
            store.finish(id, status, Some(&format!("{error:#}")))?;
        }
    }
    Ok(())
}

fn execute_job(config: &PlatformConfig, store: &Store, job: &Value) -> Result<()> {
    let id = job["id"].as_i64().context("invalid run ID")?;
    let name = job["target"]
        .as_str()
        .or_else(|| job["target_id"].as_str())
        .context("missing target")?;
    let target = config.target(name)?;
    let plan = target.plan()?;
    if target.profile != "rust-tauri@1" {
        store.finish(
            id,
            "blocked",
            Some("execution adapter is not implemented for this profile"),
        )?;
        return Ok(());
    }
    approved(config, &plan)?;
    if job["plan_digest"].as_str() != Some(&plan.digest) {
        bail!("pipeline changed after queueing; enqueue a new reviewed run");
    }
    if !plan.issues.is_empty() {
        store.finish(id, "blocked", Some(&plan.issues.join("; ")))?;
        return Ok(());
    }
    let _app = Lock::acquire(
        &config.state_dir.join(format!("locks/{}.lock", target.id)),
        false,
    )?;
    let root = config.state_dir.join("ci-runs").join(id.to_string());
    safety::no_symlinks(&root)?;
    fs::create_dir_all(&root)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let work = safety::tempdir()?;
    let checkout = work.path().join("checkout");
    for stage in &plan.stages {
        store.stage(id, &stage.id, "pending", json!({"label":stage.label}))?;
    }
    run_command(
        store,
        id,
        "checkout",
        &args(&[
            "git",
            "clone",
            "--no-local",
            "--no-hardlinks",
            "--no-checkout",
            "--",
            target
                .repository
                .to_str()
                .context("invalid repository path")?,
            checkout.to_str().unwrap(),
        ]),
        work.path(),
        &root,
        &[],
        300,
    )?;
    let commit = job["commit"]
        .as_str()
        .or_else(|| job["commit_hash"].as_str())
        .context("missing commit")?;
    run_command(
        store,
        id,
        "revision",
        &args(&[
            "git",
            "-c",
            "core.hooksPath=/dev/null",
            "checkout",
            "--detach",
            commit,
        ]),
        &checkout,
        &root,
        &[],
        120,
    )?;
    if checkout.join(".gitmodules").exists() {
        bail!("submodules require a separately implemented source policy");
    }
    let base = checkout.join(&target.root);
    safety::no_symlinks(&base)?;
    for dir in [&checkout, &base] {
        for name in [".gitleaks.toml", ".gitleaksignore"] {
            if dir.join(name).exists() {
                bail!(
                    "project scanner overrides require an approved scanner-policy adapter: {name}"
                );
            }
        }
    }
    let env = vec![
        ("CI".into(), "true".into()),
        (
            "ARUVICI_TEST_DATA".into(),
            work.path().join("test-data").display().to_string(),
        ),
    ];
    fs::create_dir(work.path().join("test-data"))?;
    // Resolve readiness against the checked-out revision too, not just the live tree.
    let mut checked = target.clone();
    checked.repository = checkout.clone();
    let checked_plan = checked.plan()?;
    if !checked_plan.issues.is_empty() {
        bail!(
            "queued revision is not ready: {}",
            checked_plan.issues.join("; ")
        );
    }
    for stage in &plan.stages {
        let cwd = checkout.join(&stage.cwd);
        safety::no_symlinks(&cwd)?;
        let result = run_command(
            store,
            id,
            &stage.id,
            &stage.argv,
            &cwd,
            &root,
            &env,
            stage.timeout_seconds,
        );
        // Preserve diagnostic reports on failure as well as success.
        for (index, output) in stage.outputs.iter().enumerate() {
            let source = cwd.join(output);
            safety::no_symlinks(&source)?;
            if source.is_file() {
                let dest = root.join(format!(
                    "{}-{index}-{}",
                    stage.id,
                    source.file_name().unwrap().to_string_lossy()
                ));
                fs::copy(&source, &dest)?;
                store.artifact(id, &stage.id, "report", &dest)?;
            }
        }
        if let Err(e) = result {
            store.event(
                id,
                Some(&stage.id),
                "gate_result",
                json!({"passed":false,"reason":"required stage failed"}),
            )?;
            return Err(e);
        }
    }
    if store.cancelled(id)? || process::interrupted() {
        bail!("run cancelled before packaging");
    }
    store.stage(id, "verify-package", "running", json!({}))?;
    let packaging = (|| -> Result<()> {
        let bundle = base.join(
            target
                .inputs
                .get("artifact")
                .context("missing bundle artifact input")?,
        );
        let app = app_for(target, &bundle)?;
        let version = artifact::bundle(&app, &bundle, &System)?;
        let zip = root.join("app.zip");
        artifact::pack(&bundle, &zip)?;
        let hash = artifact::sha256(&zip)?;
        artifact::verify(&app, &zip, &hash, &System)?;
        let manifest = json!({"run_id":id,"target":target.id,"commit":commit,"profile":target.profile,"plan_digest":plan.digest,"sha256":hash,"version":version,"signature":"adhoc_verified"});
        safety::create_new(
            &root.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
        safety::create_new(
            &root.join("app.zip.sha256"),
            format!("{hash}  app.zip\n").as_bytes(),
        )?;
        for (file, kind) in [
            ("app.zip", "package"),
            ("manifest.json", "manifest"),
            ("app.zip.sha256", "checksum"),
        ] {
            store.artifact(id, "verify-package", kind, &root.join(file))?;
        }
        Ok(())
    })();
    store.stage(
        id,
        "verify-package",
        if packaging.is_ok() {
            "passed"
        } else {
            "failed"
        },
        json!({"error":packaging.as_ref().err().map(|e|format!("{e:#}"))}),
    )?;
    packaging?;
    if store.cancelled(id)? || process::interrupted() {
        bail!("run cancelled during packaging; artifacts are not promotable");
    }
    store.finish(id, "passed", None)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_command(
    store: &Store,
    id: i64,
    stage: &str,
    argv: &[String],
    cwd: &Path,
    root: &Path,
    env: &[(String, String)],
    timeout: u64,
) -> Result<()> {
    if store.cancelled(id)? || process::interrupted() {
        bail!("run cancelled");
    }
    store.stage(id, stage, "running", json!({"program":argv.first()}))?;
    let log = root.join(format!("{stage}.log"));
    let result = ci_exec::execute(argv, cwd, env, &log, Duration::from_secs(timeout), || {
        process::interrupted() || store.cancelled(id).unwrap_or(true)
    });
    if log.is_file() {
        store.artifact(id, stage, "log", &log)?;
    }
    match result {
        Ok(outcome) => {
            store.stage(id, stage, &outcome.status, serde_json::to_value(&outcome)?)?;
            if outcome.status != "passed" {
                bail!("stage {stage} {}", outcome.status);
            }
            Ok(())
        }
        Err(error) => {
            store.stage(id, stage, "failed", json!({"error":format!("{error:#}")}))?;
            Err(error)
        }
    }
}
fn app_for(target: &Target, bundle: &Path) -> Result<App> {
    Ok(App {
        name: target.id.clone(),
        repository: target.repository.clone(),
        github: String::new(),
        bundle_id: target
            .inputs
            .get("bundle_id")
            .context("bundle_id required")?
            .clone(),
        architecture: target
            .inputs
            .get("architecture")
            .cloned()
            .unwrap_or_else(|| "aarch64-apple-darwin".into()),
        artifact: bundle.into(),
        production_app: Path::new("/Applications")
            .join(bundle.file_name().context("bundle name missing")?),
        dev_data: PathBuf::new(),
        production_data: PathBuf::new(),
        isolation_acknowledged: true,
        static_dev_port: None,
        install_command: vec![],
        frontend_command: vec![],
        test_command: vec![],
        tauri_command: vec![],
        vite_command: vec![],
        signing: Signing {
            identity: "-".into(),
            team_id: None,
            notary_profile: None,
        },
    })
}
