# Deployment and recovery boundaries

Promotion verifies a snapshot of the archive against a required trusted SHA-256,
validates every ZIP entry before extraction, and checks the resulting bundle ID,
version, architecture and code signature. The checksum sidecar is not an independent
trust anchor: an attacker can replace both an archive and its sidecar. Prefer the
digest in your protected local history or an authenticated, reviewed GitHub run.
Ad-hoc signature verification alone does not establish publisher identity.

Quit the production app manually, keep it closed, preview deployment and then approve
the interactive command. There is no CI deployment step and the CLI refuses deployment
when GITHUB_ACTIONS is set. It neither kills production processes nor starts apps.
NSWorkspace bundle IDs and all-user process command paths are checked; inspection
errors fail closed. An OS launch race remains between checks and rename. Other logged-in
sessions must also keep the app closed; a malicious or unusual app hiding its process
identity is outside the supported threat model.

An existing bundle must validate against the registry before it is moved. An unsigned,
wrong-ID or wrong-architecture existing app is a refusal, not permission to overwrite.
Investigate/adopt such a legacy app separately. Backups are timestamp-and-PID named
under `/Applications/.aruvici-backups/<app>/`, with a deployment lock in that directory
so separate user registries still serialize promotion. No automatic backup pruning.

A journal is flushed before moving anything. Installation uses same-filesystem
renames from a staged bundle. If post-install validation fails, the rejected app is
retained under `stage-*/Name.app` and the old app is restored. If a process starts
during failure handling, recovery is blocked and the journal stays for inspection.
An unexpected permission/disk/filesystem failure can also block automatic restoration;
the error reports both installation and recovery failures.

After a crash, `deploy`/`rollback` refuse to continue while `transaction.json` exists.
Inspect it and run `aruvici recover APP`, which validates journal paths and asks for
approval immediately before restoring the prior state. Do not manually delete the
journal or backups. Journal removal after a successful operation is the only deletion
in the deployment transaction; no application or backup directory is recursively
deleted. Temporary input extractions are disposable, never installed apps.

`list-backups` shows restorable bundle backups; it does not include failed stage
directories. `rollback APP BACKUP_FILENAME` validates and copies that backup, retaining
the source backup and saving the currently installed app as another backup. This only
rolls back the executable bundle, **not** database schemas or data. Application release
migrations must support rollback, or you must resolve schema compatibility manually.
Build/deploy never opens, seeds, migrates, deletes or copies production SQLite data.

`clean --keep N --dry-run` verifies ownership metadata/hashes and lists old successful
build directories. Without `--dry-run` it moves them to `state_dir/trash/APP/BUILD_ID`;
restore a build by moving that exact directory back when no operation is running.
Neither cleanup mode touches app backups/data. Trash consumes disk until an operator
chooses an explicit, separately reviewed removal; partial builds and unknown files
are deliberately left alone. A keep count of zero moves all recognized builds.

JSON events include SQLite UTC timestamps, app, operation/status and details. Build
details contain build ID, resolved commit, bundle version, tests, SHA-256 and signing
status when those phases complete; failed phases preserve the fields known so far
plus the error. Deployment records contain verified version/hash/signing and backup
paths; correlate the hash to a local build manifest for its source commit. Imported
artifacts are not automatically assigned a source commit from unauthenticated metadata.
Manager history is operational evidence, not tamper-proof audit storage. Limit access
to its state directory and maintain backups.

The manager implements no resource or filesystem sandbox for configured commands.
Use trusted code only, run CI as a separate restricted account, and independently audit
data paths. A process with write access to the state or application directories can
tamper with them; symlink/path checks are defense in depth, not protection from a
concurrent attacker sharing the same account. These limits matter on a Mac that is
also your production machine.
