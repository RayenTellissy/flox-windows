# Windows acceptance checklist

Manual checks for behaviour that can only be verified on a real Windows 10/11
machine. Run them against a release build started from the unzipped folder
(not from `cargo run`), unless an item says otherwise.

## Platform services (flox-sys)

### Folders

- [ ] After the first launch, `%APPDATA%\Flox\settings.json` exists once a setting is changed.
- [ ] `%LOCALAPPDATA%\Flox\tdlib` and `%LOCALAPPDATA%\Flox\cache\images` are created after login and browsing.
- [ ] Job scratch folders appear under `%TEMP%\flox\<uuid>` while a queue job runs.

### Keep-awake

- [ ] Start a queue job. In an elevated prompt, `powercfg /requests` lists `flox.exe` under `SYSTEM`.
- [ ] Open the player and start playback. `powercfg /requests` lists `flox.exe` under both `DISPLAY` and `SYSTEM`.
- [ ] Close the player while the queue keeps running. `flox.exe` stays under `SYSTEM` and leaves `DISPLAY`.
- [ ] Let the queue drain and close the player. `powercfg /requests` no longer lists `flox.exe` anywhere.

### Toasts and app identity

- [ ] On the first launch, `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Flox.lnk` is created and points at `flox.exe`.
- [ ] In PowerShell, `(New-Object -ComObject Shell.Application).NameSpace("$env:APPDATA\Microsoft\Windows\Start Menu\Programs").ParseName("Flox.lnk").ExtendedProperty("System.AppUserModel.ID")` prints `Flox`.
- [ ] Let a queue drain. A toast titled `Queue finished` (or `Queue finished with failures`) appears, attributed to **Flox**, not to Windows PowerShell.
- [ ] The toast is also listed under **Flox** in the Notification Center, and Flox appears in Settings > System > Notifications.
- [ ] Delete `Flox.lnk` and relaunch. The shortcut is recreated and toasts still show as Flox.
- [ ] The taskbar button groups under the Flox identity (pinning it and relaunching reuses the same button).

### Media keys

- [ ] Open the player on a library title. The Windows media flyout (volume keys or Win+A media area) shows the title with Play/Pause, Previous and Next.
- [ ] The keyboard Play/Pause media key pauses playback, and pressing it again resumes. The flyout status follows.
- [ ] The Next and Previous media keys move to the next and previous episode (TV).
- [ ] Bluetooth headset play/pause buttons behave like the keyboard key.
- [ ] Media keys work while another app has focus.
- [ ] After closing the player, the flyout no longer shows Flox and the media keys go to other apps.

### File picker

- [ ] "Choose files" on a TV title allows selecting several files. On a movie it allows only one.
- [ ] The picker opens with the **Video** filter selected, and **All files** shows every file.

### WebView2 runtime

- [ ] With the Evergreen runtime installed, startup detects its version (visible in the log at debug level).
- [ ] On a machine without the runtime, the app reports it as missing instead of crashing.

## WebView2 host, sniffer and page player (flox-web)

Run these with `RUST_LOG=flox_web=debug` so the host's warnings and blocked navigations show
in the log. The sniffer uses `%TEMP%\flox\webview2\sniffer` and the page player
`%TEMP%\flox\webview2\page`.

### Sniffer

- [ ] Play a known TV episode (for example Game of Thrones S1E1, `https://vidlink.pro/tv/1399/1/1`) from Details. A manifest arrives and mpv starts in under 20 s.
- [ ] Do the same with a movie (for example Inception, `https://vidlink.pro/movie/27205`). A manifest arrives in under 20 s.
- [ ] During both sniffs nothing appears on screen or in the taskbar, no window takes focus, no sound plays, and Alt+Tab does not list a Flox WebView window.
- [ ] Spy++ (or `Get-Process msedgewebview2`) shows the WebView2 processes while sniffing, and the host window is a 1280x720 `FloxWebViewHost` popup with `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`.
- [ ] Minimise Flox, cover it with another full-screen window, then start a sniff from the queue. It still resolves (the cloaked window is not treated as occluded). If it stalls here, note it: the fallback is an uncloaked window placed at (-32000, -32000).
- [ ] Queue a VidLink rip for an episode. The job gets past "resolving" within 60 s, and English captions are downloaded when the page lists them.
- [ ] Captions offered in the player after a VidLink sniff match the page's caption list.
- [ ] Cancel a queue job while it is resolving. The job cancels at once and the next job's sniff still works.
- [ ] Start two sniffs back to back (play an episode, back out, play the next one). The second resolves to the new episode's manifest, not the previous one.
- [ ] Rename `%ProgramFiles(x86)%\Microsoft\EdgeWebView` away (or test on a machine without the runtime). Playing a VidLink title fails with a "WebView2" unavailable message instead of hanging or crashing.

### Scripts in iframes

- [ ] With DevTools attached to the sniffer (temporarily enable `AreDevToolsEnabled` in a debug build), `window.__floxDocStart` and `window.__floxInstalled` are `true` in the top document and in every cross-origin player iframe.
- [ ] In the same session, check which path reached each iframe: add a `console.log` to the frame fallback in a debug build, or compare the log. Record here whether `AddScriptToExecuteOnDocumentCreated` alone reached the cross-origin iframes on this runtime version: ______ (runtime version ______).
- [ ] A `FLOX_MANIFEST` posted from inside an iframe reaches the app (the sniff resolves even when the player lives in an iframe).
- [ ] Ad popups and redirects are blocked: no new windows open, and the log shows `navigation blocked` for ad hosts.
- [ ] The log shows none of these warnings on an up-to-date runtime: `without ICoreWebView2_22`, `without FrameCreated`, `without controller options`, `too old to set the user agent`. Minimum runtimes: FrameCreated 1.0.902.49, frame messages and the frame fallback 1.0.1108.44, InPrivate controller 1.0.1185.39, iframe request filtering 1.0.2478.35, nested frames 1.0.2651.64.

### Page player

- [ ] Force the page fallback (make native playback fail, for example by blocking the manifest host in the hosts file). The page player appears over the player area with sound, exactly covering it, and follows window resizes and moves.
- [ ] Space toggles play and pause in the page player.
- [ ] Left and Right seek the page video; the page's own UI does not also react twice.
- [ ] Entering navigation mode shows a focus outline; Up, Down, Left and Right move it between page controls, and Enter activates the focused control.
- [ ] Opening the page's settings panel works, and Back closes an open panel before it leaves the player.
- [ ] The start position and playback speed from settings are applied once the page video is ready.
- [ ] Progress is saved from the page's ticks (Continue Watching shows the right time after leaving the page player).
- [ ] Leaving the player stops the page's sound at once and the WebView disappears.
- [ ] The page player is not muted even while a sniff runs in the muted sniffer at the same time.

## Player (mpv underlay)

Run with `FLOX_LOG=info` so mpv's `hwdec-current` and the `frame-drop-count` line (logged when
playback stops) show in the log. `flox.exe --dev-play <file>` opens the player on a local file
without TMDB or Telegram.

- [ ] Library playback of a split upload (2 parts): the video starts under the overlay, the overlay (scrim, title, seek bar, times, buttons) draws on top of the picture, and the seek bar's grey buffered band grows ahead of the white played band.
- [ ] Seek across the part boundary of that upload (hold RIGHT on the seek bar, then click past the boundary). Playback continues without PLAYBACK FAILED and the audio stays in sync.
- [ ] Quality switch: on a title uploaded in two qualities, QUALITY restarts the other print at the same position with the hint `QUALITY · <label>`, and the next playback of that title uses it.
- [ ] Audio labels: AUDIO opens the 480 px panel with one row per language/codec/channels (for example `English · E-AC3 · 5.1`); picking a row switches the track and shows `AUDIO · …`.
- [ ] Subtitles: a library print with a subtitle offers SUBTITLES; M (or a right click) cycles through the tracks and `SUBTITLES OFF`, and the text is readable at each SUBTITLE SIZE.
- [ ] Loudness boost on (Settings) is audibly louder than off on the same scene, with no clipping on loud passages.
- [ ] Autoplay next: with AUTOPLAY NEXT on, the end of an episode starts the next one (`NEXT · S1 E4`); with it off, the NEXT button appears on the overlay instead.
- [ ] The log shows `hwdec-current: <api>` (for example `d3d11va` or `dxva2`), not `no`, for a 4K HEVC 10-bit file.
- [ ] Play 60 s of a 4K HEVC 10-bit file and close the player. The `frame-drop-count N of M frames` line shows N below 1 % of M.
- [ ] Space, ENTER, arrows (with acceleration while held), Esc/Backspace, M, Ctrl+R and F11 behave as in the plan's key table; moving the mouse shows the overlay and clicking the video toggles play.
- [ ] While the player is open `powercfg /requests` lists `flox.exe` under `DISPLAY`; after closing it, `DISPLAY` no longer lists it.
- [ ] Rename `libmpv-2.dll` away and play something: the player shows PLAYBACK FAILED with `LIBMPV NOT FOUND` (or falls back to the page player on a VidLink title) instead of crashing.
