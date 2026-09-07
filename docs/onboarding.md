# Onboard the first Tauri application

1. Select one trusted private GitHub repository. Confirm it uses Tauri 2, a committed
   package-lock.json/Cargo.lock, and `npm run build` performs both TypeScript checking
   and a production frontend build (for example `tsc --noEmit && vite build`). Install
   the Tauri CLI as a locked local dev dependency, not a download-on-demand command.
2. Copy the example registry to `apps.toml`; replace every placeholder. Set the
   actual `.app` filename, production bundle ID, repo origin, release artifact path
   and the **existing** production data directory. Do not relocate an existing DB
   as part of adopting this manager. The artifact path is relative to an isolated
   checkout, normally `src-tauri/target/aarch64-apple-darwin/release/bundle/macos/Name.app`.
3. Adopt the data/window convention below, audit every SQLite/open/migration/seed
   path and test that dev and production can run together. Then set
   `isolation_acknowledged = true`. The setting records your audit; it does not
   automatically prove isolation.
4. Run `validate`, `status notes`, `test notes` and `build notes`. Builds require a
   clean committed source tree, clone it locally, and build a detached commit without
   altering your checkout. Ignored files and uncommitted secrets are not copied.
   Dependency scripts still have your account's filesystem privileges.
5. Inspect `history notes` and the output artifact directory. It contains `app.zip`,
   `app.zip.sha256` and `manifest.json` with commit, version, tests, hash and signing.
   Verify using the hash from the trusted local record or reviewed GitHub run.
6. Generate a workflow into a **new** `.github/workflows/tauri-release.yml` in the
   application repository (`aruvici workflow notes`). Review and commit it yourself;
   the generator writes stdout and does not overwrite repository files. Configure
   `ARUVICI_BIN` and `ARUVICI_CONFIG` repository variables as described in runner.md.
   The manager/config are an approved installation outside application repositories.
7. After runner setup and a successful private-repo manual run, download the artifact
   using GitHub or `gh run download`. Quit the installed app manually. Run deploy
   with `--dry-run`, review the target/version/hash, then run deploy and approve at
   its prompt. Launch the app yourself and perform an application-level smoke test.

## Data paths

Use a single application-owned data-path function for **all** SQLite connections,
migrations, caches, plugins and seeded data. Release builds retain the existing
production directory; debug builds use a distinct sibling ending in `.dev`.
Tests use a temporary directory supplied by `ARUVICI_TEST_DATA`, or create their
own TempDir. Never use the production database as a test fixture. Do not put tests,
database migration side effects or running servers into `build.rs` or build hooks.

The following Tauri 2 integration module is provided in
[`tauri-isolation.rs`](../integration/tauri-isolation.rs). Copy/adapt it into each
app, set the fixed release ID to that app's existing ID, and route every data access
through `data_dir`. The release branch ignores development environment overrides.
If an app already uses a different production path, preserve that path in its
release branch and register that same path. Never copy production data to dev by
default. Test schema changes against disposable fixtures.

In the Tauri builder's setup hook, call `label_dev_windows(app.handle())`; use
`window_title` for dynamically created windows too. The launcher also labels
configured windows and overrides the debug bundle identifier with `<release-id>.dev`.
Audit single-instance plugins, sockets, keychain service names, updater plugins and
URL schemes for separate dev/release namespaces. Disable release auto-updaters in
debug builds. The manager cannot enforce isolation inside existing application code.

## Vite/Tauri configuration

Remove custom fixed HMR ports, `hmr.clientPort`, proxies that launch extra servers,
and redundant dev-server startup hooks from the shared launcher path. Vite's default
HMR uses the selected HTTP port. Keep `frontendDist` pointing at the generated assets.
The launcher passes `--host 127.0.0.1 --port N --strictPort` to the configured Vite
command, and the identical URL through `tauri dev --config`. It replaces
`beforeDevCommand`, preserves configured window options while adding `DEV`, and
verifies the listening process group. Static ports are optional; dynamic allocation
avoids all registered static reservations.

The release pipeline overrides `devUrl` to null and disables both beforeDevCommand
and beforeBuildCommand because it runs the frontend build explicitly. The configured
frontend command must terminate and must not run `vite`, `vite preview` or another
server. Production Tauri bundles load embedded frontend assets. Avoid the Tauri
localhost plugin if zero production listening ports are required. Audit custom
application networking separately; the CLI cannot prevent arbitrary app code
opening a socket.

If `tauri.macos.conf.json` defines windows, consolidate that window list into the
base JSON before adoption so the launcher preserves the intended settings.
On Ctrl-C/SIGTERM or either child's exit, the launcher stops both process groups.
Do not configure commands that daemonize or escape their process group. SIGKILL
and power loss cannot execute cleanup; inspect and stop orphaned dev processes
manually without killing production apps.
