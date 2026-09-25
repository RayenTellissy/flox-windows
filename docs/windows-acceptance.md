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
