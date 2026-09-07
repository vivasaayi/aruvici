use anyhow::{bail, Context, Result};
use aruvici::{
    artifact, build,
    config::Registry,
    deploy, dev,
    history::History,
    process::{self, args, Executor, System},
    safety, workflow,
};
use clap::{Parser, Subcommand};
use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
};

#[derive(Parser)]
#[command(
    version,
    about = "Local Tauri build manager; deployment always requires interactive approval"
)]
struct Cli {
    #[arg(long, default_value = "apps.toml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Cmd,
}
#[derive(Subcommand)]
enum Cmd {
    /// Fully local profile-based CI: no GitHub Actions required.
    Ci(aruvici::ci_cli::CiArgs),
    Validate,
    List,
    Status {
        app: Option<String>,
    },
    Dev {
        app: String,
    },
    Test {
        app: String,
        #[arg(long, default_value = "HEAD")]
        reference: String,
    },
    Build {
        app: String,
        #[arg(long, default_value = "HEAD")]
        reference: String,
        #[arg(long)]
        repository: Option<PathBuf>,
        #[arg(long)]
        github_output: Option<PathBuf>,
    },
    /// Queue GitHub workflow_dispatch jobs (default), or persist local jobs.
    Queue {
        #[arg(required = true)]
        apps: Vec<String>,
        #[arg(long)]
        local: bool,
        #[arg(long, default_value = "main")]
        reference: String,
    },
    QueueStatus,
    /// Drain the durable local queue, one build at a time.
    Worker,
    History {
        app: Option<String>,
    },
    Verify {
        app: String,
        artifact: PathBuf,
        #[arg(long)]
        sha256: String,
    },
    Deploy {
        app: String,
        artifact: PathBuf,
        #[arg(long)]
        sha256: String,
        #[arg(long)]
        dry_run: bool,
    },
    Rollback {
        app: String,
        backup: String,
        #[arg(long)]
        dry_run: bool,
    },
    ListBackups {
        app: String,
    },
    Recover {
        app: String,
    },
    Clean {
        app: String,
        #[arg(long, default_value_t = 5)]
        keep: usize,
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a reusable workflow to stdout (redirect to a new file).
    Workflow {
        app: String,
    },
}
fn confirm(action: &str, app: &str) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!("{action} requires interactive approval immediately before modifying /Applications; run in a terminal");
    }
    let expected = format!("{action} {app}");
    eprint!("Type '{expected}' to modify /Applications now: ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if answer.trim() != expected {
        bail!("approval not granted; no installation changes made");
    }
    Ok(())
}
fn print(value: impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
fn audit_action(
    history: &History,
    app: &str,
    kind: &str,
    f: impl FnOnce() -> Result<serde_json::Value>,
) -> Result<()> {
    history.event(app, kind, "started", serde_json::json!({}))?;
    match f() {
        Ok(value) => {
            history.event(app, kind, "complete", value.clone())?;
            print(value)
        }
        Err(e) => {
            history.event(
                app,
                kind,
                "failed",
                serde_json::json!({"error":format!("{e:#}")}),
            )?;
            Err(e)
        }
    }
}
fn run() -> Result<()> {
    process::install_signals();
    let cli = Cli::parse();
    if let Cmd::Ci(args) = cli.command {
        return aruvici::ci_cli::run(args);
    }
    let registry = Registry::load(&cli.config)?;
    let exec = System;
    match cli.command {
        Cmd::Ci(_) => unreachable!(),
        Cmd::Validate => {
            print(serde_json::json!({"valid":true,"applications":registry.apps.len()}))
        }
        Cmd::List => print(&registry.apps),
        Cmd::Status { app } => {
            let apps = match app {
                Some(ref n) => vec![registry.app(n)?],
                None => registry.apps.iter().collect(),
            };
            for a in apps {
                print(
                    serde_json::json!({"app":a.name,"changes":build::status(a,&a.repository,&exec)?}),
                )?;
            }
            Ok(())
        }
        Cmd::Dev { app } => dev::launch(&registry, registry.app(&app)?, &exec),
        Cmd::Test { app, reference } => {
            let a = registry.app(&app)?;
            build::run(&registry, a, &a.repository, &reference, true, &exec)?;
            Ok(())
        }
        Cmd::Build {
            app,
            reference,
            repository,
            github_output,
        } => {
            let a = registry.app(&app)?;
            let dir = build::run(
                &registry,
                a,
                repository.as_deref().unwrap_or(&a.repository),
                &reference,
                false,
                &exec,
            )?;
            if let Some(output) = github_output {
                if std::env::var_os("GITHUB_OUTPUT").as_deref() != Some(output.as_os_str())
                    || std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true")
                {
                    bail!("--github-output must be the GitHub-provided GITHUB_OUTPUT path inside Actions");
                }
                safety::no_symlinks(&output)?;
                if dir.to_string_lossy().contains(['\n', '\r']) {
                    bail!("artifact path contains newline");
                }
                writeln!(
                    std::fs::OpenOptions::new().append(true).open(output)?,
                    "artifact_dir={}",
                    dir.display()
                )?;
            }
            print(serde_json::json!({"artifact_dir":dir}))
        }
        Cmd::Queue {
            apps,
            local,
            reference,
        } => {
            let history = History::open(&registry.state_dir)?;
            for name in apps {
                let a = registry.app(&name)?;
                if local {
                    let commit = build::resolve(a, &a.repository, &reference, &exec)?;
                    print(
                        serde_json::json!({"job":history.enqueue(&name,&commit)?,"commit":commit}),
                    )?;
                } else {
                    let private = exec.output(
                        &args(&[
                            "gh",
                            "repo",
                            "view",
                            &a.github,
                            "--json",
                            "isPrivate",
                            "--jq",
                            ".isPrivate",
                        ]),
                        &a.repository,
                    )?;
                    if private != "true" {
                        bail!(
                            "refusing to dispatch to non-private repository {}",
                            a.github
                        );
                    }
                    exec.run(
                        &args(&[
                            "gh",
                            "workflow",
                            "run",
                            "tauri-release.yml",
                            "--repo",
                            &a.github,
                            "--ref",
                            &reference,
                        ]),
                        &a.repository,
                        &[],
                    )?;
                    history.event(
                        &name,
                        "dispatch",
                        "queued",
                        serde_json::json!({"repository":a.github,"reference":reference}),
                    )?;
                }
            }
            Ok(())
        }
        Cmd::Worker => build::drain(&registry, &exec),
        Cmd::QueueStatus => print(History::open(&registry.state_dir)?.queue()?),
        Cmd::History { app } => print(History::open(&registry.state_dir)?.list(app.as_deref())?),
        Cmd::Verify {
            app,
            artifact: zip,
            sha256,
        } => print(
            serde_json::json!({"version":artifact::verify(registry.app(&app)?,&zip,&sha256,&exec)?,"sha256":sha256,"verified":true}),
        ),
        Cmd::Deploy {
            app,
            artifact,
            sha256,
            dry_run,
        } => {
            let a = registry.app(&app)?;
            let history = History::open(&registry.state_dir)?;
            audit_action(
                &history,
                &app,
                if dry_run { "deploy_preview" } else { "deploy" },
                || {
                    deploy::promote(a, &artifact, &sha256, dry_run, &exec, || {
                        confirm("deploy", &app)
                    })
                },
            )
        }
        Cmd::Rollback {
            app,
            backup,
            dry_run,
        } => {
            let a = registry.app(&app)?;
            let history = History::open(&registry.state_dir)?;
            audit_action(
                &history,
                &app,
                if dry_run {
                    "rollback_preview"
                } else {
                    "rollback"
                },
                || deploy::rollback(a, &backup, dry_run, &exec, || confirm("rollback", &app)),
            )
        }
        Cmd::ListBackups { app } => print(deploy::list_backups(registry.app(&app)?)?),
        Cmd::Recover { app } => {
            let a = registry.app(&app)?;
            let history = History::open(&registry.state_dir)?;
            audit_action(&history, &app, "recovery", || {
                deploy::recover(a, &exec, || confirm("recover", &app))?;
                Ok(serde_json::json!({"recovered":true}))
            })
        }
        Cmd::Clean { app, keep, dry_run } => {
            let a = registry.app(&app)?;
            let history = History::open(&registry.state_dir)?;
            audit_action(
                &history,
                &app,
                if dry_run {
                    "cleanup_preview"
                } else {
                    "cleanup"
                },
                || {
                    Ok(
                        serde_json::json!({"moved_to_trash":build::clean(&registry,a,keep,dry_run)?,"dry_run":dry_run}),
                    )
                },
            )
        }
        Cmd::Workflow { app } => {
            std::io::stdout()
                .write_all(workflow::generate(registry.app(&app)?).as_bytes())
                .context("write workflow")?;
            Ok(())
        }
    }
}
fn main() {
    if let Err(e) = run() {
        eprintln!(
            "{}",
            serde_json::json!({"event":"error","error":format!("{e:#}")})
        );
        std::process::exit(1);
    }
}
