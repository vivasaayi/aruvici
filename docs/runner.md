# Self-hosted ARM64 macOS runner: manual operations

Nothing in this project installs or registers a runner. Obtain approval immediately
before runner installation/registration, launchd changes, credentials, or production
deployment. These are operator instructions, not an auto-executed setup script.

## Account and repository restrictions

Use one dedicated non-admin macOS build account, with no production app-data access,
no Full Disk Access, no interactive user's login Keychain, and no sudo privileges.
Ensure it cannot modify `/Applications`; build-time scripts execute arbitrary code.
Production runs in your normal user account. Keep the runner outside every app repo,
for example `/Users/tauri-builder/actions-runner`. Keep manager binaries/config under
a separately approved directory, e.g. `/Users/tauri-builder/ci-manager`.

For multiple private repos in one organization, create a restricted runner group,
select **only explicitly approved repositories**, leave public access disabled, and
use workflow restrictions where your GitHub plan supports them. Labels are routing
metadata, not access controls. If runner groups aren't available, use repository
scoped registrations only for individually approved private repos. Prefer one runner
service; several services still must share the manager's common release lock.

Do not attach an unrestricted organization/enterprise runner to this machine.
Never use `pull_request` or `pull_request_target` events here. A private-repo writer
can execute code on your Mac; review collaborator permissions, tag protection and
workflow changes. A private repository alone is not a sandbox.
See [GitHub runner groups](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/manage-access).

## Installation and launchd (run only after approval)

In the approved repository/organization Actions runner settings, select macOS ARM64.
Use the current GitHub-provided runner archive and verify its published SHA-256 before
extracting it in the approved runner directory. Do not embed a stale release URL or
download-and-pipe-to-shell command. Register using the short-lived UI-provided token
and labels `tauri` (GitHub supplies `self-hosted`, `macOS`, `ARM64` by default). For an
organization registration choose the restricted runner group explicitly.

Run `./config.sh` interactively in the runner directory, enter the approved GitHub
URL/token, and verify the runner is visible only to the selected repositories. The
registration token is temporary; don't put it in registry files or shell history.
After explicit approval for service installation, use GitHub's macOS service wrapper
from the build user's logged-in session:

```sh
./svc.sh install
./svc.sh start
./svc.sh status
```

These macOS commands should not use sudo. The wrapper installs a user launchd
LaunchAgent and uses the runner's `runsvc.sh`; inspect the generated plist under that
user's `Library/LaunchAgents`. It runs in that user's login session, not as a root
LaunchDaemon. A logged-out account/reboot with FileVault may need a manual login
before service availability. Do not enable automatic login or disable FileVault as
part of this setup. Custom launchd services must invoke `runsvc.sh`, not `run.sh`.
See [GitHub's service instructions](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/configure-the-application).

Install stable Rust, `aarch64-apple-darwin`, Node/npm and Xcode Command Line Tools for
the build account only after reviewing the operations. Build/install the manager
from a reviewed revision, with its committed Cargo.lock. Configure launchd's PATH
to locate that account's cargo/npm/git without depending on an interactive shell.
Do not add project-local executable directories globally to the runner PATH.

Set repository Actions variables `ARUVICI_BIN` and `ARUVICI_CONFIG` to the approved
absolute manager binary and TOML paths. Every manager registry used for building on
this machine must use **the same state_dir**. Workflow checkouts are supplied through
`--repository`; the manager checks their GitHub origin and builds an isolated clone.
Configure only runner-user data paths in that runner's registry, distinct from any
actual production data. Use a separate interactive deployment registry for your user
with the real production data paths; import artifacts using a trusted digest.
Run all direct release builds in the build account's session, or dispatch them
through GitHub. Do not run a second build manager under your interactive account
with a separate release lock. Sharing build state across users instead requires
an explicitly provisioned group/ACL policy, including lock files (created private
by default); this project does not change user permissions automatically.

## Queue and concurrency behavior

Generated workflows support manual dispatch and `v*` version tags, with no automatic
deployment. They also declare `workflow_call` so a trusted same-repository caller
using an allowed event can reuse the workflow. Generate one registry-specific file
per app. Cross-repository reusable calls require a deliberate repository-gate review;
do not loosen the gate to wildcard repositories.

GitHub `concurrency: tauri-release` is repository scoped. It may coalesce pending runs;
it is not a durable FIFO across all repositories. One runner plus the manager's common
OS lock ensures one resource-intensive release job at a time, including local builds.
The local `queue --local` stores each resolved commit in SQLite and `worker` drains
FIFO. A crashed local worker's running job is marked interrupted on the next worker
run; enqueue it explicitly to retry. `queue` without `--local` dispatches GitHub jobs
after checking repository privacy. Inspect remote runs in GitHub; local queue-status
is only for SQLite jobs. No persistent manager worker service is installed.

## Updates, logs and recovery

Keep the runner's automatic updater enabled; monitor runner release/security notices.
Drain jobs before Rust/Node/Xcode/manager updates and run a known-app acceptance build
afterward. Review pinned checkout/upload-action commits periodically and update after
testing. Runner diagnostics are in `actions-runner/_diag`; inspect the service plist
for stdout/stderr paths and use `svc.sh status` / `launchctl print gui/UID` to inspect
the LaunchAgent. Manager events are JSON on stderr plus SQLite history; tool stdout
and stderr appear in GitHub job logs. No configured argv/environment values are
logged by the manager, but project commands may print sensitive data: audit them.

For recovery, check disk space, login/session, network and service logs first. OS locks
release when all owning descriptors close; never delete lock files to force access.
Stop/restart the service only after approval and when jobs have drained. Quarantine
suspicious workspaces by moving them aside, not resetting a development checkout.
Restore SQLite via its online backup API (or stop manager writers before copying the
database/WAL together); do not copy only a live main database file.

Rotate/revoke GitHub `gh` credentials in the Keychain and reauthorize only needed
repositories. For runner compromise, disable routing, revoke/re-register its runner
identity with a fresh temporary registration token and rebuild the account from a
trusted state. Do not copy `_credentials` or its key into backups/artifacts. Rotate
Apple signing/Keychain profiles separately with explicit approval when introduced.
