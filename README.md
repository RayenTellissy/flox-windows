# Flox for Windows

Flox for Windows is one desktop app that does the work of the two existing Flox apps:

- **The player** (from the Android TV app, `../flox`): browse TMDB, play a title from the Telegram library channel or from VidLink, with the same player overlay, audio and subtitle tracks, quality switch, resume and autoplay, and the same settings.
- **The ingest tool** (from the Mac app, `../flox-mac`): queue VidLink, pasted-link, 4KHDHub and local-file jobs. Each job is downloaded or remuxed with ffmpeg, split into 2 GB parts and uploaded to the library channel, where the player finds it.

It is written in Rust. The UI is built with Slint, libmpv plays video under the Slint overlay, TDLib talks to Telegram, and WebView2 loads the VidLink pages (to sniff streams, and as a fallback page player). The design is dark and monochrome. The app is keyboard-first with full mouse support.

Version: **1.0.0**.

## Screens

| Screen | What it does |
|---|---|
| Home | SEARCH · QUEUE · LIBRARY · SETTINGS, a Telegram status dot, and the rows CONTINUE WATCHING, LIBRARY, TRENDING MOVIES and TRENDING TV |
| Search | TMDB movie and TV search, debounced |
| Details | Header, seasons and episodes with library stamps, PLAY / RESUME, and the ingest bar: VIDLINK, PASTE LINKS, LOCAL FILES, 4KHDHUB (with episode selection) |
| Queue | Jobs with state, detail and progress; RETRY, REMOVE, CANCEL, CLEAR FINISHED |
| Library | The channel's uploads grouped by title, with DELETE (confirmed) |
| Settings | Playback, audio, subtitles, interface and library preferences; ACCOUNT (TMDB key, Telegram API id and hash, sign in or out); TOOLS (ffmpeg and yt-dlp paths); version |
| Login | Telegram QR login, or phone number, code and two-step password |
| Player | mpv under the Flox overlay; the VidLink page player when native playback fails |

Keys: arrows move the focus, Enter is CENTER, Esc (or Backspace outside text fields) is BACK, Space plays and pauses or toggles a selection, M or the context-menu key is MENU, Ctrl+F searches, Ctrl+, opens Settings, Ctrl+R reloads the player, F11 toggles fullscreen. Media keys work through the system media controls.

## Layout

```
Cargo.toml              workspace (version, shared dependencies, lints)
rust-toolchain.toml     Rust 1.95 + x86_64-pc-windows-msvc
.cargo/config.toml      `cargo wcheck`: clippy for the Windows target
assets/
  fonts/                Geist and Geist Mono (Regular, Medium)
  icons/                player and UI glyphs
  web/                  adblock.js, flox_nav.js, shim.js, tap.js (scripts injected into VidLink pages)
  flox.ico, flox-256.png
crates/
  flox-core/            models, settings, watch progress, TMDB, image cache, paths, tool resolver, languages, sniff contract
  flox-td/              TDLib JSON client (tdjson loaded at run time), auth, library index, uploads, streaming
  flox-rip/             ingest pipeline: downloaders, 4KHDHub, ffmpeg and ffprobe, splitter, job queue
  flox-player/          libmpv wrapper (loaded at run time), stream callbacks, OpenGL render API, tracks
  flox-web/             request policy, injected scripts, bridge messages; WebView2 host, sniffer and page player on Windows
  flox-sys/             folders, keep-awake, toasts and app identity, media keys, file dialogs, WebView2 check
  flox-app/             the `flox` binary
    src/main.rs         builds the runtime, stores and services, then opens the window
    src/launch.rs       starts Telegram and the queue, sweeps the temp folder, restarts them when Settings change
    src/app.rs          services, the shell (router, focus, loads) and the event loop
    src/app/, src/ingest_shell.rs   Settings, Login, ingest dialogs, Queue and Library manager
    src/vm/             pure view models, one per screen
    src/player/         the player controller (pure), the mpv engine, the view and the page path
    ui/                 Slint markup
    fixtures/           offline data for `--dev-fixtures` and the snapshot tests
docs/
  windows-acceptance.md the manual checklist for a real Windows machine
scripts/
  verify.sh             the verification matrix (below)
  fetch-deps.ps1        fetches or builds the third-party binaries (Windows)
  package.ps1           builds the portable zip (Windows)
  deps.lock.json        pinned URLs, SHA-256 hashes and the TDLib commit
  licenses/             third-party licence texts shipped in the zip
  make-icon.py          renders assets/flox.ico and assets/flox-256.png
.github/workflows/windows.yml   build, test and package on windows-latest
```

Neither libmpv nor tdjson is linked at build time. Both are loaded with `libloading` from the resolved path. If one is missing, the app shows a readable state (`TDLIB NOT FOUND`, `LIBMPV NOT FOUND`) instead of failing at startup.

## How it runs

- One tokio runtime serves every background task; the Slint event loop is the UI thread. The view models are pure and run on the UI thread, and results come back to it from the runtime.
- There is one TDLib client per process (`td_receive` allows no more). When the Telegram API id or hash changes in Settings, the running TDLib instance is closed, the app waits for `authorizationStateClosed`, and a new instance starts with the new parameters behind the same client. Login, the library, the player and the queue follow it without a restart. If Telegram was off at launch (no credentials, or tdjson missing), saving credentials starts it.
- The queue runs one job at a time and keeps the machine awake while busy. When the ffmpeg or yt-dlp path changes in Settings, the tools are resolved again and the next job step uses them; if ffmpeg was missing at launch, the queue is created once it resolves.
- Library playback streams the channel's parts to mpv through its stream callbacks (`flox://`). VidLink playback sniffs the page's manifest in a hidden WebView2 and plays it in mpv; if mpv cannot play it, the page itself plays in a visible WebView2 over the player area. Under PLAYBACK FAILED the hint names the cause: `SNIFF FAILED`, `PAGE PLAYER UNAVAILABLE` or `LIBMPV NOT FOUND`.

## Develop on macOS

Everything except WebView2, toasts, media keys, keep-awake and linking `flox.exe` runs on a Mac. The whole workspace also type-checks and lints for `x86_64-pc-windows-msvc` from a Mac, with no linker.

**Prerequisites:**
- rustup. `rust-toolchain.toml` installs Rust 1.95 and the `x86_64-pc-windows-msvc` target.
- Optional: `ffmpeg`, `ffprobe` and `yt-dlp` in `/opt/homebrew/bin` for the ffmpeg and queue tests, and libmpv (`brew install mpv`) for the player tests and real playback.
- TDLib: `scripts/verify.sh` uses the flox-mac vendored `libtdjson.dylib` for the live FFI tests when it is present.

**Run:**

```
cargo run -p flox-app                     # TMDB and Telegram from Settings
cargo run -p flox-app -- --dev-fixtures   # offline catalog, posters and library from crates/flox-app/fixtures/browse.json
cargo run -p flox-app -- --dev-play FILE  # opens the player on a local file
```

Environment variables:

| Variable | Effect |
|---|---|
| `FLOX_LOG` | log filter, for example `info` or `flox_web=debug` (default `info`) |
| `FLOX_HOME` | development builds only: keeps settings, data and temp under this folder (`config`, `data`, `temp`) |
| `FLOX_FORCE_PAGE=1` | skips native VidLink playback and goes straight to the page player |
| `FLOX_TMDB_API_KEY`, `FLOX_TELEGRAM_API_ID`, `FLOX_TELEGRAM_API_HASH` | build time: built-in credentials, used when the Settings value is empty |
| `FLOX_TDJSON`, `FLOX_LIBMPV` | tests: the tdjson and libmpv builds for the live tests (they skip when unset) |

No dependency may compile C for the MSVC target in a build script: that is what lets the workspace type-check for Windows from a Mac. For the same reason reqwest uses `native-tls` (SChannel on Windows), and there is no rustls/ring/aws-lc, openssl-sys, zstd-sys or libsqlite3-sys.

## Verify

```
scripts/verify.sh
```

It runs:

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings` (also `cargo wcheck`)
4. `cargo test --workspace`

If `FLOX_TDJSON` is unset and the flox-mac dylib exists, the script exports it. Tests that need `FLOX_TDJSON` or `FLOX_LIBMPV` skip when those are unset, and the ffmpeg tests skip when the tools are not installed. Tests that reach the live internet are `#[ignore]`; run them with `cargo test -- --ignored`.

The UI snapshot tests render every screen with Slint's software renderer into `target/snapshots/*.png` for review.

| Check | macOS | Windows |
|---|---|---|
| fmt, clippy (host) | yes | yes |
| clippy for x86_64-pc-windows-msvc | yes (no linker needed) | yes |
| Unit tests, UI snapshots | yes | yes |
| ffmpeg, queue and splitter tests | with Homebrew ffmpeg | skipped (they use the Homebrew paths) |
| Live TDLib FFI (start, restart with new parameters, close) | with the vendored dylib | with `tdjson.dll` |
| mpv playback tests | with `brew install mpv` | with `libmpv-2.dll` |
| WebView2, toasts, media keys, keep-awake | no-ops | manual checklist |
| Linking `flox.exe`, packaging | no | yes (CI or a Windows machine) |

What only a real Windows machine can confirm is in **[docs/windows-acceptance.md](docs/windows-acceptance.md)**, one checklist to run against the packaged zip.

## Build on Windows

**Tools:** Visual Studio 2022 Build Tools (the "Desktop development with C++" workload), rustup, Git, CMake and PowerShell 7. 7-Zip is optional; without it `tar.exe` unpacks the mpv archive. The machine needs Windows 10 or 11 x64 with the Evergreen WebView2 runtime (preinstalled on current Windows).

**Dependencies.** `scripts\deps.lock.json` pins each third-party binary by URL and SHA-256:

| Binary | Source |
|---|---|
| `libmpv-2.dll` | shinchiro `mpv-winbuild-cmake`, `mpv-dev-x86_64-v3` (needs an AVX2 CPU) |
| `ffmpeg.exe`, `ffprobe.exe` | gyan.dev essentials build |
| `yt-dlp.exe` | yt-dlp GitHub release |
| `tdjson.dll` | built from tdlib/td at the locked commit (1.8.67, the same commit as the Mac app) |

`fetch-deps.ps1` downloads the archives into `deps\`, checks every hash and stages the binaries into `target\<profile>\` (`tdjson.dll` and `libmpv-2.dll` next to the exe, the tools in `tools\`). It builds TDLib in one of three ways:

- `-BuildTdlib` clones tdlib/td and vcpkg into `deps\` and builds a self-contained `tdjson.dll` with static OpenSSL, zlib and CRT. The first build takes about 30 minutes.
- `-TdlibZip <path-or-url>` takes a prebuilt `tdjson.dll` from a zip, such as the CI's. A URL must match `tdlib.sha256` in the lock file.
- With neither, it reuses an existing `deps\tdjson.dll`.

```
.\scripts\fetch-deps.ps1 -BuildTdlib
$env:FLOX_TDJSON = "$PWD\deps\tdjson.dll"
$env:FLOX_LIBMPV = "$PWD\deps\libmpv-2.dll"
cargo test --workspace
cargo run -p flox-app
cargo build --release
.\scripts\fetch-deps.ps1 -Profile release
.\scripts\package.ps1
```

`package.ps1` writes `dist\Flox-1.0.0-win-x64.zip`:

```
Flox\flox.exe
Flox\tdjson.dll
Flox\libmpv-2.dll
Flox\tools\ffmpeg.exe, ffprobe.exe, yt-dlp.exe
Flox\LICENSES\      mpv (GPLv2+/LGPL), FFmpeg (GPL), TDLib (BSL-1.0), yt-dlp (Unlicense), Geist (OFL)
Flox\README.txt
```

On a Windows host, `crates/flox-app/build.rs` embeds `assets/flox.ico` and the application manifest in `flox.exe`. The manifest declares PerMonitorV2 DPI awareness, long paths, the UTF-8 code page and Common Controls v6. Other hosts skip this step. Release builds use the Windows subsystem (no console window).

**Icon.** `python3 scripts/make-icon.py` (needs Pillow) renders `assets/flox.ico` in 9 sizes (16 to 256 px) and `assets/flox-256.png`. The geometry is the same as the Mac and TV icons.

**Updating a dependency.** Change the URL in `deps.lock.json` and set `sha256` to the output of `shasum -a 256` (or `Get-FileHash`). If you change `tdlib.commit`, also check that the JSON API shapes still match.

**CI.** `.github/workflows/windows.yml` runs on `windows-latest`. It uses Rust 1.95 and runs `fetch-deps.ps1`. `tdjson.dll` is cached by TDLib commit, so TDLib is only rebuilt when the commit changes. The workflow then runs fmt, clippy with `-D warnings` and the tests (with `FLOX_TDJSON` and `FLOX_LIBMPV` set), makes the release build and packages it. The zip is uploaded as the `Flox-win-x64` artifact.

## Credentials

Enter the TMDB API key and the Telegram API id and hash (from my.telegram.org/apps) in Settings. They apply at once: new Telegram credentials restart TDLib in place, and the Login screen follows. They can also be baked in at build time with `FLOX_TMDB_API_KEY`, `FLOX_TELEGRAM_API_ID` and `FLOX_TELEGRAM_API_HASH`. A value saved in Settings takes precedence over the built-in one.

## Tool resolution

For ffmpeg, ffprobe, yt-dlp, tdjson and libmpv, the app searches in this order:

1. The Settings override, when it is set and the file exists (a folder holding the tool also works).
2. The app folder (`<app>\` and `<app>\tools\`).
3. `PATH`.

ffprobe is taken from beside the resolved ffmpeg, else looked up by its own name. The TOOLS rows in Settings show what resolved, and a changed ffmpeg or yt-dlp path applies to the queue at once. tdjson and libmpv are resolved at launch.

## Data locations

| What | Windows | macOS (development) |
|---|---|---|
| Settings (`settings.json`), watch progress (`progress.json`) | `%APPDATA%\Flox\` | `~/Library/Application Support/Flox-dev/` |
| TDLib database and files | `%LOCALAPPDATA%\Flox\tdlib\db`, `%LOCALAPPDATA%\Flox\tdlib\files` | `~/Library/Caches/Flox-dev/tdlib/` |
| Image cache (40 MB on disk) | `%LOCALAPPDATA%\Flox\cache\images\` | `~/Library/Caches/Flox-dev/cache/images/` |
| Job scratch folders | `%TEMP%\flox\<job id>\` | `$TMPDIR/flox/<job id>/` |
| WebView2 profiles (InPrivate) | `%TEMP%\flox\webview2\sniffer`, `...\webview2\page` | n/a |
| Start menu shortcut (toast identity) | `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Flox.lnk` | n/a |

With `FLOX_HOME` set, a development build keeps settings, data and temp under `$FLOX_HOME/config`, `data` and `temp`.

At launch the app empties `%TEMP%\flox` (leftover job folders and WebView2 profiles) before anything writes there. It only sweeps a folder it owns (named `flox`, never the temp folder itself). Run one Flox at a time: a second instance would sweep the first one's running job.
