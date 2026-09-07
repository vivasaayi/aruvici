use crate::{ci, ci_api, ci_store::Store, profiles::PlatformConfig};
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde_json::json;
use std::{io::Write, path::PathBuf};

#[derive(Args)]
pub struct CiArgs {
    #[arg(long, default_value = "platform.toml", global = true)]
    pub platform: PathBuf,
    #[command(subcommand)]
    pub command: CiCommand,
}
#[derive(Subcommand)]
pub enum CiCommand {
    Validate,
    Targets,
    Plan {
        target: String,
    },
    /// Approve exactly the reviewed pipeline digest; not available to API clients.
    Approve {
        target: String,
        #[arg(long)]
        digest: String,
    },
    Queue {
        target: String,
        #[arg(long, default_value = "HEAD")]
        reference: String,
        #[arg(long)]
        key: String,
        #[arg(long, default_value = "null")]
        context: String,
    },
    Worker,
    /// Foreground local service + worker. Does not install a launchd service.
    Serve,
    Runs,
    Run {
        id: i64,
    },
    Events {
        id: i64,
        #[arg(long, default_value_t = 0)]
        after: i64,
    },
    Cancel {
        id: i64,
    },
    Artifact {
        id: i64,
    },
    /// Call the Studio-compatible private socket protocol with a JSON request.
    Request {
        json: String,
    },
    /// Write a self-contained, read-only visual snapshot to stdout.
    Dashboard,
    /// Print a launchd plist for review; does not install/start it.
    ServicePlist,
}
pub fn run(args: CiArgs) -> Result<()> {
    let config = PlatformConfig::load(&args.platform)?;
    let value = match args.command {
        CiCommand::Validate => json!({"valid":true,"targets":config.targets.len()}),
        CiCommand::Targets => ci_api::dispatch(&config, json!({"method":"targets"}))?,
        CiCommand::Plan { target } => serde_json::to_value(config.target(&target)?.plan()?)?,
        CiCommand::Approve { target, digest } => ci::approve(&config, &target, &digest)?,
        CiCommand::Queue {
            target,
            reference,
            key,
            context,
        } => {
            json!({"run_id":ci::enqueue(&config,&target,&reference,&key,serde_json::from_str(&context)?)?})
        }
        CiCommand::Worker => {
            ci::drain(&config)?;
            json!({"drained":true})
        }
        CiCommand::Serve => {
            ci_api::serve(&args.platform.canonicalize()?)?;
            return Ok(());
        }
        CiCommand::Runs => Store::open(&config.state_dir)?.list()?,
        CiCommand::Run { id } => Store::open(&config.state_dir)?.run(id)?,
        CiCommand::Events { id, after } => Store::open(&config.state_dir)?.events(id, after)?,
        CiCommand::Cancel { id } => {
            let s = Store::open(&config.state_dir)?;
            s.cancel(id)?;
            s.run(id)?
        }
        CiCommand::Artifact { id } => Store::open(&config.state_dir)?.artifact_info(id)?,
        CiCommand::Request { json: request } => ci_api::request(
            &ci_api::socket_path(&config),
            serde_json::from_str(&request)?,
        )?,
        CiCommand::Dashboard => {
            let runs = Store::open(&config.state_dir)?.list()?;
            let data = serde_json::to_string(&runs)?
                .replace('<', "\\u003c")
                .replace('>', "\\u003e")
                .replace('&', "\\u0026");
            let html =
                include_str!("../templates/ci-dashboard.html").replace("__RUN_DATA__", &data);
            std::io::stdout().write_all(html.as_bytes())?;
            return Ok(());
        }
        CiCommand::ServicePlist => {
            let exe = std::env::current_exe()?.canonicalize()?;
            let config = args.platform.canonicalize()?;
            let escape = |s: &str| {
                s.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('"', "&quot;")
            };
            println!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>com.aruvi.ci</string><key>ProgramArguments</key><array><string>{}</string><string>ci</string><string>--platform</string><string>{}</string><string>serve</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string></dict></dict></plist>",escape(exe.to_str().context("executable UTF-8 path")?),escape(config.to_str().context("config UTF-8 path")?),escape(&std::env::var("PATH").unwrap_or_default()));
            return Ok(());
        }
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
