# Signing and Gatekeeper

Initially use `[apps.signing] identity = "-"`. The pipeline passes
`bundle.macOS.signingIdentity = "-"` to the Tauri build, which signs the bundle and
nested code. The manager does not apply a blanket `codesign --deep --force` that
could replace component entitlements. It checks the final bundle and packaged copy
with `codesign --verify --deep --strict` and validates ID, version and CPU architecture.

Ad-hoc signing gives local integrity checks, not an Apple-authenticated publisher.
It does not make a downloaded/quarantined app pass Gatekeeper, and it cannot be
notarized. Distribution may still trigger refusal or an unidentified-developer
warning. The manager does not strip quarantine, disable Gatekeeper or bypass system
policy. It propagates an archive's quarantine marker onto the extracted bundle.
Review macOS's per-app approval UI yourself if applicable. See
[Tauri macOS signing](https://tauri.app/distribute/sign/macos/).

## Developer ID and notarization later

After separately approving enrollment and credential setup, install a Developer ID
Application identity in the signing account's Keychain. Keep the private key and
certificate out of Git, workflow artifacts, TOML, shell history and logs. Review the
Keychain access policy for noninteractive codesign; do not grant unrestricted
access to all programs. Test login/lock behavior of the dedicated build account.

Configure only non-secret names:

```toml
[apps.signing]
identity = "Developer ID Application: Your Name (TEAMID)"
team_id = "TEAMID"
notary_profile = "aruvici-notary"
```

Create the profile using `xcrun notarytool store-credentials aruvici-notary` interactively
only after explicit approval. Never place passwords/tokens on a committed command
line. The implementation submits a ZIP with `notarytool --keychain-profile --wait`,
staples the ticket, validates it, repackages, and rechecks the final artifact using
stapler and `spctl`. It also verifies the certificate's team and Developer ID
Application requirement. Configure entitlements and hardened runtime for your app
in Tauri, then perform a real acceptance run. No credentials exist here and this
optional pipeline has not been exercised against Apple's service.

Ambient Apple credential/signing variables are removed from child commands; signing
is driven by the explicit registry and Keychain. npm/Cargo code still runs as the
signing account and may access whatever that account can access. For stronger future
separation, move signing to a distinct account/service with narrowly scoped inputs.
Read [Apple's notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)
before credential setup. Runtime Keychain and app permissions can behave differently
when switching from ad-hoc to Developer ID; test those transitions before promoting.
