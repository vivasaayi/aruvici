use aruvici::profiles::{PlatformConfig, Target};
use std::{collections::BTreeMap, fs, path::PathBuf};

fn target(repo: PathBuf) -> Target {
    Target {
        id: "desktop".into(),
        repository: repo,
        root: ".".into(),
        profile: "rust-tauri@1".into(),
        inputs: BTreeMap::from([
            ("bundle_id".into(), "com.example.desktop".into()),
            (
                "artifact".into(),
                "target/release/bundle/macos/Desktop.app".into(),
            ),
            ("isolation_acknowledged".into(), "true".into()),
        ]),
    }
}

#[test]
fn readiness_and_digest_are_stable() {
    let dir = aruvici::safety::tempdir().unwrap();
    let target = target(dir.path().to_owned());
    let missing = target.plan().unwrap();
    assert!(missing
        .issues
        .iter()
        .any(|s| s.contains("package-lock.json")));
    fs::create_dir(dir.path().join("src-tauri")).unwrap();
    for file in [
        "Cargo.toml",
        "package.json",
        "package-lock.json",
        "src-tauri/tauri.conf.json",
    ] {
        fs::write(dir.path().join(file), "{}").unwrap();
    }
    let ready = target.plan().unwrap();
    assert!(ready.issues.is_empty(), "{:?}", ready.issues);
    assert_eq!(ready.digest, missing.digest);
    assert_eq!(ready.stages[0].id, "secrets");
    assert!(ready.stages[0].argv.contains(&"--redact".into()));
    assert!(ready.stages.last().unwrap().argv[0].ends_with("node_modules/.bin/tauri"));
    let ids: Vec<_> = ready.stages.iter().map(|stage| stage.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "secrets",
            "fmt",
            "dependencies",
            "frontend",
            "clippy",
            "rust-tests",
            "package"
        ]
    );
    let package = ready.stages.last().unwrap();
    assert_eq!(
        package.argv.iter().filter(|arg| *arg == "--config").count(),
        1
    );
    let overrides: serde_json::Value = serde_json::from_str(package.argv.last().unwrap()).unwrap();
    assert!(overrides["build"]["devUrl"].is_null());
    assert_eq!(overrides["bundle"]["macOS"]["signingIdentity"], "-");
}

#[test]
fn reject_unsafe_paths_and_unknown_commands() {
    let dir = aruvici::safety::tempdir().unwrap();
    let original = target(dir.path().to_owned());
    for path in [
        "../outside.app",
        "/Applications/Desktop.app",
        "-options.app",
        "foo\nbar.app",
    ] {
        let mut target = original.clone();
        target.inputs.insert("artifact".into(), path.into());
        assert!(target.plan().is_err(), "accepted {path:?}");
    }
    let mut target = original.clone();
    target
        .inputs
        .insert("command".into(), "echo malicious".into());
    assert!(target.plan().is_err());
    let mut target = original;
    target.root = "../outside".into();
    assert!(target.plan().is_err());
}

#[test]
fn reject_symlinked_input() {
    let dir = aruvici::safety::tempdir().unwrap();
    std::os::unix::fs::symlink("/etc/passwd", dir.path().join("Cargo.toml")).unwrap();
    assert!(target(dir.path().into()).plan().is_err());
}

#[test]
fn approval_digest_changes_with_policy() {
    let dir = aruvici::safety::tempdir().unwrap();
    let mut target = target(dir.path().into());
    let before = target.plan().unwrap().digest;
    target
        .inputs
        .insert("architecture".into(), "x86_64-apple-darwin".into());
    assert_ne!(before, target.plan().unwrap().digest);
}

#[test]
fn unsupported_adapters_never_appear_ready() {
    let dir = aruvici::safety::tempdir().unwrap();
    for profile in ["rust-docker@1", "native-ios@1", "native-android@1"] {
        let target = Target {
            id: "app".into(),
            repository: dir.path().into(),
            root: ".".into(),
            profile: profile.into(),
            inputs: BTreeMap::new(),
        };
        let plan = target.plan().unwrap();
        assert!(plan
            .issues
            .iter()
            .any(|s| s.contains("adapter execution is not available")));
        assert!(plan.stages.len() > 1);
    }
}

#[test]
fn registry_rejects_duplicate_ids_and_overlapping_state() {
    let dir = aruvici::safety::tempdir().unwrap();
    let target = target(dir.path().join("repository"));
    let mut config = PlatformConfig {
        state_dir: dir.path().join("state"),
        targets: vec![target.clone(), target],
    };
    assert!(config.validate().is_err());
    config.targets.pop();
    assert!(config.validate().is_ok());
    config.state_dir = dir.path().join("repository/state");
    assert!(config.validate().is_err());
}
