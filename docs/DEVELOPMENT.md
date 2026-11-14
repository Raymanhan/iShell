# Development and releases / 开发与发布

For product features and installation, see [English](../README.md) or [简体中文](../README.zh-CN.md). This guide covers the desktop architecture, local setup, validation, and packaging.

## Prerequisites

- Node.js 22.12+ (or a newer LTS release) and npm, matching Vite's runtime requirements.
- Rust stable, with `rustfmt` and `clippy` for CI checks.
- Platform-specific [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).
- OpenSSH for the default macOS/Linux backend. Windows uses `russh`.

On Debian/Ubuntu, the repository's Linux workflows install:

```bash
sudo apt-get update
sudo apt-get install -y libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf libssl-dev
```

Saved credentials use SQLite and a local AES key file; the current implementation does not require GNOME Keyring or KWallet.

## Local development

Run from the repository root:

```bash
npm ci
npm run tauri -- dev
```

For a browser-only UI preview:

```bash
npm run dev
```

Vite serves the preview at `http://127.0.0.1:1420`. The browser uses mock servers, metrics, files, and simulated terminal interactions. It is useful for UI work, but cannot validate SSH, real transfers, native window effects, or forwarding.

## Validation

CI checks the frontend and both SSH backend code paths. Run these commands from the repository root:

```bash
npm run build
npm test
python3 tests/ai_runner_test.py
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --features force-russh -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --features force-russh
```

`force-russh` compiles and tests the Windows transport path on other hosts. It does not replace testing a native Windows installer. The tab-preview lifecycle regression tests can also be run separately with Node.js 22.18+ (or a newer LTS release), which supports the direct TypeScript import used by the tests:

```bash
node --test tests/nativeDragGhost.test.mjs
```

CI also runs end-to-end tests of both SSH backends, and rz/sz over a real SSH session, against throwaway local SSH servers. To run them yourself on a disposable Linux machine with `lrzsz` installed (the script creates local accounts):

```bash
sudo scripts/live-ssh-servers.sh /tmp/ishell-live > live.env
set -a; . ./live.env; set +a
(cd src-tauri && xvfb-run -a cargo test live_ -- --nocapture)
node --import ./tests/register-ts.mjs --test tests/zmodemSsh.test.mjs
```

Changes to transfers, authentication, forwarding, or window handoff also need end-to-end checks with a desktop build and suitable test servers.

## Translations

The interface supports Simplified Chinese, Traditional Chinese, and English. UI messages use Simplified Chinese source strings wrapped in `t("…")`; translations live in `src/i18n/locales/`. Backend error templates are also extracted for `translateMessage()`.

```bash
node scripts/i18n.mjs check
```

Missing translations fail `npm test`. Merge an English source-to-translation JSON map with `node scripts/i18n.mjs merge en new.json`. To regenerate Traditional Chinese, install `opencc-python-reimplemented` and run `node scripts/i18n.mjs zh-tw`.

## Architecture

The React interface calls Tauri commands over IPC. Rust manages local persistence, SSH transports, terminal sessions, transfers, monitoring, and forwarding lifecycles.

| Layer | Implementation |
| --- | --- |
| Desktop shell | Tauri 2, native windows and effects |
| Interface | React 19, TypeScript, Vite, xterm.js |
| macOS/Linux SSH | System OpenSSH; ControlMaster reuses interactive connections |
| Windows SSH | Pure-Rust `russh` with pooled sessions |
| Bulk transfer connections | Dedicated SSH connections separate from interactive traffic |
| Remote file operations | Commands/scripts over SSH; several operations require remote Python 3; folder transfers use tar |
| Metrics and logs | Remote shell tools and streaming processes over SSH |
| AI assistant | OpenAI-compatible Chat Completions or the Anthropic Messages API, model metadata, streaming tool calls, SSH tool execution on one or several servers, permission checks, encrypted history loaded on demand, and context compaction |
| Terminal | Output streamed as raw bytes over a Tauri channel per pane, decoded in the session's encoding; WebGL rendering; ZMODEM (rz/sz) through zmodem.js; local shells through `portable-pty` |
| Interface languages | Simplified Chinese, Traditional Chinese, and English with runtime switching |
| Persistence | SQLite with versioned migrations for profiles/history/snippets/rules, AES-256-GCM for stored credentials with the master key in the OS credential store, WebView local storage for preferences |

Listing, reading, editing, renaming and deleting files use the server's SFTP subsystem. Large transfers use `tar`/`python3` over a dedicated connection where available and fall back to SFTP; servers without an SFTP subsystem fall back to shell commands.

```text
src/
  api/                Tauri IPC wrappers and runtime detection
  components/         Terminals, connection manager, files, editor, logs, metrics
  constants/          Themes and tail-viewer defaults
  features/           Connections, panes, window handoff, transfer queues,
                      AI conversations, snippets, forwarding, themes, and updates
  i18n/               Runtime localization and translation catalogs
  mocks/              Browser-preview data and simulated transfers
  styles/             Base, layout, controls, views, and themes
  utils/              Formatting, syntax highlighting, and drag helpers
  App.tsx             Workbench layout and tab/pane orchestration

src-tauri/src/
  ai/                 Model settings, provider streaming, tools, permissions,
                      remote code runner, encrypted history, and compaction
  commands.rs         Tauri command boundary and connection import/export
  models.rs           Serializable command/event types
  openssh.rs          SSH arguments, jump-host chains, and askpass helpers
  pool.rs             Transport invalidation boundary
  russh_transport.rs  Pure-Rust SSH, jump hosts, and forwarding
  ssh/                Remote commands, file operations, uploads, downloads,
                      archives, monitoring scripts, and parsers
  store.rs            Local SQLite persistence and credential encryption
  terminal.rs         Interactive terminal registry and output snapshots
  process_monitor.rs  Remote process-monitor lifecycle
  tail_monitor.rs     Remote tail lifecycle and UTF-8 stream decoding
  tunnel.rs           Running-forward registry
  time.rs             Time helpers
  lib.rs              Application assembly and lifecycle cleanup
```

## Build installers

```bash
npm ci
npm run tauri -- build
```

Without an explicit target, bundles are normally under `src-tauri/target/release/bundle/`. With `--target`, they are under `src-tauri/target/<target>/release/bundle/`.

On macOS, build for Intel or Apple Silicon with the corresponding target and platform prerequisites:

```bash
rustup target add x86_64-apple-darwin
npm run tauri -- build --target x86_64-apple-darwin

rustup target add aarch64-apple-darwin
npm run tauri -- build --target aarch64-apple-darwin
```

### Code signing

Unsigned installers trigger Gatekeeper on macOS and SmartScreen on Windows. The release workflow signs them when these repository secrets are set, and otherwise builds unsigned installers with a warning:

| Secret | Platform | Purpose |
| --- | --- | --- |
| `APPLE_CERTIFICATE` | macOS | Base64 of the Developer ID Application `.p12` (`base64 -i cert.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | macOS | Password of that `.p12` |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | macOS | Notarization: Apple ID, an app-specific password, and the team ID |
| `WINDOWS_CERTIFICATE` | Windows | Base64 of the code signing `.pfx` |
| `WINDOWS_CERTIFICATE_PASSWORD` | Windows | Password of that `.pfx` |

On macOS the app is signed with the hardened runtime, notarized and stapled, and the DMG is signed, notarized and stapled too. On Windows the app, the NSIS installer and the MSI are signed with SHA-256 and timestamped. Each job then verifies the signatures and fails rather than publish a broken one.

## Release workflow

[Build installers](../.github/workflows/release.yml) builds Windows x64, Linux x64, macOS Intel, and macOS Apple Silicon packages. A pushed `v*` tag triggers a build and GitHub Release publication; a manual workflow run builds installer artifacts, and the publication job runs only for tag refs.

For a new release, keep versions in `package.json`, `package-lock.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`, and `src-tauri/tauri.conf.json` aligned. Add release notes to both READMEs before creating and pushing the matching version tag.

The updater-manifest script reads the Chinese README entry in the form ``- `vX.Y.Z` release notes``. Keep that entry on a single line so the update prompt receives the complete text.

### Update signing

The updater reads `latest.json` from the latest GitHub release. Its public verification key is configured in `src-tauri/tauri.conf.json` under `plugins.updater.pubkey`.

Generate a key pair from the project directory; the command prompts for a password:

```bash
npm run tauri signer generate -- -w ~/.tauri/ishell.key
```

For a new signing setup, configure the generated public key in Tauri and these repository Actions secrets:

- `TAURI_SIGNING_PRIVATE_KEY`: private-key file contents.
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: its password.

The workflow signs macOS `.app.tar.gz`, Windows NSIS, and Linux AppImage update packages, then runs [build-updater-manifest.mjs](../scripts/build-updater-manifest.mjs) to generate `latest.json`. Without the signing secret, installers still build using the no-updater configuration, but signed update artifacts are omitted.

Preserve the signing key for an existing distributed app: installed copies reject updates signed by a different key. Updater signatures verify update packages; they are separate from OS installer code signing or macOS notarization.
