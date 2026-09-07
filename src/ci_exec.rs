//! Bounded, redacted process output for local pipeline steps.
use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::Path,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub const LOG_LIMIT: usize = 10 * 1024 * 1024;
const LINE_LIMIT: usize = 64 * 1024;

#[derive(Debug, Serialize)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    pub status: String,
    pub truncated: bool,
}

fn sensitive(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "CREDENTIAL",
        "PRIVATE_KEY",
        "API_KEY",
        "AUTHORIZATION",
    ]
    .iter()
    .any(|part| key.contains(part))
}

struct Log {
    file: File,
    written: usize,
    truncated: bool,
    secrets: Vec<String>,
}
impl Log {
    fn line(&mut self, bytes: &[u8], oversized: bool) -> std::io::Result<()> {
        let mut line = if oversized {
            self.truncated = true;
            "[oversized output line suppressed]\n".to_owned()
        } else {
            String::from_utf8_lossy(bytes).into_owned()
        };
        for secret in &self.secrets {
            line = line.replace(secret, "[REDACTED]");
        }
        // Suppress credential-bearing lines even for credentials created by the command.
        let lower = line.to_ascii_lowercase();
        if lower.contains("bearer ")
            || lower.contains("ghp_")
            || lower.contains("github_pat_")
            || lower.contains("-----begin") && lower.contains("private key")
            || sensitive(&line) && (line.contains('=') || line.contains(':'))
        {
            line = "[credential-bearing output redacted]\n".to_owned();
        }
        let remaining = LOG_LIMIT.saturating_sub(self.written);
        // Never persist only a fragment of a redacted record.
        if line.len() > remaining {
            self.truncated = true;
            return Ok(());
        }
        self.file.write_all(line.as_bytes())?;
        self.written += line.len();
        Ok(())
    }
}

fn reader(
    mut pipe: impl Read + AsRawFd,
    log: Arc<Mutex<Log>>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    // SAFETY: the pipe is owned for the duration of this function.
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(std::io::Error::last_os_error()).context("configure output pipe");
    }
    let mut buffer = [0_u8; 8192];
    let mut line = Vec::new();
    let mut oversized = false;
    let mut private_key = false;
    let mut stopping_at = None;
    loop {
        if stop.load(Ordering::Relaxed) {
            let started = stopping_at.get_or_insert_with(Instant::now);
            if started.elapsed() > Duration::from_secs(1) {
                log.lock().unwrap().truncated = true;
                break;
            }
        }
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                for &byte in &buffer[..n] {
                    if line.len() < LINE_LIMIT && !oversized {
                        line.push(byte);
                    } else {
                        oversized = true;
                        line.clear();
                    }
                    if byte == b'\n' {
                        let text = String::from_utf8_lossy(&line);
                        if text.contains("-----BEGIN") && text.contains("PRIVATE KEY") {
                            private_key = true;
                        }
                        if !private_key {
                            log.lock().unwrap().line(&line, oversized)?;
                        }
                        if text.contains("-----END") && text.contains("PRIVATE KEY") {
                            log.lock()
                                .unwrap()
                                .line(b"[private key redacted]\n", false)?;
                            private_key = false;
                        }
                        line.clear();
                        oversized = false;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).context("read command output"),
        }
    }
    if (!line.is_empty() || oversized) && !private_key {
        log.lock().unwrap().line(&line, oversized)?;
    }
    Ok(())
}

/// Executes argv directly, merges redacted stdout/stderr records into a new log,
/// and terminates the command's process group on completion or cancellation.
/// The caller supplies a unique log path whose parent already exists.
pub fn execute(
    argv: &[String],
    cwd: &Path,
    env: &[(String, String)],
    log: &Path,
    timeout: Duration,
    cancel: impl Fn() -> bool,
) -> Result<Outcome> {
    crate::safety::no_symlinks(log)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(log)
        .context("create step log")?;
    let mut secrets: Vec<_> = std::env::vars()
        .chain(env.iter().cloned())
        .filter(|(key, value)| sensitive(key) && !value.is_empty())
        .flat_map(|(_, value)| {
            value
                .lines()
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    secrets.sort_by_key(|v| std::cmp::Reverse(v.len()));
    secrets.dedup();
    let log = Arc::new(Mutex::new(Log {
        file,
        written: 0,
        truncated: false,
        secrets,
    }));
    if cancel() {
        return Ok(Outcome {
            exit_code: None,
            status: "cancelled".into(),
            truncated: false,
        });
    }
    let mut child = crate::process::ChildGroup(
        crate::process::command(argv, cwd, env)?
            .env_remove("GITLEAKS_CONFIG")
            .env_remove("GITLEAKS_CONFIG_TOML")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start pipeline command")?,
    );
    let stdout = child.0.stdout.take().context("missing stdout pipe")?;
    let stderr = child.0.stderr.take().context("missing stderr pipe")?;
    let stop = Arc::new(AtomicBool::new(false));
    let out = {
        let log = log.clone();
        let stop = stop.clone();
        thread::spawn(move || reader(stdout, log, stop))
    };
    let err = {
        let log = log.clone();
        let stop = stop.clone();
        thread::spawn(move || reader(stderr, log, stop))
    };
    let started = Instant::now();
    let result: Result<(Option<i32>, &str)> = (|| loop {
        if cancel() || crate::process::interrupted() {
            return Ok((None, "cancelled"));
        }
        if started.elapsed() >= timeout {
            return Ok((None, "timed_out"));
        }
        if let Some(status) = child.0.try_wait().context("wait for pipeline command")? {
            return Ok((
                status.code(),
                if status.success() { "passed" } else { "failed" },
            ));
        }
        thread::sleep(Duration::from_millis(20));
    })();
    drop(child);
    stop.store(true, Ordering::Relaxed);
    let out_result = out
        .join()
        .map_err(|_| anyhow::anyhow!("stdout capture thread failed"));
    let err_result = err
        .join()
        .map_err(|_| anyhow::anyhow!("stderr capture thread failed"));
    out_result??;
    err_result??;
    let (exit_code, status) = result?;
    let log = log.lock().unwrap();
    log.file.sync_all().context("persist step log")?;
    Ok(Outcome {
        exit_code,
        status: status.into(),
        truncated: log.truncated,
    })
}
