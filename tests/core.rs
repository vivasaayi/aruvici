use anyhow::{bail, Result};
use aruvici::{
    artifact, build,
    config::Registry,
    deploy, dev,
    history::History,
    process::{args, Executor, System},
    profiles::{PlatformConfig, Preview, Target},
    safety::Lock,
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

fn temp() -> tempfile::TempDir {
    tempfile::tempdir_in("/private/tmp").unwrap()
}
fn registry(root: &Path) -> Registry {
    let mut r: Registry = toml::from_str(include_str!("../apps.example.toml")).unwrap();
    r.state_dir = root.join("state");
    r.apps[0].repository = root.join("repository");
    r.apps[0].dev_data = root.join("dev-data");
    r.apps[0].production_data = root.join("production-data");
    r.apps[0].isolation_acknowledged = true;
    r
}
#[test]
fn example_validates() {
    let r: Registry = toml::from_str(include_str!("../apps.example.toml")).unwrap();
    r.validate().unwrap();
}
#[test]
fn duplicate_names_ids_ports_destinations_rejected() {
    let t = temp();
    let r = registry(t.path());
    for field in ["name", "id", "port", "destination"] {
        let mut r: Registry = toml::from_str(&toml::to_string(&r).unwrap()).unwrap();
        r.apps[0].static_dev_port = Some(1420);
        let mut b = r.apps[0].clone();
        b.name = "other".into();
        b.bundle_id = "com.example.other".into();
        b.repository = t.path().join("repo2");
        b.dev_data = t.path().join("dev2");
        b.production_data = t.path().join("prod2");
        b.production_app = "/Applications/Other.app".into();
        b.artifact = "target/Other.app".into();
        b.static_dev_port = Some(1421);
        match field {
            "name" => b.name = "NOTES".into(),
            "id" => b.bundle_id = r.apps[0].bundle_id.to_uppercase(),
            "port" => b.static_dev_port = Some(1420),
            _ => {
                b.production_app = "/Applications/notes.app".into();
                b.artifact = "target/notes.app".into();
            }
        }
        r.apps.push(b);
        assert!(r.validate().is_err(), "{field}");
    }
}
#[test]
fn rejects_overlapping_data_traversal_and_unknown_fields() {
    let t = temp();
    let mut r = registry(t.path());
    r.apps[0].dev_data = r.apps[0].production_data.join("debug");
    assert!(r.validate().is_err());
    let mut r = registry(t.path());
    r.apps[0].artifact = "../Notes.app".into();
    assert!(r.validate().is_err());
    assert!(toml::from_str::<Registry>(&format!(
        "unexpected=true\n{}",
        include_str!("../apps.example.toml")
    ))
    .is_err());
}
#[test]
fn rejects_symlinked_path() {
    let t = temp();
    fs::create_dir(t.path().join("real")).unwrap();
    std::os::unix::fs::symlink(t.path().join("real"), t.path().join("alias")).unwrap();
    let mut r = registry(t.path());
    r.state_dir = t.path().join("alias/state");
    assert!(r.validate().is_err());
}
#[test]
fn preview_targets_are_confined_and_visibly_distinct() {
    let t = temp();
    let state = t.path().join("state");
    let target = Target {
        id: "notes-preview".into(),
        repository: t.path().join("repository"),
        profile: "rust-tauri@1".into(),
        root: PathBuf::from("."),
        inputs: [
            ("bundle_id", "com.example.notes.preview"),
            ("product_name", "Notes Preview"),
            ("artifact", "target/Notes Preview.app"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect(),
        preview: Some(Preview {
            destination: state.join("previews/notes-preview/current/Notes Preview.app"),
        }),
    };
    PlatformConfig {
        state_dir: state.clone(),
        targets: vec![target.clone()],
    }
    .validate()
    .unwrap();

    let mut outside = target.clone();
    outside.preview.as_mut().unwrap().destination = t.path().join("Notes Preview.app");
    assert!(PlatformConfig {
        state_dir: state.clone(),
        targets: vec![outside]
    }
    .validate()
    .is_err());

    let mut invisible = target;
    invisible
        .inputs
        .insert("product_name".into(), "Notes".into());
    assert!(!invisible.plan().unwrap().issues.is_empty());
}
#[test]
fn preview_installation_refuses_production_and_outside_paths_before_mutation() {
    let t = temp();
    let app = registry(t.path()).apps.remove(0);
    let root = t.path().join("state/previews");
    let zip = t.path().join("candidate.zip");
    fs::write(&zip, "not read for an invalid destination").unwrap();
    assert!(deploy::install_preview(
        &app,
        &zip,
        "not-a-hash",
        Path::new("/Applications/Notes Preview.app"),
        &root,
        &System,
    )
    .is_err());
    assert!(deploy::install_preview(
        &app,
        &zip,
        "not-a-hash",
        &t.path().join("outside/Notes Preview.app"),
        &root,
        &System,
    )
    .is_err());
    assert!(!root.exists());
}
#[test]
fn dynamic_ports_are_distinct_and_static_busy_fails() {
    let a = dev::allocate(None).unwrap();
    let b = dev::allocate(None).unwrap();
    assert_ne!(
        a.local_addr().unwrap().port(),
        b.local_addr().unwrap().port()
    );
    assert!(dev::allocate(Some(a.local_addr().unwrap().port())).is_err());
}
#[test]
fn lock_excludes_and_releases() {
    let t = temp();
    let p = t.path().join("build.lock");
    let a = Lock::acquire(&p, false).unwrap();
    assert!(Lock::acquire(&p, false).is_err());
    drop(a);
    assert!(Lock::acquire(&p, false).is_ok());
}
#[test]
fn lock_child_probe() {
    if let Some(p) = std::env::var_os("ARUVICI_TEST_LOCK") {
        assert!(Lock::acquire(Path::new(&p), false).is_err());
    }
}
#[test]
fn lock_excludes_separate_process() {
    let t = temp();
    let p = t.path().join("release.lock");
    let _guard = Lock::acquire(&p, false).unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_child_probe"])
        .env("ARUVICI_TEST_LOCK", &p)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}
#[test]
fn checksum_detects_tampering() {
    let t = temp();
    let p = t.path().join("app.zip");
    fs::write(&p, b"abc").unwrap();
    let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    artifact::checksum(&p, hash).unwrap();
    fs::write(&p, b"abd").unwrap();
    assert!(artifact::checksum(&p, hash).is_err());
    assert!(artifact::checksum(&p, "bad").is_err());
}
fn zip_entry(path: &Path, name: &str) {
    let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
    z.start_file(name, zip::write::SimpleFileOptions::default())
        .unwrap();
    z.write_all(b"contents").unwrap();
    z.finish().unwrap();
}
#[test]
fn zip_rejects_traversal_wrong_root_and_symlinks() {
    let t = temp();
    let p = t.path().join("a.zip");
    for name in [
        "../escape",
        "Other.app/file",
        "Notes.app/../../escape",
        "Notes.app/dir\\evil",
    ] {
        zip_entry(&p, name);
        assert!(
            artifact::extract(&p, "Notes.app", t.path()).is_err(),
            "{name}"
        );
    }
    let mut z = zip::ZipWriter::new(fs::File::create(&p).unwrap());
    z.add_symlink(
        "Notes.app/link",
        "/etc/passwd",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    z.finish().unwrap();
    assert!(artifact::extract(&p, "Notes.app", t.path()).is_err());
}
#[test]
fn zip_rejects_case_aliases() {
    let t = temp();
    let p = t.path().join("a.zip");
    let mut z = zip::ZipWriter::new(fs::File::create(&p).unwrap());
    for name in ["Notes.app/File", "Notes.app/file"] {
        z.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(b"x").unwrap();
    }
    z.finish().unwrap();
    assert!(artifact::extract(&p, "Notes.app", t.path()).is_err());
}
struct DeployFixture {
    _t: tempfile::TempDir,
    dest: PathBuf,
    stage: PathBuf,
    backup: PathBuf,
    journal: PathBuf,
    data: PathBuf,
}
fn deployment(old: bool) -> DeployFixture {
    let t = temp();
    let dest = t.path().join("Notes.app");
    let stage = t.path().join("staged.app");
    let backup = t.path().join("backup.app");
    let journal = t.path().join("transaction.json");
    let data = t.path().join("production.sqlite");
    if old {
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("version"), "old").unwrap();
    }
    fs::create_dir(&stage).unwrap();
    fs::write(stage.join("version"), "new").unwrap();
    fs::write(&data, "production database sentinel").unwrap();
    DeployFixture {
        _t: t,
        dest,
        stage,
        backup,
        journal,
        data,
    }
}
#[test]
fn deployment_success_preserves_backup_and_data() {
    let f = deployment(true);
    deploy::transaction(
        &f.dest,
        &f.stage,
        &f.backup,
        &f.journal,
        |_| Ok(()),
        || Ok(false),
    )
    .unwrap();
    assert_eq!(fs::read_to_string(f.dest.join("version")).unwrap(), "new");
    assert_eq!(fs::read_to_string(f.backup.join("version")).unwrap(), "old");
    assert!(!f.journal.exists());
    assert_eq!(
        fs::read_to_string(f.data).unwrap(),
        "production database sentinel"
    );
}
#[test]
fn failed_post_install_restores_previous_app_and_retains_rejected_app() {
    let f = deployment(true);
    let result = deploy::transaction(
        &f.dest,
        &f.stage,
        &f.backup,
        &f.journal,
        |p| {
            if p == f.dest && fs::read_to_string(p.join("version"))? == "new" {
                bail!("injected signature failure");
            }
            Ok(())
        },
        || Ok(false),
    );
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(f.dest.join("version")).unwrap(), "old");
    assert_eq!(fs::read_to_string(f.stage.join("version")).unwrap(), "new");
    assert!(!f.journal.exists());
    assert_eq!(
        fs::read_to_string(f.data).unwrap(),
        "production database sentinel"
    );
}
#[test]
fn failed_first_install_returns_to_absent_state() {
    let f = deployment(false);
    assert!(deploy::transaction(
        &f.dest,
        &f.stage,
        &f.backup,
        &f.journal,
        |p| {
            if p == f.dest {
                bail!("post check failed");
            }
            Ok(())
        },
        || Ok(false)
    )
    .is_err());
    assert!(!f.dest.exists());
    assert!(f.stage.exists());
    assert!(!f.journal.exists());
}
#[test]
fn running_app_or_inspection_failure_refuses_without_mutation() {
    let f = deployment(true);
    assert!(deploy::transaction(
        &f.dest,
        &f.stage,
        &f.backup,
        &f.journal,
        |_| Ok(()),
        || Ok(true)
    )
    .is_err());
    assert!(deploy::transaction(
        &f.dest,
        &f.stage,
        &f.backup,
        &f.journal,
        |_| Ok(()),
        || bail!("process inspection denied")
    )
    .is_err());
    assert_eq!(fs::read_to_string(f.dest.join("version")).unwrap(), "old");
    assert!(!f.backup.exists());
    assert!(!f.journal.exists());
}
#[test]
fn interrupted_transaction_blocks_further_deployment() {
    let f = deployment(true);
    fs::write(&f.journal, "{}").unwrap();
    assert!(deploy::transaction(
        &f.dest,
        &f.stage,
        &f.backup,
        &f.journal,
        |_| Ok(()),
        || Ok(false)
    )
    .is_err());
    assert!(!f.backup.exists());
}
#[test]
fn staging_validation_failure_keeps_installed_app() {
    let f = deployment(true);
    assert!(deploy::transaction(
        &f.dest,
        &f.stage,
        &f.backup,
        &f.journal,
        |_| bail!("invalid bundle"),
        || Ok(false)
    )
    .is_err());
    assert!(!f.journal.exists());
    assert!(f.dest.exists());
}
#[test]
fn command_failure_and_missing_executable_propagate() {
    assert!(System
        .run(&args(&["/usr/bin/false"]), Path::new("/"), &[])
        .is_err());
    assert!(System
        .run(&args(&["/no/such/executable"]), Path::new("/"), &[])
        .is_err());
    assert!(System
        .output(&args(&["/usr/bin/false"]), Path::new("/"))
        .is_err());
}
#[test]
fn sqlite_history_and_queue_survive_reopen() {
    let t = temp();
    let state = t.path().join("state");
    let h = History::open(&state).unwrap();
    h.event(
        "notes",
        "build",
        "failed",
        serde_json::json!({"commit":"abc","tests":"failed","error":"example"}),
    )
    .unwrap();
    let id = h.enqueue("notes", "abc").unwrap();
    drop(h);
    let h = History::open(&state).unwrap();
    assert_eq!(h.claim().unwrap().unwrap().0, id);
    assert!(h.claim().unwrap().is_none());
    assert_eq!(h.queue().unwrap()[0]["status"], "interrupted");
    assert_eq!(h.list(None).unwrap()[0]["status"], "failed");
}
struct FailingExecutor;
impl Executor for FailingExecutor {
    fn run(&self, _: &[String], _: &Path, _: &[(String, String)]) -> Result<()> {
        bail!("injected command failure")
    }
    fn output(&self, argv: &[String], _: &Path) -> Result<String> {
        if argv.iter().any(|s| s == "get-url") {
            Ok("https://github.com/YOUR_ORG/notes.git".into())
        } else if argv.iter().any(|s| s == "status") {
            Ok(String::new())
        } else {
            Ok("a".repeat(40))
        }
    }
}
#[test]
fn build_command_failure_is_recorded_and_releases_locks() {
    let t = temp();
    let r = registry(t.path());
    let app = &r.apps[0];
    assert!(build::run(&r, app, &app.repository, "HEAD", false, &FailingExecutor).is_err());
    let h = History::open(&r.state_dir).unwrap();
    assert_eq!(h.list(None).unwrap()[0]["status"], "failed");
    assert!(Lock::acquire(&r.state_dir.join("locks/release.lock"), false).is_ok());
}
#[test]
fn cleanup_is_previewable_and_recoverable() {
    let t = temp();
    let r = registry(t.path());
    let root = r.state_dir.join("artifacts/notes/100-1");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("app.zip"), "archive").unwrap();
    let hash = artifact::sha256(&root.join("app.zip")).unwrap();
    fs::write(
        root.join("manifest.json"),
        serde_json::json!({"app":"notes","build_id":"100-1","sha256":hash}).to_string(),
    )
    .unwrap();
    fs::create_dir_all(&r.apps[0].production_data).unwrap();
    fs::write(
        r.apps[0].production_data.join("database.sqlite"),
        "important",
    )
    .unwrap();
    assert_eq!(
        build::clean(&r, &r.apps[0], 0, true).unwrap(),
        vec![root.clone()]
    );
    assert!(root.exists());
    build::clean(&r, &r.apps[0], 0, false).unwrap();
    assert!(!root.exists());
    assert!(r.state_dir.join("trash/notes/100-1/app.zip").exists());
    assert!(r.apps[0].production_data.join("database.sqlite").exists());
}
#[test]
fn generated_workflow_has_private_gate_no_pr_or_deploy_and_correct_labels() {
    let t = temp();
    let r = registry(t.path());
    let w = aruvici::workflow::generate(&r.apps[0]);
    assert!(w.contains("[self-hosted, macOS, ARM64, tauri]"));
    assert!(w.contains("github.event.repository.private == true"));
    assert!(!w.contains("pull_request:"));
    assert!(w.contains("cancel-in-progress: false"));
    assert!(!w.contains(" deploy "));
    assert!(!w.contains("APP_NAME"));
}
#[test]
fn cli_validates_and_returns_nonzero_for_unknown_app() {
    let t = temp();
    let r = registry(t.path());
    let config = t.path().join("apps.toml");
    fs::write(&config, toml::to_string(&r).unwrap()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_aruvici"))
        .arg("--config")
        .arg(&config)
        .arg("validate")
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = Command::new(env!("CARGO_BIN_EXE_aruvici"))
        .arg("--config")
        .arg(&config)
        .args(["build", "unknown"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unregistered application"));
}

#[cfg(target_os = "macos")]
#[test]
fn real_macos_adhoc_signature_zip_roundtrip_and_wrong_bundle_rejection() {
    let t = temp();
    let r = registry(t.path());
    let app = &r.apps[0];
    let bundle = t.path().join("Notes.app");
    fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    fs::copy(
        fixture.join("Info.plist"),
        bundle.join("Contents/Info.plist"),
    )
    .unwrap();
    System
        .run(
            &args(&[
                "/usr/bin/clang",
                fixture.join("main.c").to_str().unwrap(),
                "-o",
                bundle.join("Contents/MacOS/notes").to_str().unwrap(),
            ]),
            t.path(),
            &[],
        )
        .unwrap();
    System
        .run(
            &args(&[
                "/usr/bin/codesign",
                "--force",
                "--sign",
                "-",
                bundle.to_str().unwrap(),
            ]),
            t.path(),
            &[],
        )
        .unwrap();
    assert_eq!(artifact::bundle(app, &bundle, &System).unwrap(), "1.2.3");
    let zip = t.path().join("app.zip");
    artifact::pack(&bundle, &zip).unwrap();
    let hash = artifact::sha256(&zip).unwrap();
    assert_eq!(
        artifact::verify(app, &zip, &hash, &System).unwrap(),
        "1.2.3"
    );
    let mut wrong = app.clone();
    wrong.bundle_id = "com.example.wrong".into();
    assert!(artifact::verify(&wrong, &zip, &hash, &System).is_err());
    fs::write(bundle.join("Contents/MacOS/notes"), "tampered").unwrap();
    assert!(artifact::bundle(app, &bundle, &System).is_err());
}
