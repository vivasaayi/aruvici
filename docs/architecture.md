# Architecture and implementation plan

This documents the original desktop pipeline. The new local `ci` commands are
documented in [local-ci.md](local-ci.md); broader Studio integration is tracked in
[local-platform-plan.md](local-platform-plan.md).

Rust CLI first; a future dashboard calls the same domain APIs. TOML is the approved
registry. SQLite stores jobs and structured build/deployment events. External tools
are behind a command executor; deployment transactions take injectable validation
and running-process checks for tests without touching /Applications.

Phases: (1) registry/path boundaries, history and OS locks; (2) isolated build/test
pipeline and GitHub dispatch queue; (3) archive verification, recoverable promotion,
rollback and cleanup; (4) shared development launcher; (5) workflow generation,
onboarding/runner operations and local verification.

Trust boundary: approved repository code and configured commands execute arbitrary
code as the build user. This is not a sandbox. Use a dedicated non-admin runner
account, inaccessible production data, and a separate interactive deployment user.
The build user must not have write access to /Applications. Only approved private
repositories may route jobs here. Never accept fork PR events or workflow inputs
that supply commands, paths, or repository URLs. A SHA-256 authenticates an artifact
only when its expected digest comes from a trusted build record or reviewed run.
Ad-hoc signatures validate integrity, not publisher identity or Gatekeeper trust.

One common absolute state directory and OS flock serialize release work across
repositories/processes. Each app also has a nonblocking operation lock. Locks are
released by the kernel after crashes; lock files are never unlinked. All local/CI
builds must use this manager and the same state directory. GitHub concurrency is
additional repository-scoped coordination, not the machine-wide mutex.

Build a clean, detached local clone of a resolved commit; refuse dirty source trees
for builds (status and dev remain usable). Never reset/clean the user's checkout.
CI can supply its checked-out repository only after validating its origin. Execute
npm ci, frontend build, cargo test, Tauri release with dev hooks disabled, signature
verification, optional Keychain-backed notarization/stapling, ZIP and SHA-256.
Commands are argv arrays, not shell strings. The build directory and target are
disposable manager-owned directories. Production data is never a cleanup target.

Deployment requires an explicit command and typed confirmation immediately before
mutation. Reject running apps, symlinked paths and wrong bundle IDs/architectures.
Validate ZIP paths and resource limits, extract without symlinks, verify the bundle,
stage on the /Applications filesystem, journal, rename the old app to a backup,
rename the staged app into place, then revalidate. On failure retain the rejected
app and restore the backup. A journal blocks further changes after an interrupted
transaction until `recover` is explicitly invoked. Recovery also refuses running
apps. Rename is atomic per operation; two renames are not a single atomic swap.
There is a small OS launch race: do not launch the app during promotion. The manager
does not launch, terminate or disable production apps.

Backups stay under /Applications/.aruvici-backups/<registry-name>. No automated
backup deletion. Cleanup moves only recognized complete build directories to a
manager trash directory. It never recursively deletes apps, backups or data.

Development takes an IPv4 loopback port under a launcher lock, starts Vite with
strictPort and matching Tauri devUrl, then monitors both process groups. The socket
handoff to Vite is inherently racy: bind failures abort safely; rerun to allocate a
new port. Signal handlers terminate process groups. SIGKILL/power loss cannot run
cleanup. The app must adopt the debug data/window conventions before onboarding;
the launcher cannot rewrite hard-coded SQLite paths or dynamically created windows.
