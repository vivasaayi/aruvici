# Aruvici

A local Rust CLI for building and manually promoting a collection of Tauri 2
applications on an Apple Silicon Mac. No daemon or dashboard is required.

The new [fully local CI commands](docs/local-ci.md) implement approved profiles,
a durable local queue, a foreground worker/service, captured stage logs/artifacts,
a private socket API and visual history snapshots. Rust-Tauri executes today;
Rust-Docker/native iOS/native Android provide plans with execution explicitly blocked.
No GitHub Actions setup is needed for `aruvici ci`.

The [Studio platform roadmap](docs/local-platform-plan.md) covers remaining native
adapters and live Studio screens. An actual typed API client is provided in
[integration/ci-client.ts](integration/ci-client.ts). The original desktop commands
and optional GitHub integration documented below remain available for compatibility.

Implemented: validated TOML registry, isolated Git builds, serial release locking,
GitHub dispatch/local SQLite queues, structured history, shared Vite/Tauri launcher,
ad-hoc signing via Tauri, ZIP/checksum/signature validation, confirmed deployment,
recoverable backups, rollback, interrupted-transaction recovery and cleanup previews.

Start with [the architecture](docs/architecture.md), then
[onboard your first app](docs/onboarding.md). Runner installation is a separate
[manual operation](docs/runner.md). See [signing](docs/signing.md) and
[recovery/security boundaries](docs/operations.md).

## Build and check locally

Requires stable Rust, Xcode Command Line Tools and macOS system SQLite. Application
builds additionally need Node/npm, a locked local Tauri 2 CLI, and the Rust target.
GitHub queue dispatch uses an already authenticated `gh` installation.

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
./target/release/aruvici --help
./target/release/aruvici --config apps.example.toml validate
```

The socket test requires permission to bind localhost. The macOS integration test
compiles a tiny temporary Mach-O app and ad-hoc signs it with `-`; it neither creates
credentials nor installs/launches the app. Deployment tests use temporary directories.
No test writes to `/Applications` or your application's data.

Copy `apps.example.toml` to `apps.toml`, fill in real approved paths, and commit the
registry. There are no secrets in the configuration. Use absolute paths without
symlink components (`/private/tmp`, not `/tmp`, when manually configuring temp paths).
The example deliberately has `isolation_acknowledged = false` until adoption.

## Commands

Use `--config /absolute/path/apps.toml` with any command if outside this project.

```sh
aruvici validate
aruvici list
aruvici status                 # all repositories; no fetch/reset/clean
aruvici status notes
aruvici dev notes              # Ctrl-C stops both process groups
aruvici test notes
aruvici build notes --reference HEAD
aruvici queue notes calendar --reference main  # GitHub workflow_dispatch
aruvici queue notes --local --reference HEAD   # durable local queue
aruvici queue-status
aruvici worker                # drain local queue; explicit retry on failure
aruvici history notes
aruvici workflow notes        # generated workflow on stdout
aruvici verify notes /path/app.zip --sha256 TRUSTED_64_HEX_DIGEST
aruvici deploy notes /path/app.zip --sha256 TRUSTED_64_HEX_DIGEST --dry-run
aruvici deploy notes /path/app.zip --sha256 TRUSTED_64_HEX_DIGEST
aruvici list-backups notes
aruvici rollback notes 1788000000000-1234.app --dry-run
aruvici rollback notes 1788000000000-1234.app
aruvici recover notes
aruvici clean notes --keep 5 --dry-run
aruvici clean notes --keep 5
```

Real deploy/rollback/recover require a terminal and typed approval immediately
before `/Applications` changes. There is no `--yes` bypass. A preview checks the
artifact and running-app state without changing `/Applications`; it may create a
temporary extraction directory and a SQLite preview event. Cleanup moves builds
to manager trash for recovery; it does not delete installed apps, backups or data.

## Status and limits

This repository contains an implemented and locally tested first CLI, an example
registry, workflow generator/template, operational docs and failure-path tests.
An already generated example is in [examples/tauri-release.yml](examples/tauri-release.yml).
It is not an installed runner or an onboarded Tauri application. Real application
builds, GitHub routing, actual `/Applications` promotion, Developer ID signing and
notarization still require setup and an acceptance run with your applications.

Initial support: Tauri 2 JSON configuration at `src-tauri/tauri.conf.json`, ordinary
Vite, no submodules, and bundles containing regular files/directories. Symlinked
frameworks, resource-fork-dependent apps, JSON5/TOML Tauri configs and custom detached
dev daemons need explicit support before onboarding. Intel target selection exists;
the supplied runner workflow is ARM64 and Intel execution has not been validated.

The manager cannot sandbox arbitrary npm scripts, Cargo build scripts or tests.
Separate build-user permissions and app data-path adoption are essential. It does
not guarantee recovery from hardware failure or prevent someone launching an app
between process inspection and filesystem rename. Keep apps closed during promotion.
