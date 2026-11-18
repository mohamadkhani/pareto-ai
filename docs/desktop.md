# Desktop builds

Pareto AI ships a thin desktop shell (decision D43): a Tauri v2 window whose
only job is to run the **unchanged gateway binary as a sidecar** and point a
webview at it. The gateway still serves its embedded Angular console, so there
is no second frontend build and no custom webview assets.

| Piece | Location |
| --- | --- |
| Tauri shell (Rust + config) | `desktop/` |
| Sidecar copy expected by Tauri | `desktop/binaries/pareto-gateway-<target-triple>[.exe]` (git-ignored, written by `desktop/build.rs`) |
| Placeholder icon source | `desktop/icons/placeholder.svg` |
| CI | `.github/workflows/desktop.yml` |
| Gateway build | `server/` (unchanged, not a workspace member) |

## Startup sequence

1. Tauri starts; a second launch focuses the existing window instead of opening
   a new one (`tauri-plugin-single-instance`).
2. The shell starts the sidecar on the fixed loopback port `127.0.0.1:8081`
   (never random, so API clients can rely on it — D46).
3. It creates `<app-data>/gateway` and spawns the sidecar with:

   | Variable | Value |
   | --- | --- |
   | `GATEWAY_DATA_DIR` | `<app-data>/gateway` |
   | `GATEWAY_LISTEN` | `127.0.0.1:8081` |
   | `GATEWAY_INSECURE_COOKIES` | `1` (the app speaks plain HTTP on loopback) |

   `GATEWAY_ADMIN_EMAIL`, `GATEWAY_ADMIN_PASSWORD` and `GATEWAY_MASTER_KEY` are
   **removed** from the inherited environment, so the first run always reaches
   the setup wizard.
4. It polls `GET /health` every 200 ms for up to 20 s.
5. On success it opens the main window (1280×800, minimum 900×600) at
   `http://127.0.0.1:8081/` and shows the system-tray icon.
6. On failure it shows a native error dialog containing the last 40 lines of
   `<app-data>/gateway/sidecar.log`, then exits with status 1.
7. On exit it kills and reaps the sidecar.

## Tray and background operation

The shell owns one system-tray icon (`tauri`'s `tray-icon` feature). The
gateway is a background service: it keeps running after the window is gone,
and the tray is how the user controls that.

| Action | Result |
| --- | --- |
| Close the window (title-bar ✕) | The window hides; the gateway keeps running; the tray icon stays |
| Tray → "Open Pareto AI" | Shows and focuses the window again |
| Left double-click the tray icon (Windows) | Shows and focuses the window |
| Left click the tray icon (macOS) | Opens the tray menu (platform convention) |
| Tray → "Quit Pareto AI" | Stops and reaps the gateway sidecar, then exits the app |
| Second app launch | Focuses the running instance's window (single-instance plugin) |

There is no other way to stop the service: `SIGTERM`/session logout still
exits and stops the sidecar, but the window close button never does.

`<app-data>` is the OS-specific application data directory for the identifier
`dev.logicamp.paretoai`:

| OS | Path |
| --- | --- |
| Linux | `~/.local/share/dev.logicamp.paretoai/gateway` |
| Windows | `%APPDATA%\dev.logicamp.paretoai\gateway` |
| macOS | `~/Library/Application Support/dev.logicamp.paretoai/gateway` |

That directory holds `gateway.db`, `master.key` (generated on first start,
mode 0600) and `sidecar.log`. Deleting it resets the app to the wizard.

## Prerequisites

Common: Rust stable (1.77+; this repo is on 1.99), Node 22 and pnpm 10.27
(`packageManager` in `web/package.json` and `desktop/package.json`).

Linux (Debian/Ubuntu):

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev \
    librsvg2-dev patchelf build-essential rpm
```

`rpm` is only needed to produce the `.rpm` package. This host (Arch) already has
WebKitGTK 4.1 and GTK 3; `dpkg-deb` is provided by `dpkg`, which Tauri's deb
bundler works with internally.

Windows:

- WebView2 runtime (preinstalled on Windows 10/11 and on `windows-latest`).
- MSVC build tools ("Desktop development with C++").
- Tauri downloads the WiX (MSI) and NSIS toolchains on the first bundle.

macOS:

- Xcode command line tools (`xcode-select --install`).

## Building

```bash
just desktop-setup      # pnpm install in desktop/ (Tauri CLI)
just desktop-sidecar    # builds web/ + gateway release, copies the sidecar
just desktop-dev        # tauri dev against that gateway
just desktop-check      # cargo fmt --check, clippy -D warnings, tests
just desktop-build      # full bundle for the host platform
```

`desktop-sidecar` builds the UI first because the gateway embeds it
(`rust-embed`). `build.rs` also copies the gateway into `desktop/binaries/` on
its own, resolving, in order: `PARETO_GATEWAY_BIN`, `server/target/<target>/<profile>/gateway`,
`server/target/<profile>/gateway`, then the other profile. A missing gateway is a
build warning (so `cargo check` works on a fresh checkout) and a hard error in
the bundling phase.

Run a single bundle type while iterating:

```bash
cd desktop
pnpm tauri build --debug --no-bundle          # binary only, fastest check
pnpm tauri build --debug --bundles deb,rpm    # deb + rpm
```

`pnpm tauri dev` starts in dev mode but still launches the real gateway sidecar
from `desktop/binaries/`; it does not proxy to `just dev`.

## Artifacts

Tauri writes bundles to `desktop/target/<target>/release/bundle/` (or
`desktop/target/release/bundle/` without `--target`). Verified locally on Linux
with `--debug`; the names come from `productName = "Pareto AI"`, so they contain
a space.

| OS | Bundles | File names |
| --- | --- | --- |
| Linux | deb | `Pareto AI_0.1.0_amd64.deb` |
| Linux | rpm | `Pareto AI-0.1.0-1.x86_64.rpm` |
| Linux | AppImage | `Pareto AI_0.1.0_amd64.AppImage` |
| Windows | NSIS | `Pareto AI_0.1.0_x64-setup.exe` |
| Windows | MSI | `Pareto AI_0.1.0_x64_en-US.msi` |
| macOS | dmg | `Pareto AI_0.1.0_universal.dmg` |
| macOS | app | `Pareto AI.app` (CI also uploads `Pareto AI-macos-universal.app.zip`) |

The deb installs `pareto-desktop` and `pareto-gateway` side by side in
`/usr/bin`; the shell finds the sidecar in its own directory, so the bundled
layout needs no extra configuration.

## Arch Linux (AUR)

Four packages, updated by CI on every `v*` tag (see *Publishing to AUR*):

| Package | Contents |
| --- | --- |
| `pareto-gateway` | Server only, built from source: `/usr/bin/pareto-gateway` + the daemon |
| `pareto-gateway-bin` | The same, from the prebuilt release tarball |
| `pareto-ai` | Desktop app built from source: tray shell + bundled gateway + the daemon |
| `pareto-ai-bin` | The same, from the prebuilt release tarball |

The server family is the lightweight install (no GTK/WebKit dependencies). The
desktop family bundles its own gateway copy (`/usr/lib/pareto-desktop/`, also
symlinked as `/usr/bin/pareto-gateway`), so **the two families conflict**: only
one can be installed at a time, and either one is enough to run the gateway.

Every package ships the same daemon, so the gateway runs headless:

```bash
sudo systemctl enable --now pareto-gateway   # http://127.0.0.1:8081
sudo systemctl edit pareto-gateway           # or edit /etc/pareto-gateway.conf
```

State lives in `/var/lib/pareto-gateway` (created by `StateDirectory`); the
first run serves the setup wizard at the console URL. With the desktop family
the tray app runs the same gateway on the **same fixed port** (`127.0.0.1:8081`,
data in `~/.local/share/dev.logicamp.paretoai/`), so the daemon and the tray
app must not run at the same time — use one or the other.

### Publishing to AUR

`packaging/arch/` holds the four canonical PKGBUILDs plus `pareto-gateway.service`,
`pareto-gateway.sysusers`, `pareto-gateway.conf`, `pareto-ai.desktop` and
`publish-aur.sh`. On a `v*` tag, after the GitHub Release is up, the
`publish-aur` jobs in `ci.yml` (server) and `desktop.yml` (desktop) copy each
PKGBUILD into its AUR repo, inject the version and the real b2sum, regenerate
`.SRCINFO` and push — the `dicto` flow. Hand-edits on AUR are overwritten on
the next release.

This needs the `AUR_SSH_PRIVATE_KEY` repository secret (an ed25519 key
registered on the AUR account, `https://aur.archlinux.org/account/ssh`); the
same key publishes all four packages. Without the secret the release still
ships, but the `publish-aur` jobs fail.

The plain server binaries — no desktop shell — are a separate CI artifact from
`.github/workflows/ci.yml` (the `gateway` job):
`pareto-gateway-<version>-linux-x86_64.tar.gz` and
`pareto-gateway-<version>-windows-x64.zip`, each holding the embedded-UI binary
as `pareto-gateway[.exe]` and the api-only one as `pareto-gateway-api[.exe]`,
plus the systemd daemon files on Linux.
Every run uploads them to the run's *Artifacts* section: the `gateway` job waits
only for the web **build** (`web`), never for the UI tests (`web-check`), so a
failing test run still publishes downloadable server binaries. They are attached
to the GitHub Release on a `v*` tag, when `web-check`, `rust` and `gateway` are
all green.

## Icons

`desktop/icons/placeholder.svg` is a **placeholder** mark, not final branding.
`icons/placeholder.png` is the 1024×1024 raster (regenerate it with
`rsvg-convert -w 1024 -h 1024 icons/placeholder.svg -o icons/placeholder.png`),
and the platform icon set comes from the Tauri CLI:

```bash
pnpm tauri icon icons/placeholder.png   # writes icons/ + the Windows logos
```

Replacing the brand means replacing the SVG (or a 1024×1024 PNG) and rerunning
that command. The iOS and Android outputs are not used by this app and were
removed.

## CI (`.github/workflows/desktop.yml`)

**macOS is disabled for now (2026-11-18).** With a private repository in the
current plan, GitHub cancels the `macos-14` job within ~2 s without running a
single step — no macOS runner is assigned. Options: make the repository public
(public repos get free macOS minutes), enable paid macOS minutes / a spending
limit, or build locally on a Mac with `just desktop-sidecar && just desktop-build`
in `desktop/`. Re-enable the commented matrix entry once one of the first two
applies.

Hosted runners only: Windows and macOS bundles cannot be produced on the Linux
development host.

| Job entry | Runner | Target | Bundles |
| --- | --- | --- | --- |
| `linux-x86_64` | `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | deb, rpm, AppImage |
| `windows-x64` | `windows-latest` | `x86_64-pc-windows-msvc` | NSIS, MSI |
| `macos-universal` | `macos-14` (arm64) | `universal-apple-darwin` | dmg, app |

Each job installs Node/pnpm and Rust stable, caches cargo via
`Swatinem/rust-cache`, builds `web/`, builds the gateway for its target, places
the sidecar with Tauri's naming, and runs `pnpm tauri build --target … --bundles …`.
The macOS job builds the gateway for both Apple targets and merges it with
`lipo`, so one universal app carries both architectures. Artifacts upload as
`pareto-desktop-<name>`; on a `v*` tag a final job downloads them all and
attaches them to the GitHub Release with `softprops/action-gh-release`.
`workflow_dispatch` allows manual runs.

The CI workflow does not use `--offline`: the Justfile's `--offline` assumes a
warm local cargo cache, which a fresh runner does not have.

## Signing and notarization (optional)

Unsigned builds are complete and pass CI. Signing is enabled purely by adding
repository secrets; the workflow exports them only when they are non-empty.

macOS:

| Secret | Meaning |
| --- | --- |
| `APPLE_CERTIFICATE` | base64 of the Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | its password |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Name (TEAMID)` |
| `APPLE_ID` | Apple ID used for notarization |
| `APPLE_PASSWORD` | app-specific password |
| `APPLE_TEAM_ID` | team id |

Tauri signs and notarizes when all of them are present. An App Store Connect API
key (`APPLE_API_KEY`, `APPLE_API_ISSUER`, `APPLE_API_KEY_PATH`) is an alternative
to `APPLE_ID`/`APPLE_PASSWORD`; it is not wired into the workflow yet.

Windows:

| Secret | Meaning |
| --- | --- |
| `WINDOWS_CERTIFICATE` | base64 of the code-signing `.pfx` |
| `WINDOWS_CERTIFICATE_PASSWORD` | its password |

Keep every key out of the repository; only the CI secret store holds them.

AUR publishing (optional): the `AUR_SSH_PRIVATE_KEY` secret is an ed25519
private key whose public half is registered on the AUR account. It is
account-wide (AUR has no per-package deploy keys) and updates all four
packages; see *Arch Linux (AUR)* above.

## Cross-compile limits

- This Linux host can only build Linux bundles. Windows needs MSVC + WebView2,
  macOS needs Xcode; both are runner-only.
- A Linux machine cannot produce `.msi`/`.exe` installers, and Tauri does not
  support Windows targets from Linux.
- The universal macOS build is the one cross-architecture case we rely on: it is
  produced on an arm64 runner using the `x86_64-apple-darwin` Rust target, not on
  Linux.
- AppImage bundling needs `linuxdeploy`; on hosts without FUSE, set
  `APPIMAGE_EXTRACT_AND_RUN=1` (the workflow does).

## Deferred

- **Updater**: no `tauri-plugin-updater` and no update signing keys yet. The
  build log may print `__TAURI_BUNDLE_TYPE` warnings because the Tauri crate
  (2.12.6) is slightly newer than the CLI (2.12.5); this only affects updates.
- **Code signing / notarization in practice**: wired but untested without the
  secrets above.
- **Windows ARM64 and Linux ARM64**: not in the matrix.
- **AppStream metadata** for the AppImage: `appimagetool` warns it is missing.

## Local verification (Linux, 2026-11-18)

| Command | Result |
| --- | --- |
| `cargo check` / `cargo clippy --all-targets -- -D warnings` in `desktop/` | clean |
| `cargo test` in `desktop/` | 4 tests pass (health probe, free port, timeout, log tail) |
| `just desktop-check` after the tray change | clean (fmt, clippy `-D warnings`, 4 tests); the tray and hide-on-close behaviour itself was not exercised (no usable display automation here) |
| `just desktop-check` after the fixed-port change (D46) | clean (fmt, clippy `-D warnings`, 4 tests) |
| AUR packaging (2026-11-18) | `makepkg --printsrcinfo` clean for all four PKGBUILDs, `systemd-analyze verify` clean for the unit (only "binary not installed" on this machine), `bash -n publish-aur.sh` clean; a real `makepkg` build is only possible after the first `v*` tag exists |
| gateway sidecar with the app's env (`GATEWAY_DATA_DIR`, `GATEWAY_LISTEN`, `GATEWAY_INSECURE_COOKIES`) | `/health` 200, `/api/setup/status` reports `mode=setup`, master key generated in the data dir |
| `pnpm tauri build --debug --no-bundle` | binary built |
| `pnpm tauri build --debug --bundles deb,rpm` | both bundles built |
| `pnpm tauri build --debug --bundles appimage` with `APPIMAGE_EXTRACT_AND_RUN=1` | **blocked on this host**: `appimagetool` cannot download `runtime-x86_64` from GitHub (`server returned status code 0`). Not a configuration problem; GitHub runners can download it. |
| `pnpm tauri dev` | the app started, generated `master.key`, the gateway came up in setup mode on a random loopback port and received the shutdown signal — the GUI itself was not inspected (no usable display automation here) |

Windows and macOS were **not** built locally; their first real check is the
GitHub Actions run.
