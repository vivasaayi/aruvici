# Preview channel

A Preview target is the safe interpretation of “auto release” on a shared
development/CI/production Mac. It creates a separately named, separately
identified test application after every fully passing build. It never promotes
or replaces `/Applications/AruviStudio.app`.

## Safety contract

- A Preview target must use a bundle identifier ending in `.preview` and a
  product name containing `Preview`. macOS therefore treats it as a different
  app, and the user can see which one is running.
- Its destination must be below `<state_dir>/previews`; configuration cannot
  point it at `/Applications`, a repository, or an arbitrary user directory.
- The Tauri build override uses the Preview bundle identifier and product name.
  Tauri applications must derive their application data from that identifier
  (or use an explicitly separate `ARUVI_APP_DATA_DIR`). AruviStudio already
  maps `com.aruvi.studio.preview` to its own profile.
- The normal pipeline still runs from an isolated Git checkout and must pass
  scan, format, lint, tests, package, checksum and signature checks first.
- Candidate replacement is a journaled rename transaction. The prior Preview
  bundle goes to a timestamped backup under the same manager-owned preview
  root. No recursive deletion is used.
- If Preview is running, installation is **deferred**. The build remains a
  passing, verified candidate and the next queued Preview build will update it
  after the test application is closed. Aruvici never terminates it.
- Preview installation never touches application data, including SQLite
  databases. Production deployment retains its separate interactive command,
  running-app check and typed confirmation.

## AruviStudio example

`platform.toml` contains `aruvi-studio-preview`. It packages the same commit as
the ARM64 target with these important inputs:

```toml
bundle_id = "com.aruvi.studio.preview"
product_name = "AruviStudio Preview"
artifact = "src-tauri/target/aarch64-apple-darwin/release/bundle/macos/AruviStudio Preview.app"

[targets.preview]
destination = "/Users/you/Library/Application Support/aruvici/previews/aruvi-studio-preview/current/AruviStudio Preview.app"
```

Review and approve the plan once, then queue a committed checkpoint:

```sh
aruvici ci --platform platform.toml plan aruvi-studio-preview
aruvici ci --platform platform.toml approve aruvi-studio-preview --digest REVIEWED_DIGEST
aruvici ci --platform platform.toml queue aruvi-studio-preview --reference COMMIT --key studio-COMMIT
```

The worker/service is intentionally not installed or started by this change.
Starting it as a launchd service is a separate machine-level approval. The next
Studio integration slice will submit the committed work-item checkpoint through
the existing private local socket and display the run, candidate, and feedback
link; it will not gain an API for production promotion.
