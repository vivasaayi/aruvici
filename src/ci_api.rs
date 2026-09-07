//! Private Unix-socket API: one bounded newline-delimited JSON request per connection.
use crate::{ci, ci_store::Store, process, profiles::PlatformConfig, safety};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Duration,
};

pub fn socket_path(config: &PlatformConfig) -> PathBuf {
    config.state_dir.join("ci.sock")
}
pub fn dispatch(config: &PlatformConfig, request: Value) -> Result<Value> {
    let store = Store::open(&config.state_dir)?;
    let method = request["method"].as_str().context("missing method")?;
    let id = || request["id"].as_i64().context("missing numeric id");
    let target = || request["target"].as_str().context("missing target");
    match method {
        "health" => Ok(json!({"status":"online","protocol":1})),
        "targets" => config
            .targets
            .iter()
            .map(|t| Ok(json!({"id":t.id,"profile":t.profile,"plan":t.plan()?})))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        "plan" => Ok(serde_json::to_value(config.target(target()?)?.plan()?)?),
        "runs" => store.list(),
        "run" => store.run(id()?),
        "events" => store.events(id()?, request["after"].as_i64().unwrap_or(0)),
        "enqueue" => Ok(
            json!({"run_id":ci::enqueue(config,target()?,request["commit"].as_str().context("missing commit")?,request["key"].as_str().context("missing idempotency key")?,request.get("context").cloned().unwrap_or(Value::Null))?}),
        ),
        "cancel" => {
            store.cancel(id()?)?;
            store.run(id()?)
        }
        "artifact" => store.artifact_info(id()?),
        "artifact_chunk" => {
            let info = store.artifact_info(id()?)?;
            let path = Path::new(info["path"].as_str().context("artifact path missing")?);
            safety::absolute(path)?;
            if !path.starts_with(config.state_dir.join("ci-runs")) {
                bail!("artifact outside run storage");
            }
            let mut file = fs::File::open(path)?;
            let offset = request["offset"].as_u64().unwrap_or(0);
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = vec![0; 65536];
            let n = file.read(&mut bytes)?;
            bytes.truncate(n);
            // Byte array avoids UTF-8 coercion and extra base64 dependency.
            Ok(
                json!({"bytes":bytes,"offset":offset,"next_offset":offset+n as u64,"eof":offset+n as u64>=info["size"].as_u64().unwrap_or(0),"sha256":info["sha256"]}),
            )
        }
        "logs" => {
            let run = id()?;
            let stage = request["stage"].as_str().context("missing stage")?;
            if !crate::config::token(stage) {
                bail!("invalid stage");
            }
            store.run(run)?;
            let path = config
                .state_dir
                .join("ci-runs")
                .join(run.to_string())
                .join(format!("{stage}.log"));
            safety::no_symlinks(&path)?;
            let mut file = fs::File::open(path)?;
            let offset = request["offset"].as_u64().unwrap_or(0);
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = vec![0; 65536];
            let n = file.read(&mut bytes)?;
            Ok(json!({"text":String::from_utf8_lossy(&bytes[..n]),"next_offset":offset+n as u64}))
        }
        _ => bail!("unsupported method; approvals and deployments are not exposed by the API"),
    }
}

pub fn request(socket: &Path, value: Value) -> Result<Value> {
    let mut stream =
        UnixStream::connect(socket).context("connect local CI service; start ci serve first")?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    serde_json::to_writer(&mut stream, &value)?;
    stream.write_all(b"\n")?;
    let mut response = String::new();
    BufReader::new(stream)
        .take(4 * 1024 * 1024)
        .read_to_string(&mut response)?;
    Ok(serde_json::from_str(&response)?)
}

pub fn serve(config_path: &Path) -> Result<()> {
    let config = PlatformConfig::load(config_path)?;
    Store::open(&config.state_dir)?;
    let _service = safety::Lock::acquire(&config.state_dir.join("locks/ci-service.lock"), false)?;
    let socket = socket_path(&config);
    safety::no_symlinks(&socket)?;
    if socket.exists() {
        use std::os::unix::fs::FileTypeExt;
        if !fs::symlink_metadata(&socket)?.file_type().is_socket() {
            bail!("refusing to replace non-socket at {}", socket.display());
        }
        if UnixStream::connect(&socket).is_ok() {
            bail!("socket already has a live listener");
        }
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_stop = stop.clone();
    let path = config_path.to_owned();
    let worker = std::thread::spawn(move || {
        while !worker_stop.load(std::sync::atomic::Ordering::Relaxed) && !process::interrupted() {
            let result = PlatformConfig::load(&path).and_then(|c| ci::drain(&c));
            if let Err(e) = result {
                eprintln!(
                    "{}",
                    json!({"event":"worker_wait","error":format!("{e:#}")})
                );
            }
            for _ in 0..10 {
                if worker_stop.load(std::sync::atomic::Ordering::Relaxed) || process::interrupted()
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    });
    eprintln!("{}", json!({"event":"service_ready","socket":socket}));
    let result = (|| -> Result<()> {
        while !process::interrupted() {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                    let response = (|| -> Result<Value> {
                        let mut line = Vec::new();
                        BufReader::new(&stream)
                            .take(65537)
                            .read_until(b'\n', &mut line)?;
                        if line.len() > 65536 || line.last() != Some(&b'\n') {
                            bail!("request must be one JSON line under 64KiB");
                        }
                        let current = PlatformConfig::load(config_path)?;
                        if current.state_dir != config.state_dir {
                            bail!("state directory changed; restart service");
                        }
                        dispatch(&current, serde_json::from_slice(&line)?)
                    })();
                    let envelope = match response {
                        Ok(v) => json!({"ok":true,"result":v}),
                        Err(e) => json!({"ok":false,"error":format!("{e:#}")}),
                    };
                    let _ = serde_json::to_writer(&mut stream, &envelope);
                    let _ = stream.write_all(b"\n");
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    })();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = worker.join();
    // Remove only the socket created by this service, never directory contents.
    fs::remove_file(socket)?;
    result
}
