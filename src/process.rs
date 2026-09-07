use anyhow::{bail, Context, Result};
use std::{
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
extern "C" fn signal_handler(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}
pub fn install_signals() {
    // SAFETY: handler only writes a lock-free atomic; it never allocates.
    unsafe {
        libc::signal(libc::SIGINT, signal_handler as *const () as usize);
        libc::signal(libc::SIGTERM, signal_handler as *const () as usize);
    }
}
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}
pub trait Executor {
    fn run(&self, argv: &[String], cwd: &Path, env: &[(String, String)]) -> Result<()>;
    fn output(&self, argv: &[String], cwd: &Path) -> Result<String>;
}
pub fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}
pub fn command(argv: &[String], cwd: &Path, env: &[(String, String)]) -> Result<Command> {
    let mut c = Command::new(argv.first().context("empty command")?);
    c.args(&argv[1..])
        .current_dir(cwd)
        .envs(env.iter().cloned())
        .stdin(Stdio::null());
    // Do not inherit ambient Apple secrets or signing overrides into the build.
    for key in [
        "APPLE_ID",
        "APPLE_PASSWORD",
        "APPLE_API_KEY",
        "APPLE_API_ISSUER",
        "APPLE_API_KEY_PATH",
        "APPLE_CERTIFICATE",
        "APPLE_CERTIFICATE_PASSWORD",
        "APPLE_SIGNING_IDENTITY",
        "TAURI_CONFIG",
        "CARGO_TARGET_DIR",
    ] {
        c.env_remove(key);
    }
    c.process_group(0);
    Ok(c)
}
pub struct ChildGroup(pub std::process::Child);
impl Drop for ChildGroup {
    fn drop(&mut self) {
        // SAFETY: children are started in their own process group; never signal ours.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGTERM);
        }
        std::thread::sleep(Duration::from_millis(150));
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}
pub struct System;
impl Executor for System {
    fn run(&self, argv: &[String], cwd: &Path, env: &[(String, String)]) -> Result<()> {
        eprintln!(
            "{}",
            serde_json::json!({"event":"command_start", "program":argv.first(), "cwd":cwd, "timestamp":crate::safety::stamp()})
        );
        let mut child = ChildGroup(
            command(argv, cwd, env)?
                .spawn()
                .with_context(|| format!("start {}", argv[0]))?,
        );
        loop {
            if interrupted() {
                bail!("command interrupted: {}", argv[0]);
            }
            if let Some(status) = child.0.try_wait()? {
                if !status.success() {
                    bail!("command {} failed with {status}", argv[0]);
                }
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn output(&self, argv: &[String], cwd: &Path) -> Result<String> {
        let out = command(argv, cwd, &[])?
            .output()
            .with_context(|| format!("start {}", argv[0]))?;
        if !out.status.success() {
            bail!(
                "{} failed ({}): {}",
                argv[0],
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    }
}
