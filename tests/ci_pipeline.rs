//! Full CLI boundary tests using a disposable Git repository and fixture tools.
#![cfg(target_os = "macos")]

use aruvici::{
    artifact,
    profiles::{PlatformConfig, Target},
    safety,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    repository: PathBuf,
    platform: PathBuf,
    tools: PathBuf,
    config: PlatformConfig,
}

fn checked(command: &mut Command) -> Output {
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "command failed: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    result
}

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

impl Fixture {
    fn new() -> Self {
        let temp = safety::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let repository = root.join("repository");
        let desktop = repository.join("desktop");
        fs::create_dir_all(desktop.join("src-tauri")).unwrap();
        for (name, content) in [
            ("Cargo.toml", "[package]\nname='fixture'\nversion='0.1.0'\n"),
            ("package.json", "{\"name\":\"fixture\"}"),
            ("package-lock.json", "{}"),
            ("src-tauri/tauri.conf.json", "{}"),
            ("tracked.txt", "committed state\n"),
        ] {
            fs::write(desktop.join(name), content).unwrap();
        }
        script(&desktop.join("fixture-tauri"), "test -f Cargo.toml\nprintf 'tauri\\n' >> \"$FIXTURE_CALLS\"\nmkdir -p output\ncp -R \"$FIXTURE_BUNDLE\" output/Fixture.app");
        checked(
            Command::new("/usr/bin/git")
                .args(["init", "-q"])
                .arg(&repository),
        );
        checked(
            Command::new("/usr/bin/git")
                .current_dir(&repository)
                .args(["add", "."]),
        );
        checked(Command::new("/usr/bin/git").current_dir(&repository).args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "fixture",
        ]));
        let tools = root.join("tools");
        fs::create_dir(&tools).unwrap();
        script(&tools.join("gitleaks"), "test -f Cargo.toml\nprintf 'gitleaks\\n' >> \"$FIXTURE_CALLS\"\nprintf '[]' > .aruvici-secrets.json\nif [ \"${FIXTURE_SCAN_FAIL:-0}\" = 1 ]; then printf '%s\\n' \"$PIPELINE_TEST_TOKEN\"; exit 3; fi");
        script(
            &tools.join("cargo"),
            "test -f Cargo.toml\nprintf 'cargo %s\\n' \"$1\" >> \"$FIXTURE_CALLS\"",
        );
        script(&tools.join("npm"), "test -f package.json\nprintf 'npm %s\\n' \"$1\" >> \"$FIXTURE_CALLS\"\nif [ \"$1\" = ci ]; then mkdir -p node_modules/.bin; cp fixture-tauri node_modules/.bin/tauri; fi");
        let config = PlatformConfig {
            state_dir: root.join("state"),
            targets: vec![Target {
                id: "fixture".into(),
                repository: repository.clone(),
                profile: "rust-tauri@1".into(),
                root: "desktop".into(),
                inputs: BTreeMap::from([
                    ("cargo_manifest".into(), "Cargo.toml".into()),
                    ("bundle_id".into(), "com.example.fixture".into()),
                    (
                        "architecture".into(),
                        if cfg!(target_arch = "aarch64") {
                            "aarch64-apple-darwin"
                        } else {
                            "x86_64-apple-darwin"
                        }
                        .into(),
                    ),
                    ("artifact".into(), "output/Fixture.app".into()),
                    ("isolation_acknowledged".into(), "true".into()),
                ]),
                preview: None,
            }],
        };
        let platform = root.join("platform.toml");
        fs::write(&platform, toml::to_string(&config).unwrap()).unwrap();
        Self {
            _temp: temp,
            root,
            repository,
            platform,
            tools,
            config,
        }
    }
    fn cli(&self, args: &[&str], scan_fail: bool) -> Output {
        Command::new(env!("CARGO_BIN_EXE_aruvici"))
            .args(["ci", "--platform"])
            .arg(&self.platform)
            .args(args)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.tools.display()),
            )
            .env("FIXTURE_CALLS", self.root.join("calls"))
            .env("FIXTURE_BUNDLE", self.root.join("prepared/Fixture.app"))
            .env("FIXTURE_SCAN_FAIL", if scan_fail { "1" } else { "0" })
            .env("PIPELINE_TEST_TOKEN", "fixture-secret-never-persist")
            .output()
            .unwrap()
    }
    fn json(&self, args: &[&str]) -> Value {
        let output = self.cli(args, false);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn approve(&self) {
        let plan = self.json(&["plan", "fixture"]);
        assert_eq!(plan["issues"], serde_json::json!([]));
        self.json(&[
            "approve",
            "fixture",
            "--digest",
            plan["digest"].as_str().unwrap(),
        ]);
    }
    fn prepare_bundle(&self) {
        let bundle = self.root.join("prepared/Fixture.app");
        fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        fs::write(bundle.join("Contents/Info.plist"), r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.example.fixture</string><key>CFBundleExecutable</key><string>fixture</string><key>CFBundleShortVersionString</key><string>1.2.3</string><key>CFBundleVersion</key><string>1</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#).unwrap();
        let source = self.root.join("fixture.c");
        fs::write(&source, "int main(void) { return 0; }\n").unwrap();
        checked(
            Command::new("/usr/bin/clang")
                .arg(&source)
                .arg("-o")
                .arg(bundle.join("Contents/MacOS/fixture")),
        );
        checked(
            Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-", "--timestamp=none"])
                .arg(bundle),
        );
    }
}

#[test]
fn local_cli_packages_signed_commit_preserves_dirty_tree_and_deduplicates() {
    let fixture = Fixture::new();
    fixture.prepare_bundle();
    fixture.approve();
    fs::write(
        fixture.repository.join("desktop/tracked.txt"),
        "dirty user edit\n",
    )
    .unwrap();
    fs::write(
        fixture.repository.join("untracked.txt"),
        "untracked user file\n",
    )
    .unwrap();
    let queued = fixture.json(&["queue", "fixture", "--key", "feature-1"]);
    assert_eq!(
        queued,
        fixture.json(&["queue", "fixture", "--key", "feature-1"])
    );
    fixture.json(&["worker"]);
    let runs = fixture.json(&["runs"]);
    assert_eq!(runs.as_array().unwrap().len(), 1);
    let run = &runs[0];
    assert_eq!(run["status"], "passed", "{run:#}");
    let files = run["artifacts"].as_array().unwrap();
    for kind in ["package", "manifest", "checksum", "log", "report"] {
        assert!(files.iter().any(|a| a["kind"] == kind), "missing {kind}");
    }
    for file in files {
        assert_eq!(
            artifact::sha256(Path::new(file["path"].as_str().unwrap())).unwrap(),
            file["sha256"].as_str().unwrap()
        );
    }
    let manifest = files.iter().find(|a| a["kind"] == "manifest").unwrap();
    let manifest: Value =
        serde_json::from_slice(&fs::read(manifest["path"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(manifest["version"], "1.2.3");
    assert_eq!(manifest["commit"], run["commit"]);
    assert_eq!(
        fs::read_to_string(fixture.repository.join("desktop/tracked.txt")).unwrap(),
        "dirty user edit\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.repository.join("untracked.txt")).unwrap(),
        "untracked user file\n"
    );
    let calls = fs::read_to_string(fixture.root.join("calls")).unwrap();
    assert!(calls.starts_with("gitleaks\n"));
    assert!(calls.ends_with("tauri\n"));
}

#[test]
fn required_scan_failure_stops_build_and_retains_redacted_diagnostics() {
    let fixture = Fixture::new();
    fixture.approve();
    fixture.json(&["queue", "fixture", "--key", "scan-failure"]);
    let output = fixture.cli(&["worker"], true);
    assert!(output.status.success());
    let runs = fixture.json(&["runs"]);
    let run = &runs[0];
    assert_eq!(run["status"], "failed", "{run:#}");
    assert_eq!(
        fs::read_to_string(fixture.root.join("calls")).unwrap(),
        "gitleaks\n"
    );
    let files = run["artifacts"].as_array().unwrap();
    assert!(!files.iter().any(|a| a["kind"] == "package"));
    let log = files
        .iter()
        .find(|a| a["kind"] == "log" && a["stage"] == "secrets")
        .unwrap();
    let text = fs::read_to_string(log["path"].as_str().unwrap()).unwrap();
    assert!(!text.contains("fixture-secret-never-persist"));
    assert!(text.contains("REDACTED"));
    assert!(run["stages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == "package" && s["status"] == "blocked"));
}

#[test]
fn changed_configuration_requires_new_approval() {
    let mut fixture = Fixture::new();
    fixture.approve();
    fixture.config.targets[0]
        .inputs
        .insert("bundle_id".into(), "com.example.changed".into());
    fs::write(&fixture.platform, toml::to_string(&fixture.config).unwrap()).unwrap();
    let result = fixture.cli(&["queue", "fixture", "--key", "changed"], false);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("approved pipeline changed"));
}
