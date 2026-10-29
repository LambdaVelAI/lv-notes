# Releasing the macOS app

Pushing a version tag builds the app on GitHub Actions
(`.github/workflows/release.yml`) for Apple Silicon and Intel in one
universal `.dmg`, and attaches it to a **draft** pre-release. Nothing is
public until you publish the draft.

## Cutting a release

1. Set the version in `apps/desktop/src-tauri/tauri.conf.json` (and, to keep
   them in step, `apps/desktop/package.json` and
   `apps/desktop/src-tauri/Cargo.toml`). The workflow refuses a tag that
   doesn't match `tauri.conf.json`.
2. Commit, then tag and push:

   ```bash
   git tag v0.1.0
   ```

   ```bash
   git push origin v0.1.0
   ```

3. When the workflow finishes, open the draft on the repository's Releases
   page, check the notes and the `.dmg`, and publish it.

CI (`.github/workflows/ci.yml`) runs the same tests on every push to
`main`, so a tag on a green commit should build.

## Signing and notarization (one-time setup)

Without these secrets the workflow still builds, but the app is unsigned:
macOS refuses to open it until the tester allows it in System Settings ›
Privacy & Security › Open Anyway (since macOS 15, right-click › Open no
longer does it), or reports it as "damaged" until they run
`xattr -dr com.apple.quarantine /Applications/lv-notes.app`. With them,
Tauri signs the app with your Developer ID and has Apple notarize it, so it
opens normally.

The secrets are set on `LambdaVelAI/lv-notes`. The workflow's warnings say
when a build went out unsigned or not notarized.

### 1. A Developer ID Application certificate

In Xcode: Settings › Accounts › your team › Manage Certificates › + ›
**Developer ID Application**. (Or create it at developer.apple.com ›
Certificates, from a certificate signing request made in Keychain Access.)

Then in Keychain Access, find "Developer ID Application: Your Name
(TEAMID)" under My Certificates, right-click › Export, and save it as a
`.p12` with a password.

### 2. An App Store Connect API key, for notarization

App Store Connect › Users and Access › Integrations › App Store Connect API
› Team Keys › +, with the **Developer** role. Note the **Issuer ID** and the
**Key ID**, and download the `.p8` file (Apple lets you download it once).

### 3. Repository secrets

In the GitHub repository: Settings › Secrets and variables › Actions › New
repository secret.

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | The `.p12`, base64-encoded: `base64 -i DeveloperID.p12 \| pbcopy` |
| `APPLE_CERTIFICATE_PASSWORD` | The password you exported the `.p12` with |
| `APPLE_SIGNING_IDENTITY` | The certificate's name, e.g. `Developer ID Application: Ada Lovelace (AB12CD34EF)` |
| `APPLE_API_ISSUER` | The Issuer ID |
| `APPLE_API_KEY_ID` | The Key ID |
| `APPLE_API_KEY_P8` | The whole `.p8` file's contents |

Keep the `.p12` and `.p8` files somewhere safe (a password manager), and
delete loose copies. The next tag you push builds a signed, notarized app;
the workflow log says which it did.

## What testers' files go through

A project file from an older format version is copied aside before the app
upgrades it (`Thesis (before upgrade from v1).lv`, next to the original).
Formats only move forward: an alpha file keeps opening in later versions.
