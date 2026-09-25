# Windows acceptance checklist

Manual checks for what can only be verified on a real Windows 10 or 11 machine. Everything else is
covered by `scripts/verify.sh` and CI (see the README).

Run the checks against a release build started from the unzipped `Flox-1.0.0-win-x64.zip` (not from
`cargo run`), unless an item says otherwise. Set `FLOX_LOG=info` for the whole session, and
`FLOX_LOG=info,flox_web=debug` for section 4, so the log shows what the items refer to. Tick each
item, and write what you saw next to any item that fails.

## 1. First launch, folders and identity

- [ ] After the first launch, `%APPDATA%\Flox\settings.json` exists once a setting is changed.
- [ ] `%LOCALAPPDATA%\Flox\tdlib` and `%LOCALAPPDATA%\Flox\cache\images` are created after login and browsing.
- [ ] Job scratch folders appear under `%TEMP%\flox\<uuid>` while a queue job runs.
- [ ] Temp sweep: quit Flox with a folder and a file left in `%TEMP%\flox` (and a `%TEMP%\flox\webview2` profile from a VidLink play), then relaunch. `%TEMP%\flox` is empty afterwards, and nothing else in `%TEMP%` was touched.
- [ ] On the first launch, `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Flox.lnk` is created and points at `flox.exe`.
- [ ] In PowerShell, `(New-Object -ComObject Shell.Application).NameSpace("$env:APPDATA\Microsoft\Windows\Start Menu\Programs").ParseName("Flox.lnk").ExtendedProperty("System.AppUserModel.ID")` prints `Flox`.
- [ ] Delete `Flox.lnk` and relaunch. The shortcut is recreated and toasts still show as Flox.
- [ ] The taskbar button groups under the Flox identity (pinning it and relaunching reuses the same button).
- [ ] With the Evergreen WebView2 runtime installed, startup detects its version (visible in the log at debug level).

## 2. Telegram and Settings changes while running

- [ ] Start with no Telegram API id and hash (Home's library row reads SET UP TELEGRAM). Enter both in Settings. Without relaunching, the ACCOUNT row moves to SIGNED OUT, and SIGN IN opens Login with a QR code that signs in. The library row then loads.
- [ ] Signed in, change the API id or hash to another valid app's. The log shows `TDLib restarted with new credentials`, the ACCOUNT row goes through CONNECTING, the app never aborts with `Receive must not be called simultaneously from two different threads`, and signing in again works.
- [ ] Clear the API id and hash. The ACCOUNT row reads NOT CONFIGURED and the library row SET UP TELEGRAM. Entering them again reconnects without a relaunch.
- [ ] After changing the account, the next queue job uploads to the library channel of the new account.
- [ ] Set the ffmpeg path to a different ffmpeg build's folder. The TOOLS row shows the new `ffmpeg.exe`, the log shows `tools resolved again`, and the next queue job runs with it (Process Explorer shows the new path).
- [ ] Rename `tools\ffmpeg.exe` away and launch (the Details ingest bar is missing). Point the ffmpeg path at a working ffmpeg in Settings and reopen a title: the ingest bar is back and a job runs.

## 3. Player (mpv underlay)

`flox.exe --dev-play <file>` opens the player on a local file without TMDB or Telegram. mpv's
`hwdec-current` and the `frame-drop-count` line (logged when playback stops) show at `FLOX_LOG=info`.

- [ ] Library playback of a split upload (2 parts): the video starts under the overlay, the overlay (scrim, title, seek bar, times, buttons) draws on top of the picture, and the seek bar's grey buffered band grows ahead of the white played band.
- [ ] Seek across the part boundary of that upload (hold RIGHT on the seek bar, then click past the boundary). Playback continues without PLAYBACK FAILED and the audio stays in sync.
- [ ] Quality switch: on a title uploaded in two qualities, QUALITY restarts the other print at the same position with the hint `QUALITY · <label>`, and the next playback of that title uses it.
- [ ] Audio labels: AUDIO opens the 480 px panel with one row per language/codec/channels (for example `English · E-AC3 · 5.1`); picking a row switches the track and shows `AUDIO · …`.
- [ ] Subtitles: a library print with a subtitle offers SUBTITLES; M (or a right click) cycles through the tracks and `SUBTITLES OFF`, and the text is readable at each SUBTITLE SIZE.
- [ ] Loudness boost on (Settings) is audibly louder than off on the same scene, with no clipping on loud passages.
- [ ] Autoplay next: with AUTOPLAY NEXT on, the end of an episode starts the next one (`NEXT · S1 E4`); with it off, the NEXT button appears on the overlay instead.
- [ ] The log shows `hwdec-current: <api>` (for example `d3d11va` or `dxva2`), not `no`, for a 4K HEVC 10-bit file.
- [ ] Play 60 s of a 4K HEVC 10-bit file and close the player. The `frame-drop-count N of M frames` line shows N below 1 % of M.
- [ ] Space, Enter, arrows (with acceleration while held), Esc/Backspace, M, Ctrl+R and F11 behave as the README's key list says; moving the mouse shows the overlay and clicking the video toggles play.
- [ ] Keep-awake: while the player plays, an elevated `powercfg /requests` lists `flox.exe` under both `DISPLAY` and `SYSTEM`; after closing the player it is no longer under `DISPLAY` (section 5 covers `SYSTEM` while the queue runs).

## 4. VidLink: sniffer, native playback and page player

Signed out of Telegram (or on a title with no library print), PLAY takes the VidLink path. The
sniffer uses the profile `%TEMP%\flox\webview2\sniffer` and the page player `%TEMP%\flox\webview2\page`.

### Sniffer and native playback

- [ ] Play a TV episode (for example Game of Thrones S1E1, `https://vidlink.pro/tv/1399/1/1`) from Details. A manifest arrives and mpv starts in under 20 s (the watchdog allows 45 s): the video plays under the Flox overlay (not in a WebView), the log shows no `sniff:` warning, the eyebrow reads `S<n> · E<n>`, and the seek bar and times work.
- [ ] SUBTITLES offers the page's captions, matching the page's caption list, with the preferred subtitle language first.
- [ ] Do the same with a movie (for example Inception, `https://vidlink.pro/movie/27205`). A manifest arrives in under 20 s.
- [ ] During both sniffs nothing appears on screen or in the taskbar, no window takes focus, no sound plays, and Alt+Tab does not list a Flox WebView window.
- [ ] Spy++ (or `Get-Process msedgewebview2`) shows the WebView2 processes while sniffing, and the host window is a 1280x720 `FloxWebViewHost` popup with `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`.
- [ ] Minimise Flox, cover it with another full-screen window, then start a sniff from the queue. It still resolves (the cloaked window is not treated as occluded). If it stalls here, note it: the fallback is an uncloaked window placed at (-32000, -32000).
- [ ] Start two sniffs back to back (play an episode, back out, play the next one). The second resolves to the new episode's manifest, not the previous one.
- [ ] Closing the player during the LOADING stamp (before the sniff answers) leaves no WebView or sound behind, and opening another title right away sniffs the new one.
- [ ] With the network cut before PLAY, the sniff gives up after 45 s, reloads once (`RELOADING PLAYER`), then shows PLAYBACK FAILED with the hint `SNIFF FAILED`; Enter retries.

### Scripts in iframes

- [ ] With DevTools attached to the sniffer (temporarily enable `AreDevToolsEnabled` in a debug build), `window.__floxDocStart` and `window.__floxInstalled` are `true` in the top document and in every cross-origin player iframe.
- [ ] In the same session, check which path reached each iframe: add a `console.log` to the frame fallback in a debug build, or compare the log. Record whether `AddScriptToExecuteOnDocumentCreated` alone reached the cross-origin iframes on this runtime version: ______ (runtime version ______).
- [ ] A `FLOX_MANIFEST` posted from inside an iframe reaches the app (the sniff resolves even when the player lives in an iframe).
- [ ] Ad popups and redirects are blocked: no new windows open, and the log shows `navigation blocked` for ad hosts.
- [ ] The log shows none of these warnings on an up-to-date runtime: `without ICoreWebView2_22`, `without FrameCreated`, `without controller options`, `too old to set the user agent`. Minimum runtimes: FrameCreated 1.0.902.49, frame messages and the frame fallback 1.0.1108.44, InPrivate controller 1.0.1185.39, iframe request filtering 1.0.2478.35, nested frames 1.0.2651.64.

### Page player

Reach it by making native playback fail (for example by blocking the manifest host in the hosts
file), or with `FLOX_FORCE_PAGE=1`, which logs `FLOX_FORCE_PAGE is set: skipping native playback`.

- [ ] The page player appears over the whole player area with its own UI and sound, exactly covering it, and no Flox overlay or hint draws over it.
- [ ] Window resize keeps the page view sized: drag the window edge, move the window, maximize, restore and toggle F11 while the page player is up; within a quarter second each time the page view exactly covers the client area again, with no gap or overflow.
- [ ] Space and Enter toggle play and pause.
- [ ] Left and Right seek the page video; the page's own UI does not also react twice.
- [ ] Holding Enter or pressing Up/Down enters navigation mode with a focus outline; the arrows move it between page controls, and Enter activates the focused control.
- [ ] M opens the page's settings panel. Esc closes an open panel, then leaves navigation mode, then leaves the player.
- [ ] Ctrl+R reloads the page at the current position.
- [ ] The saved position and a playback speed other than 1.0 are applied once the page video is ready.
- [ ] Progress is saved from the page's ticks: Continue Watching shows the right time after leaving the page player. At the end of a TV episode with AUTOPLAY NEXT on, the next episode opens in the page player.
- [ ] Leaving the player stops the page's sound at once and the WebView disappears.
- [ ] The page player is not muted even while a sniff runs in the muted sniffer at the same time.

## 5. Queue and ingest

- [ ] Queue a VidLink rip for an episode. The job gets past "resolving" within 60 s, and English captions are downloaded when the page lists them.
- [ ] Cancel a queue job while it is resolving. The job cancels at once and the next job's sniff still works.
- [ ] LOCAL FILES on a TV title allows selecting several files; on a movie it allows only one.
- [ ] The file picker opens with the **Video** filter selected, and **All files** shows every file.
- [ ] Start a queue job. An elevated `powercfg /requests` lists `flox.exe` under `SYSTEM`.
- [ ] Close the player while the queue keeps running. `flox.exe` stays under `SYSTEM` and leaves `DISPLAY`.
- [ ] Let the queue drain and close the player. `powercfg /requests` no longer lists `flox.exe` anywhere.
- [ ] When the queue drains, a toast titled `Queue finished` (or `Queue finished with failures`) appears, attributed to **Flox**, not to Windows PowerShell.
- [ ] The toast is also listed under **Flox** in the Notification Center, and Flox appears in Settings > System > Notifications.

## 6. Media keys

- [ ] Open the player on a library title. The Windows media flyout (volume keys or Win+A media area) shows the title with Play/Pause, Previous and Next.
- [ ] The keyboard Play/Pause media key pauses playback, and pressing it again resumes. The flyout status follows.
- [ ] The Next and Previous media keys move to the next and previous episode (TV).
- [ ] Bluetooth headset play/pause buttons behave like the keyboard key.
- [ ] Media keys work while another app has focus.
- [ ] After closing the player, the flyout no longer shows Flox and the media keys go to other apps.

## 7. Missing dependencies

- [ ] Without the WebView2 runtime (test on a machine without it, or rename `%ProgramFiles(x86)%\Microsoft\EdgeWebView` away): the app starts and reports the runtime as missing, and playing a VidLink title fails with a WebView2 unavailable message instead of hanging or crashing.
- [ ] Rename `libmpv-2.dll` away and play a VidLink title: the player falls back to the page player instead of crashing. If the page player cannot load either, PLAYBACK FAILED shows the hint `LIBMPV NOT FOUND · PAGE PLAYER UNAVAILABLE`.
- [ ] Rename `tdjson.dll` away and launch: Settings' ACCOUNT row reads TDLIB NOT FOUND, and browsing and VidLink playback still work.
