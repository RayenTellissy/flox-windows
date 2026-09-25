# Flox for Windows

Flox for Windows brings the two existing Flox apps together in one desktop app:

- **The Android TV player** (`../flox`): browse TMDB, play from the Telegram library or VidLink, with the same player overlay and settings.
- **The Mac ingest tool** (`../flox-mac`): queue VidLink, pasted-link, 4KHDHub and local-file jobs that are split and uploaded to the library channel.

It is written in Rust. The UI is built with Slint. libmpv handles playback, TDLib handles Telegram, and WebView2 handles the VidLink pages. The design is dark and monochrome, and the app is keyboard-first with full mouse support.

Version: **1.0.0**.

## Layout

```
Cargo.toml              workspace (version, shared dependencies, lints)
rust-toolchain.toml     Rust 1.95 + x86_64-pc-windows-msvc
assets/
  fonts/                Geist and Geist Mono (Regular, Medium)
  icons/                player and UI glyphs (PNG from Android xxhdpi, SVG from Android vectors)
  web/                  adblock.js, flox_nav.js (Android), shim.js, tap.js (Mac sniffer)
crates/
  flox-core/            models, settings, progress, TMDB, image cache, paths, tool resolver, sniff contract
  flox-td/              TDLib JSON client (tdjson loaded at runtime), auth, library index, upload, streaming
  flox-rip/             ingest pipeline: downloaders, 4KHDHub, ffmpeg/ffprobe, splitter, queue
  flox-player/          libmpv wrapper (loaded at runtime), stream callbacks, OpenGL render API
  flox-web/             ad-block policy, injected scripts, bridge parsing; WebView2 host on Windows
  flox-sys/             folders, keep-awake, toasts, media keys, file dialogs, WebView2 check
  flox-app/             the `flox` binary: Slint UI (ui/), focus engine, view models, player
scripts/
  verify.sh             the verification matrix (below)
  fetch-deps.ps1        fetches or builds the third-party binaries (Windows)
  package.ps1           builds the portable zip (Windows)
```

Neither libmpv nor tdjson is linked at build time. Both are loaded with `libloading` from the resolved path. If one is missing, the app shows a readable "not found" state instead of failing at startup.

## Prerequisites

**macOS (development):**
- rustup. `rust-toolchain.toml` installs Rust 1.95 and the `x86_64-pc-windows-msvc` target.
- Optional: `ffmpeg`, `ffprobe` and `yt-dlp` in `/opt/homebrew/bin`, and libmpv (`brew install mpv`) for the player tests.
- TDLib: the flox-mac vendored `libtdjson.dylib` is used for the live FFI tests.

**Windows (run and package):**
- Windows 10 or 11 x64, with the Evergreen WebView2 runtime (preinstalled on current Windows).
- Rust 1.95 with the MSVC toolchain (Visual Studio 2022 Build Tools, C++ workload).
- PowerShell 7 for the scripts.

No dependency may compile C for the MSVC target in a build script. This rule is what lets the whole workspace type-check for Windows from a Mac. For the same reason, reqwest uses `native-tls` (SChannel on Windows), and there is no rustls/ring/aws-lc, openssl-sys, zstd-sys or libsqlite3-sys.

## Verify

```
scripts/verify.sh
```

It runs these steps:

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings` (also available as `cargo wcheck`)
4. `cargo test --workspace`

If `FLOX_TDJSON` is unset and the flox-mac dylib exists, the script exports it as `FLOX_TDJSON`. Tests that need `FLOX_TDJSON` or `FLOX_LIBMPV` skip when those are unset. Tests that hit the live internet are `#[ignore]`; run them with `cargo test -- --ignored`.

The UI snapshot test writes `target/snapshots/scaffold.png` using Slint's software renderer.

| Check | macOS | Windows |
|---|---|---|
| fmt, clippy (host) | yes | yes |
| clippy for x86_64-pc-windows-msvc | yes (no linker needed) | yes |
| Unit tests, UI snapshots | yes | yes |
| Live TDLib FFI | with the vendored dylib | with `tdjson.dll` |
| mpv playback | with `brew install mpv` | yes |
| WebView2, toasts, media keys, keep-awake | no-ops | yes (manual checklist) |
| Linking `flox.exe`, packaging | no | yes |

## Run

**macOS (development build):**

```
cargo run -p flox-app
```

**Windows:**

```
.\scripts\fetch-deps.ps1 -BuildTdlib
cargo run -p flox-app
.\scripts\fetch-deps.ps1 -Profile release
cargo build --release
.\scripts\package.ps1
```

`fetch-deps.ps1` stages `tdjson.dll` and `libmpv-2.dll` next to the executable, and ffmpeg, ffprobe and yt-dlp in `tools\`.

### Credentials

You can enter the TMDB API key and the Telegram API id and hash (from my.telegram.org/apps) in Settings. You can also bake them in at build time with these environment variables:

```
FLOX_TMDB_API_KEY=...
FLOX_TELEGRAM_API_ID=...
FLOX_TELEGRAM_API_HASH=...
```

A value saved in Settings takes precedence over the built-in one.

### Tool resolution

For ffmpeg, ffprobe, yt-dlp, tdjson and libmpv, the app searches in this order:

1. The Settings override, when it is set and the file exists.
2. The app folder (`<app>\` and `<app>\tools\`).
3. `PATH`.

ffprobe is derived from the resolved ffmpeg.

## Data locations

| What | Windows | macOS (dev) |
|---|---|---|
| Settings (`settings.json`), progress (`progress.json`) | `%APPDATA%\Flox\` | `~/Library/Application Support/Flox-dev/` |
| TDLib database and files | `%LOCALAPPDATA%\Flox\tdlib\{db,files}` | `~/Library/Caches/Flox-dev/tdlib/` |
| Image cache | `%LOCALAPPDATA%\Flox\cache\images\` | `~/Library/Caches/Flox-dev/cache/images/` |
| Job scratch folders | `%TEMP%\flox\<job id>\` | `$TMPDIR/flox/<job id>/` |
| WebView2 profile (InPrivate) | `%TEMP%\flox\webview2\` | n/a |

The job scratch folders and the WebView2 profile are cleared at launch.
