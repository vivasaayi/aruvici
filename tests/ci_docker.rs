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

fn checked(c: &mut Command) -> Output {
    let out = c.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}
fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    platform: PathBuf,
    tools: PathBuf,
}
impl Fixture {
    fn new(valid: bool) -> Self {
        let temp = safety::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let repo = root.join("repo");
        fs::create_dir(&repo).unwrap();
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname='oci-fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(repo.join("Dockerfile"), "FROM scratch\n").unwrap();
        let oci = root.join("valid.oci");
        if valid {
            let layout = root.join("layout");
            fs::create_dir_all(layout.join("blobs/sha256")).unwrap();
            fs::write(
                layout.join("oci-layout"),
                "{\"imageLayoutVersion\":\"1.0.0\"}",
            )
            .unwrap();
            fs::write(
                layout.join("index.json"),
                "{\"schemaVersion\":2,\"manifests\":[]}",
            )
            .unwrap();
            fs::write(layout.join("blobs/sha256/abc"), "blob").unwrap();
            checked(Command::new("/usr/bin/tar").current_dir(&layout).args([
                "-cf",
                oci.to_str().unwrap(),
                "oci-layout",
                "index.json",
                "blobs",
            ]));
        } else {
            fs::write(&oci, "not an OCI archive").unwrap();
        }
        checked(Command::new("/usr/bin/git").args(["init", "-q"]).arg(&repo));
        checked(
            Command::new("/usr/bin/git")
                .current_dir(&repo)
                .args(["add", "."]),
        );
        checked(Command::new("/usr/bin/git").current_dir(&repo).args([
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ]));
        let tools = root.join("tools");
        fs::create_dir(&tools).unwrap();
        script(
            &tools.join("gitleaks"),
            "printf '[]' > .aruvici-secrets.json",
        );
        script(&tools.join("cargo"), "exit 0");
        script(
            &tools.join("docker"),
            "test \"$1 $2\" = 'buildx build'\ncp \"$FIXTURE_OCI\" .aruvici-image.oci",
        );
        let config = PlatformConfig {
            state_dir: root.join("state"),
            targets: vec![Target {
                id: "api".into(),
                repository: repo.clone(),
                profile: "rust-docker@1".into(),
                root: ".".into(),
                inputs: BTreeMap::from([
                    ("cargo_manifest".into(), "Cargo.toml".into()),
                    ("dockerfile".into(), "Dockerfile".into()),
                    ("context".into(), ".".into()),
                    ("platform".into(), "linux/arm64".into()),
                    ("image".into(), "oci-fixture".into()),
                ]),
            }],
        };
        let platform = root.join("platform.toml");
        fs::write(&platform, toml::to_string(&config).unwrap()).unwrap();
        Self {
            _temp: temp,
            root,
            repo,
            platform,
            tools,
        }
    }
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_aruvici"))
            .args(["ci", "--platform"])
            .arg(&self.platform)
            .args(args)
            .env("PATH", format!("{}:/usr/bin:/bin", self.tools.display()))
            .env("FIXTURE_OCI", self.root.join("valid.oci"))
            .output()
            .unwrap()
    }
    fn json(&self, args: &[&str]) -> Value {
        let o = self.cli(args);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice(&o.stdout).unwrap()
    }
    fn approve(&self) {
        let p = self.json(&["plan", "api"]);
        assert_eq!(p["issues"], serde_json::json!([]));
        self.json(&["approve", "api", "--digest", p["digest"].as_str().unwrap()]);
    }
}
#[test]
fn docker_profile_exports_verified_oci_and_preserves_source() {
    let f = Fixture::new(true);
    f.approve();
    fs::write(f.repo.join("dirty"), "keep").unwrap();
    f.json(&["queue", "api", "--key", "oci-1"]);
    f.json(&["worker"]);
    let run = &f.json(&["runs"])[0];
    assert_eq!(run["status"], "passed", "{run:#}");
    let package = run["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["kind"] == "package")
        .unwrap();
    assert!(package["path"].as_str().unwrap().ends_with(".oci"));
    assert_eq!(
        artifact::sha256(Path::new(package["path"].as_str().unwrap())).unwrap(),
        package["sha256"].as_str().unwrap()
    );
    assert_eq!(fs::read_to_string(f.repo.join("dirty")).unwrap(), "keep");
    assert!(run["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["kind"] == "manifest"));
}
#[test]
fn malformed_oci_export_fails_after_build_without_promotable_artifact() {
    let f = Fixture::new(false);
    f.approve();
    f.json(&["queue", "api", "--key", "bad-oci"]);
    f.json(&["worker"]);
    let run = &f.json(&["runs"])[0];
    assert_eq!(run["status"], "failed", "{run:#}");
    assert!(run["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["kind"] == "package"));
    assert!(run["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["kind"] != "manifest"));
}
