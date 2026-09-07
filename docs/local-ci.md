# Local CI engine

`aruvici ci` runs approved project profiles on this Mac. GitHub, GitHub Actions,
a Jenkins server, and a GitHub runner are not required. Source comes from the
registered local Git repository; each run records a resolved commit and builds
an isolated clone. Uncommitted edits are not included or overwritten.

This release implements local Tauri and Docker backends plus a Unix-socket integration
surface. It does not modify Aruvi Studio or provide its proposed live CI screens.
The typed [Studio client](../integration/ci-client.ts) wraps the implemented v1
protocol; provide a Studio Rust/Tauri bridge as its transport. The earlier
`ci-contract.proposed.ts` describes a future richer contract and is not the current API.

## Onboard a desktop project

Copy [platform.example.toml](../platform.example.toml) to `platform.toml`. Replace
the state directory, repository, bundle identifier and expected `.app` path.
Paths must be specific absolute paths for state/repositories; target root and
input paths are relative. Parent traversal and symlinked configured inputs are
rejected. State storage must not overlap a source repository.

Set `profile = "rust-tauri@1"`. The current adapter expects the conventional
`src-tauri/tauri.conf.json` under target root. The configured frontend directory
must contain `package.json` and `package-lock.json`; the locked dependencies must
include the Tauri CLI. Set `cargo_manifest` explicitly for `src-tauri/Cargo.toml`.
Adopt [data isolation](onboarding.md) before setting
`isolation_acknowledged = "true"`. Tests receive a disposable
`ARUVICI_TEST_DATA` directory; the application must actually honor that convention.

Install and maintain Git, stable Rust with rustfmt/Clippy and the selected macOS
target, Node/npm, Xcode command-line tools and Gitleaks separately. The CLI does
not install toolchains. A missing executable fails its stage; plan readiness
checks configuration/files, not a complete toolchain inventory.

Assuming the compiled executable is on your PATH:

```sh
aruvici ci --platform platform.toml validate
aruvici ci --platform platform.toml targets
aruvici ci --platform platform.toml plan desktop
aruvici ci --platform platform.toml approve desktop --digest DIGEST_FROM_PLAN
aruvici ci --platform platform.toml queue desktop --reference HEAD --key first-build
aruvici ci --platform platform.toml worker
aruvici ci --platform platform.toml runs
aruvici ci --platform platform.toml run 1
```

`plan` executes no build commands. Review its exact argv, directories, timeouts,
outputs and issues before approving the returned digest. Approvals bind the
target configuration and resolved stages; policy changes require a new approval.
Missing readiness inputs block queueing even if a digest was approved.

`queue` resolves a local reference to a commit immediately. Repeated submissions
with the same target, commit, plan digest and key return the same run. Use a new
key to request another attempt. Optional `--context` accepts JSON for Studio
product/work-item/workflow identifiers, for example:

```sh
aruvici ci queue desktop --reference COMMIT_SHA --key feature-42-attempt-1 \
  --context '{"product_id":"desktop","work_item_id":"42"}'
```

The worker clones and checks out that commit, then checks the committed files
again. A dirty live tree cannot supply missing files to the isolated build.
Submodules are currently rejected. There is no automatic fetch, Git tag poller,
or Studio checkpoint trigger yet; commit and submit explicitly.

## Executed profile and captured evidence

The Tauri stage order is:

1. `gitleaks dir . --redact` with a JSON report.
2. `cargo fmt -- --check`.
3. `npm ci` in the frontend directory.
4. `npm run build` in the frontend directory.
5. `cargo clippy --locked --all-targets -- -D warnings`.
6. `cargo test --locked`.
7. The installed local Tauri CLI builds the macOS `.app` for ARM64 by default.
8. Bundle/architecture/signature validation, ZIP packaging, checksum and manifest.

The frontend build is the project's build script; this does not independently
guarantee TypeScript checks or frontend tests. Dependency vulnerability auditing,
SBOM generation and normalized test-case parsing are not implemented. Gitleaks
scans the checked-out directory, not the full Git history.
Project `.gitleaks.toml`/`.gitleaksignore` policy overrides are rejected in the
initial adapter; centrally approved scanner customization is not implemented.

The Tauri override sets ad-hoc signing identity `-`, clears build/dev hooks and
sets the development URL to null. The separately executed frontend stage supplies
the assets. This profile supports ARM64 or Intel macOS, not Tauri mobile. Ad-hoc
signing verifies bundle integrity but does not provide Developer ID trust or
notarization; see [signing guidance](signing.md).

Stage failure stops the pipeline and blocks remaining stages. Logs and generated
file reports are registered even when the producing command fails. Successful
verification registers `app.zip`, `app.zip.sha256` and `manifest.json`; the manifest
records commit, profile, policy digest, version, package hash and signing status.
Artifact IDs identify stored files with sizes and SHA-256 hashes.

Logs merge stdout/stderr, are capped at 10 MiB per stage and suppress oversized
lines. Capture redacts known sensitive environment values and recognizable
credential-bearing text. This is heuristic redaction, not a guarantee that every
secret is removed. The Gitleaks JSON is the scanner-produced report with
`--redact`; it is not converted into normalized findings. Treat stored reports
and logs as potentially sensitive local evidence.

Fixed profile commands still execute repository-controlled npm scripts and Rust
build scripts. Approval is a pipeline policy decision, not an operating-system
sandbox. Builds use the invoking user's account and environment.

## Rust-Docker

`rust-docker@1` executes the same required redacted secret scan, Rust formatting,
Clippy and Rust tests before it invokes the user-supplied Dockerfile. It uses fixed
`docker buildx build` arguments and exports a single OCI archive rather than loading
or running a local production container:

```text
docker buildx build --platform linux/arm64 --file Dockerfile \
  --tag configured-local-name --output type=oci,dest=.aruvici-image.oci CONTEXT
```

The archive must be a regular nonempty tar file containing `oci-layout`, `index.json`
and at least one `blobs/sha256/` entry. Aruvici records its SHA-256, byte size,
platform, immutable run manifest and checksum sidecar. A malformed export fails the
run after retaining the captured archive as diagnostic evidence; it never becomes a
promotion-ready result. Docker must already be installed and its daemon available to
the local build account. Aruvici does not start Docker Desktop, publish an image,
load an image into the daemon, mount host data into the build, or restart containers.

The Dockerfile and build context are executable project code. Keep the configured
context narrowly scoped and do not put secrets/production data in it. Linux runtime
tests, image vulnerability scanning, SBOM generation, OCI registry publication and
container deployment are not implemented in this adapter.

## Native mobile profile tags

`native-ios@1` and `native-android@1` can produce inspectable plans and concrete
missing-input issues. Each explicitly reports that adapter execution is unavailable
and is blocked from queueing. They do not currently produce IPA, APK or AAB files.
Their input keys are documented in the example registry. Simulator/emulator execution,
signing and packaging adapters remain roadmap work.

## Queue, service and history

`worker` drains the queue once. `serve` stays in the foreground, accepts local
API requests and repeatedly drains queued work:

```sh
aruvici ci --platform platform.toml serve
```

Release work is serialized with a state-directory lock and a per-target lock.
Use one shared state directory for all targets that must share the single-worker
limit. A worker restart marks abandoned runs interrupted rather than silently
retrying them. Retry by enqueueing with a new key. Cancellation terminates the
command process group and preserves evidence:

```sh
aruvici ci cancel 1
aruvici ci events 1 --after 0
aruvici ci artifact 3
```

Run history includes the latest 200 runs; event reads return up to 1,000 events
after the provided sequence ID. Persist the last event ID and request subsequent
batches to replay after reconnecting. SQLite state and run artifacts live beneath
the configured state directory. This version does not implement automatic artifact
retention for the new CI store.

To produce a launchd configuration for review:

```sh
aruvici ci --platform platform.toml service-plist > com.aruvi.ci.plist
```

This prints a plist with the current executable, configuration path and PATH.
It neither installs nor starts a service. Review paths/environment before a
separately approved installation; foreground `serve` is sufficient for testing.
No GitHub runner is involved.

## Aruvi Studio integration protocol

The service listens at `<state_dir>/ci.sock`, restricted to the local user with
mode `0600`. Studio's Rust backend can connect using `UnixStream`; a browser
cannot connect directly. Use one newline-terminated JSON request per connection,
under 64 KiB. Read a newline JSON response, then close the connection:

```json
{"method":"enqueue","target":"desktop","commit":"FULL_COMMIT_SHA","key":"work-item-42-attempt-1","context":{"work_item_id":"42"}}
```

Responses use `{"ok":true,"result":...}` or `{"ok":false,"error":"..."}`.
API clients cannot approve policies or deploy applications. File downloads use
artifact IDs rather than caller-selected filesystem paths.

| Method | Request fields | Result |
| --- | --- | --- |
| `health` | none | Online status and protocol version |
| `targets` | none | Targets with resolved plans/readiness |
| `plan` | `target` | Profile plan and approval digest |
| `enqueue` | `target`, `commit`, `key`, optional `context` | Run ID |
| `runs` | none | Recent runs with stages/artifacts |
| `run` | numeric `id` | Run detail |
| `events` | run `id`, optional `after` | Persisted event batch |
| `cancel` | run `id` | Updated run |
| `logs` | run `id`, `stage`, optional byte `offset` | Text chunk and next offset |
| `artifact` | artifact `id` | Metadata, size and hash |
| `artifact_chunk` | artifact `id`, optional byte `offset` | Byte array, next offset, EOF and hash |

The CLI can exercise that same protocol while `serve` is running:

```sh
aruvici ci request '{"method":"health"}'
aruvici ci request '{"method":"logs","id":1,"stage":"rust-tests","offset":0}'
aruvici ci request '{"method":"artifact_chunk","id":3,"offset":0}'
```

Log and artifact reads return at most 65,536 bytes per request. Binary content is
a JSON array of byte values, not UTF-8 or base64. Concatenate chunks in offset order
and verify the completed file against the advertised SHA-256. Logs should be
rendered as text, and package files offered as downloads.

## Visualization and promotion

```sh
aruvici ci dashboard > ci-dashboard.html
```

The generated self-contained HTML snapshot lets you select a recorded run and
inspect stages, statuses, artifact metadata and provenance. It embeds current
history and is read-only; regenerate it for updated data. It is not a live Studio
dashboard and does not include binary downloads or promotion controls.

The existing manual deployment commands and `apps.toml` registry remain separate.
Register matching bundle/deployment/data settings there, inspect the package and
checksum, and use the documented interactive deployment procedure. CI does not
write into `/Applications` or automatically promote a passed build.

The next slices in [the platform roadmap](local-platform-plan.md) are Studio's
backend connector and setup/run/artifact views, additional executable platform
adapters, richer findings, remote tag polling and visual promotion integration.
