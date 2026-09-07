use crate::{
    config::{App, Registry},
    process::{self, args, ChildGroup, Executor},
    safety::Lock,
};
use anyhow::{bail, Context, Result};
use std::{
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};

pub fn allocate(port: Option<u16>) -> Result<TcpListener> {
    TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port.unwrap_or(0)))
        .context("cannot reserve localhost development port")
}
pub fn launch(registry: &Registry, app: &App, exec: &impl Executor) -> Result<()> {
    app.require_isolation()?;
    crate::build::status(app, &app.repository, exec)?;
    let _app = Lock::acquire(
        &registry
            .state_dir
            .join(format!("locks/dev-{}.lock", app.name)),
        false,
    )?;
    let startup = Lock::acquire(&registry.state_dir.join("locks/dev-start.lock"), true)?;
    let socket = loop {
        let socket = allocate(app.static_dev_port)?;
        if app.static_dev_port.is_some()
            || !registry
                .apps
                .iter()
                .any(|a| a.static_dev_port == Some(socket.local_addr().unwrap().port()))
        {
            break socket;
        }
    };
    let port = socket.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}");
    // CLI-owned beforeDevCommand avoids launching the app's static Vite hook.
    let source_config: serde_json::Value = serde_json::from_slice(
        &std::fs::read(app.repository.join("src-tauri/tauri.conf.json"))
            .context("launcher requires src-tauri/tauri.conf.json (Tauri 2 JSON)")?,
    )?;
    let mut windows = source_config["app"]["windows"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| vec![serde_json::json!({"label":"main"})]);
    for window in &mut windows {
        let title = window["title"].as_str().unwrap_or(&app.name).to_string();
        window["title"] = format!("{title} — DEV").into();
    }
    let override_config = serde_json::json!({"identifier":format!("{}.dev",app.bundle_id),"app":{"windows":windows},"build":{"devUrl":url,"beforeDevCommand":""},"bundle":{"active":false}});
    let env = vec![
        ("PORT".into(), port.to_string()),
        ("VITE_PORT".into(), port.to_string()),
        (
            "ARUVICI_DEV_DATA".into(),
            app.dev_data.display().to_string(),
        ),
    ];
    let mut vite = app.vite_command.clone();
    vite.extend(args(&[
        "--host",
        "127.0.0.1",
        "--port",
        &port.to_string(),
        "--strictPort",
    ]));
    drop(socket); // Vite must bind itself; strictPort prevents silent drift.
    let mut frontend = ChildGroup(process::command(&vite, &app.repository, &env)?.spawn()?);
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if process::interrupted() {
            bail!("development startup interrupted");
        }
        if frontend.0.try_wait()?.is_some() {
            bail!("Vite exited before readiness; retry for a new port");
        }
        if TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse()?,
            Duration::from_millis(100),
        )
        .is_ok()
        {
            // Confirm the listener belongs to Vite's process group, not a
            // different process that won the socket handoff race.
            let owners = exec.output(
                &args(&[
                    "/usr/sbin/lsof",
                    "-nP",
                    &format!("-iTCP:{port}"),
                    "-sTCP:LISTEN",
                    "-t",
                ]),
                &app.repository,
            )?;
            if owners.is_empty() {
                bail!("cannot identify Vite listener");
            }
            for pid in owners.lines() {
                if !pid.bytes().all(|b| b.is_ascii_digit()) {
                    bail!("invalid listener PID");
                }
                let group = exec.output(
                    &args(&["/bin/ps", "-o", "pgid=", "-p", pid]),
                    &app.repository,
                )?;
                if group.trim() != frontend.0.id().to_string() {
                    bail!("allocated port was claimed by another process; retry");
                }
            }
            if frontend.0.try_wait()?.is_some() {
                bail!("Vite failed to own allocated port; retry");
            }
            break;
        }
        if Instant::now() > deadline {
            bail!("Vite did not listen within 45 seconds");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(startup);
    eprintln!(
        "{}",
        serde_json::json!({"event":"dev_ready","app":app.name,"url":url,"data":app.dev_data})
    );
    let mut tauri = app.tauri_command.clone();
    tauri.extend(args(&["dev", "--config", &override_config.to_string()]));
    let mut desktop = ChildGroup(process::command(&tauri, &app.repository, &env)?.spawn()?);
    loop {
        if process::interrupted() {
            bail!("development interrupted");
        }
        if frontend.0.try_wait()?.is_some() {
            bail!("Vite exited; stopping Tauri process group");
        }
        if let Some(status) = desktop.0.try_wait()? {
            if !status.success() {
                bail!("Tauri exited with {status}");
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
