use crate::safety::{absolute, overlaps, relative};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub state_dir: PathBuf,
    pub apps: Vec<App>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub name: String,
    pub repository: PathBuf,
    pub github: String,
    pub bundle_id: String,
    pub architecture: String,
    pub artifact: PathBuf,
    pub production_app: PathBuf,
    pub dev_data: PathBuf,
    pub production_data: PathBuf,
    #[serde(default)]
    pub isolation_acknowledged: bool,
    pub static_dev_port: Option<u16>,
    pub install_command: Vec<String>,
    pub frontend_command: Vec<String>,
    pub test_command: Vec<String>,
    pub tauri_command: Vec<String>,
    pub vite_command: Vec<String>,
    pub signing: Signing,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Signing {
    pub identity: String,
    pub team_id: Option<String>,
    pub notary_profile: Option<String>,
}

pub fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        && value != "."
        && value != ".."
}
impl Registry {
    pub fn load(path: &Path) -> Result<Self> {
        let registry: Self = toml::from_str(
            &fs::read_to_string(path)
                .with_context(|| format!("read registry {}", path.display()))?,
        )?;
        registry.validate()?;
        Ok(registry)
    }
    pub fn app(&self, name: &str) -> Result<&App> {
        self.apps
            .iter()
            .find(|a| a.name == name)
            .with_context(|| format!("unregistered application: {name}"))
    }
    pub fn validate(&self) -> Result<()> {
        absolute(&self.state_dir)?;
        if self.state_dir.starts_with("/Applications") {
            bail!("state directory cannot be inside /Applications");
        }
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        let mut ports = HashSet::new();
        let mut paths: Vec<(&Path, &str)> = vec![(&self.state_dir, "state")];
        for app in &self.apps {
            if !token(&app.name) || !names.insert(app.name.to_lowercase()) {
                bail!("invalid or duplicate app name: {}", app.name);
            }
            if !token(&app.bundle_id)
                || !app.bundle_id.contains('.')
                || !ids.insert(app.bundle_id.to_lowercase())
            {
                bail!("invalid or duplicate bundle identifier: {}", app.bundle_id);
            }
            let repo: Vec<_> = app.github.split('/').collect();
            if repo.len() != 2 || !repo.iter().all(|s| token(s)) {
                bail!("GitHub repository must be owner/repo");
            }
            if !["aarch64-apple-darwin", "x86_64-apple-darwin"].contains(&app.architecture.as_str())
            {
                bail!("unsupported architecture for {}", app.name);
            }
            if let Some(port) = app.static_dev_port {
                if port < 1024 || !ports.insert(port) {
                    bail!("invalid or duplicate static development port: {port}");
                }
            }
            for p in [
                &app.repository,
                &app.production_app,
                &app.dev_data,
                &app.production_data,
            ] {
                absolute(p)?;
            }
            if app.production_app.parent() != Some(Path::new("/Applications"))
                || app.production_app.extension().and_then(|s| s.to_str()) != Some("app")
                || app.production_app.file_stem().is_none_or(|s| s.is_empty())
            {
                bail!("production destination must be /Applications/Name.app");
            }
            relative(&app.artifact)?;
            if app.artifact.extension().and_then(|s| s.to_str()) != Some("app") {
                bail!("artifact must point to an .app bundle");
            }
            if app.artifact.file_name() != app.production_app.file_name() {
                bail!("artifact and production bundle names must match");
            }
            for (p, kind) in [
                (&app.repository, "repository"),
                (&app.production_app, "destination"),
                (&app.dev_data, "development data"),
                (&app.production_data, "production data"),
            ] {
                if let Some((other, other_kind)) =
                    paths.iter().find(|(other, _)| overlaps(p, other))
                {
                    bail!(
                        "{kind} {} overlaps {other_kind} {}",
                        p.display(),
                        other.display()
                    );
                }
                paths.push((p, kind));
            }
            for cmd in [
                &app.install_command,
                &app.frontend_command,
                &app.test_command,
                &app.tauri_command,
                &app.vite_command,
            ] {
                if cmd.is_empty() || cmd[0].is_empty() || cmd.iter().any(|s| s.contains('\0')) {
                    bail!("commands must be nonempty argv arrays without NUL");
                }
            }
            if app.signing.identity != "-"
                && (!app
                    .signing
                    .identity
                    .starts_with("Developer ID Application:")
                    || app.signing.team_id.as_ref().is_none_or(|s| !token(s)))
            {
                bail!("Developer ID signing requires an identity and team_id");
            }
            if app.signing.notary_profile.is_some() && app.signing.identity == "-" {
                bail!("notarization requires Developer ID signing");
            }
        }
        // A dev bundle ID must not collide with another app's release ID.
        for app in &self.apps {
            if ids.contains(&format!("{}.dev", app.bundle_id).to_lowercase()) {
                bail!("development bundle identifier collides with a production app");
            }
        }
        Ok(())
    }
}
impl App {
    pub fn require_isolation(&self) -> Result<()> {
        if !self.isolation_acknowledged {
            bail!("{}: adopt docs/onboarding.md data/window isolation, then set isolation_acknowledged = true", self.name);
        }
        Ok(())
    }
}
