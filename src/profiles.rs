//! Fixed, reviewable local build profiles. Project inputs are data, never shell commands.
use crate::{config::token, safety};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformConfig {
    pub state_dir: PathBuf,
    pub targets: Vec<Target>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub id: String,
    pub repository: PathBuf,
    pub profile: String,
    #[serde(default = "dot")]
    pub root: PathBuf,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
}
fn dot() -> PathBuf {
    PathBuf::from(".")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub target_id: String,
    pub profile: String,
    pub digest: String,
    pub stages: Vec<Stage>,
    pub issues: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stage {
    pub id: String,
    pub label: String,
    pub argv: Vec<String>,
    /// Relative to the isolated checkout, never the user's live repository.
    pub cwd: PathBuf,
    pub timeout_seconds: u64,
    /// Relative to cwd. The executor validates these again before collection.
    pub outputs: Vec<String>,
}

impl PlatformConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = toml::from_str(
            &fs::read_to_string(path)
                .with_context(|| format!("read platform configuration {}", path.display()))?,
        )?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        safety::absolute(&self.state_dir)?;
        if self.state_dir.starts_with("/Applications") {
            bail!("platform state cannot be inside /Applications");
        }
        let mut ids = HashSet::new();
        for target in &self.targets {
            if !ids.insert(target.id.to_lowercase()) {
                bail!("duplicate target ID: {}", target.id);
            }
            if safety::overlaps(&self.state_dir, &target.repository) {
                bail!("platform state must not overlap target repository");
            }
            target.plan()?;
        }
        Ok(())
    }
    pub fn target(&self, id: &str) -> Result<&Target> {
        self.targets
            .iter()
            .find(|target| target.id == id)
            .with_context(|| format!("unregistered target: {id}"))
    }
}

fn relative(path: &str, allow_dot: bool) -> Result<()> {
    if path.chars().any(char::is_control) || path.starts_with('-') || path.contains('\\') {
        bail!("unsafe input path: {path:?}");
    }
    if allow_dot && path == "." {
        return Ok(());
    }
    safety::relative(Path::new(path))
}

impl Target {
    pub fn plan(&self) -> Result<Plan> {
        if !token(&self.id) {
            bail!("invalid target ID: {}", self.id);
        }
        safety::absolute(&self.repository)?;
        let root = self.root.to_str().context("target root must be UTF-8")?;
        relative(root, true)?;
        safety::no_symlinks(&self.repository.join(&self.root))?;
        let allowed: &[&str] = match self.profile.as_str() {
            "rust-tauri@1" => &[
                "cargo_manifest",
                "frontend_root",
                "tauri_config",
                "bundle_id",
                "architecture",
                "artifact",
                "isolation_acknowledged",
            ],
            "rust-docker@1" => &[
                "cargo_manifest",
                "dockerfile",
                "context",
                "platform",
                "image",
            ],
            "native-ios@1" => &["project", "scheme", "configuration", "destination"],
            "native-android@1" => &["wrapper", "module", "variant"],
            _ => bail!("unknown profile: {}", self.profile),
        };
        for (key, value) in &self.inputs {
            if !allowed.contains(&key.as_str()) {
                bail!("unknown {} input: {key}", self.profile);
            }
            if value.is_empty() || value.chars().any(char::is_control) || value.starts_with('-') {
                bail!("invalid value for input {key}");
            }
        }
        let mut plan = Plan {
            target_id: self.id.clone(),
            profile: self.profile.clone(),
            digest: String::new(),
            stages: Vec::new(),
            issues: Vec::new(),
        };
        if !self.repository.is_dir() {
            plan.issues.push(format!(
                "repository does not exist: {}",
                self.repository.display()
            ));
        }
        let base = self.repository.join(&self.root);
        let value = |key: &str, default: &str| {
            self.inputs
                .get(key)
                .cloned()
                .unwrap_or_else(|| default.into())
        };
        let check_file = |path: &str, issues: &mut Vec<String>| -> Result<()> {
            relative(path, false)?;
            let resolved = base.join(path);
            safety::no_symlinks(&resolved)?;
            if !resolved.is_file() {
                issues.push(format!("required file missing: {path}"));
            }
            Ok(())
        };
        let mut stages = Vec::new();
        let mut stage =
            |id: &str, label: &str, args: Vec<String>, cwd: PathBuf, outputs: Vec<String>| {
                stages.push(Stage {
                    id: id.into(),
                    label: label.into(),
                    argv: args,
                    cwd,
                    timeout_seconds: if id == "package" { 3600 } else { 900 },
                    outputs,
                });
            };
        stage(
            "secrets",
            "Secret scan (redacted)",
            strings(&[
                "gitleaks",
                "dir",
                ".",
                "--redact",
                "--report-format",
                "json",
                "--report-path",
                ".aruvici-secrets.json",
            ]),
            self.root.clone(),
            vec![".aruvici-secrets.json".into()],
        );
        if self.profile.starts_with("rust-") {
            let manifest = value("cargo_manifest", "Cargo.toml");
            check_file(&manifest, &mut plan.issues)?;
            for (id, label, args) in [
                (
                    "fmt",
                    "Rust formatting",
                    vec![
                        "cargo",
                        "fmt",
                        "--manifest-path",
                        &manifest,
                        "--",
                        "--check",
                    ],
                ),
                (
                    "clippy",
                    "Rust lint",
                    vec![
                        "cargo",
                        "clippy",
                        "--locked",
                        "--manifest-path",
                        &manifest,
                        "--all-targets",
                        "--",
                        "-D",
                        "warnings",
                    ],
                ),
                (
                    "rust-tests",
                    "Rust tests",
                    vec!["cargo", "test", "--locked", "--manifest-path", &manifest],
                ),
            ] {
                stage(id, label, strings(&args), self.root.clone(), vec![]);
            }
        }
        match self.profile.as_str() {
            "rust-tauri@1" => {
                let frontend = value("frontend_root", ".");
                relative(&frontend, true)?;
                for name in ["package.json", "package-lock.json"] {
                    let path = if frontend == "." {
                        PathBuf::from(name)
                    } else {
                        Path::new(&frontend).join(name)
                    };
                    check_file(&path.to_string_lossy(), &mut plan.issues)?;
                }
                let config = value("tauri_config", "src-tauri/tauri.conf.json");
                if config != "src-tauri/tauri.conf.json" {
                    bail!("rust-tauri@1 currently requires tauri_config = src-tauri/tauri.conf.json under target root");
                }
                check_file(&config, &mut plan.issues)?;
                let bundle = value("bundle_id", "");
                if !token(&bundle) || !bundle.contains('.') {
                    plan.issues
                        .push("required input bundle_id must be a dotted bundle identifier".into());
                }
                let arch = value("architecture", "aarch64-apple-darwin");
                if !["aarch64-apple-darwin", "x86_64-apple-darwin"].contains(&arch.as_str()) {
                    bail!("unsupported Tauri architecture: {arch}");
                }
                let artifact = value("artifact", "");
                if artifact.is_empty() {
                    plan.issues.push(
                        "required input artifact: relative path to the generated .app bundle"
                            .into(),
                    );
                } else {
                    relative(&artifact, false)?;
                    if Path::new(&artifact).extension().and_then(|s| s.to_str()) != Some("app") {
                        bail!("Tauri artifact must be an .app bundle");
                    }
                }
                if value("isolation_acknowledged", "false") != "true" {
                    plan.issues.push("adopt development/test/production data isolation, then set isolation_acknowledged = 'true'".into());
                }
                let frontend_cwd = self.root.join(&frontend);
                stage(
                    "dependencies",
                    "Install locked frontend dependencies",
                    strings(&["npm", "ci"]),
                    frontend_cwd.clone(),
                    vec![],
                );
                stage(
                    "frontend",
                    "Frontend build (project build script)",
                    strings(&["npm", "run", "build"]),
                    frontend_cwd.clone(),
                    vec![],
                );
                // Paths passed to Tauri are absolute in the eventual checkout via a root-relative cwd.
                // Run the CLI at target root; npm resolves the checked-in frontend binary explicitly.
                let executable = Path::new(&frontend).join("node_modules/.bin/tauri");
                let executable = format!("./{}", executable.display());
                let override_json = serde_json::json!({"identifier": bundle, "build": {"beforeBuildCommand": "", "beforeDevCommand": "", "devUrl": null}, "bundle": {"macOS": {"signingIdentity": "-"}}}).to_string();
                stage(
                    "package",
                    "Build ad-hoc signed macOS app",
                    vec![
                        executable,
                        "build".into(),
                        "--target".into(),
                        arch,
                        "--bundles".into(),
                        "app".into(),
                        "--config".into(),
                        override_json,
                    ],
                    self.root.clone(),
                    if artifact.is_empty() {
                        vec![]
                    } else {
                        vec![artifact]
                    },
                );
            }
            "rust-docker@1" => {
                let dockerfile = value("dockerfile", "Dockerfile");
                check_file(&dockerfile, &mut plan.issues)?;
                let context = value("context", ".");
                relative(&context, true)?;
                safety::no_symlinks(&base.join(&context))?;
                if !base.join(&context).is_dir() {
                    plan.issues
                        .push(format!("build context missing: {context}"));
                }
                let platform = value("platform", "linux/arm64");
                if !["linux/arm64", "linux/amd64"].contains(&platform.as_str()) {
                    bail!("unsupported Docker platform");
                }
                let image = value("image", &format!("aruvici-{}", self.id));
                if !token(&image) {
                    bail!("image must be a simple local image name");
                }
                stage(
                    "package",
                    "Build and export OCI image",
                    vec![
                        "docker".into(),
                        "buildx".into(),
                        "build".into(),
                        "--platform".into(),
                        platform,
                        "--file".into(),
                        dockerfile,
                        "--tag".into(),
                        image,
                        "--output".into(),
                        "type=oci,dest=.aruvici-image.oci".into(),
                        context,
                    ],
                    self.root.clone(),
                    vec![".aruvici-image.oci".into()],
                );
            }
            "native-ios@1" => {
                let project = value("project", "App.xcodeproj");
                relative(&project, false)?;
                safety::no_symlinks(&base.join(&project))?;
                if !base.join(&project).is_dir() {
                    plan.issues
                        .push(format!("Xcode project missing: {project}"));
                }
                let scheme = value("scheme", "");
                if !token(&scheme) {
                    plan.issues
                        .push("required input scheme must be a simple Xcode scheme name".into());
                }
                let configuration = value("configuration", "Debug");
                if !["Debug", "Release"].contains(&configuration.as_str()) {
                    bail!("configuration must be Debug or Release");
                }
                let destination = value("destination", "platform=iOS Simulator,name=iPhone 16");
                stage(
                    "ios-tests",
                    "iOS simulator tests",
                    vec![
                        "xcodebuild".into(),
                        "-project".into(),
                        project,
                        "-scheme".into(),
                        scheme,
                        "-configuration".into(),
                        configuration,
                        "-destination".into(),
                        destination,
                        "-resultBundlePath".into(),
                        "TestResults.xcresult".into(),
                        "test".into(),
                    ],
                    self.root.clone(),
                    vec!["TestResults.xcresult".into()],
                );
            }
            "native-android@1" => {
                let wrapper = value("wrapper", "gradlew");
                check_file(&wrapper, &mut plan.issues)?;
                let module = value("module", "app");
                let variant = value("variant", "Debug");
                if !token(&module) || !variant.bytes().all(|b| b.is_ascii_alphanumeric()) {
                    bail!("invalid Android module or variant");
                }
                let executable = format!("./{wrapper}");
                for (id, task) in [
                    ("android-lint", "lint"),
                    ("android-tests", "test"),
                    ("package", "assemble"),
                ] {
                    stage(
                        id,
                        task,
                        vec![
                            executable.clone(),
                            "--no-daemon".into(),
                            format!(":{module}:{task}{variant}"),
                        ],
                        self.root.clone(),
                        vec![],
                    );
                }
            }
            _ => unreachable!(),
        }
        if !matches!(self.profile.as_str(), "rust-tauri@1" | "rust-docker@1") {
            plan.issues.push(format!(
                "{} adapter execution is not available yet; this profile is planning-only",
                self.profile
            ));
        }
        if self.profile == "rust-tauri@1" {
            let order = [
                "secrets",
                "fmt",
                "dependencies",
                "frontend",
                "clippy",
                "rust-tests",
                "package",
            ];
            stages.sort_by_key(|stage| {
                order
                    .iter()
                    .position(|id| *id == stage.id)
                    .unwrap_or(usize::MAX)
            });
        }
        plan.stages = stages;
        plan.digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(self, &plan.stages))?)
        );
        Ok(plan)
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}
