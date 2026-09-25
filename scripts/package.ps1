<#
  Builds dist\Flox-<version>-win-x64.zip from a release build and the fetched deps:
    Flox\flox.exe, Flox\tdjson.dll, Flox\libmpv-2.dll,
    Flox\tools\{ffmpeg,ffprobe,yt-dlp}.exe, Flox\LICENSES\, Flox\README.txt

  Run after `cargo build --release` and `.\scripts\fetch-deps.ps1`.
  The version comes from [workspace.package] in Cargo.toml.

  Usage: .\scripts\package.ps1
#>
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$root = Split-Path $PSScriptRoot -Parent
$deps = "$root\deps"
$exe = "$root\target\release\flox.exe"

$cargo = Get-Content "$root\Cargo.toml" -Raw
if ($cargo -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') { throw "workspace version not found in Cargo.toml" }
$version = $Matches[1]

$inputs = @(
  $exe,
  "$deps\tdjson.dll",
  "$deps\libmpv-2.dll",
  "$deps\tools\ffmpeg.exe",
  "$deps\tools\ffprobe.exe",
  "$deps\tools\yt-dlp.exe",
  "$deps\licenses\ffmpeg-LICENSE.txt"
)
foreach ($path in $inputs) {
  if (-not (Test-Path $path)) { throw "missing ${path}: run cargo build --release and scripts\fetch-deps.ps1 first" }
}

$lock = Get-Content "$root\scripts\deps.lock.json" -Raw | ConvertFrom-Json

# Staging layout (kept identical for a future MSI)
$dist = "$root\dist"
$stage = "$dist\stage"
$app = "$stage\Flox"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force $app, "$app\tools", "$app\LICENSES" | Out-Null

Copy-Item $exe "$app\flox.exe"
Copy-Item "$deps\tdjson.dll", "$deps\libmpv-2.dll" $app
Copy-Item "$deps\tools\ffmpeg.exe", "$deps\tools\ffprobe.exe", "$deps\tools\yt-dlp.exe" "$app\tools\"
Copy-Item "$root\scripts\licenses\*" "$app\LICENSES\"
Copy-Item "$deps\licenses\ffmpeg-LICENSE.txt" "$app\LICENSES\"

$readme = @"
Flox $version for Windows (x64)
===============================

Flox browses TMDB, plays from a Telegram library channel or VidLink, and queues
ingest jobs that are split and uploaded to the library channel.

Running
-------
Unzip this folder anywhere you can write to and run flox.exe. Nothing is installed
and no administrator rights are needed.

Requirements
------------
- Windows 10 or 11, 64-bit.
- The Microsoft Edge WebView2 runtime, which current Windows already includes.
  Without it the VidLink features are unavailable and the app says so.

First run
---------
Open Settings and enter the TMDB API key, and the Telegram API id and hash from
https://my.telegram.org/apps, then sign in to Telegram.

Where things are kept
---------------------
- Settings and watch progress: %APPDATA%\Flox\
- Telegram database and files, image cache: %LOCALAPPDATA%\Flox\
- Job scratch folders: %TEMP%\flox\ (cleared at launch)

Deleting those folders resets the app.

Bundled components
------------------
- tdjson.dll: TDLib $($lock.tdlib.version) (commit $($lock.tdlib.commit)), Boost Software License 1.0
- libmpv-2.dll: mpv (commit $($lock.mpv.mpvCommit), build $($lock.mpv.tag)), GPLv2 or later
- tools\ffmpeg.exe, tools\ffprobe.exe: FFmpeg $($lock.ffmpeg.tag) (gyan.dev essentials build), GPL
- tools\yt-dlp.exe: yt-dlp $($lock.ytdlp.tag), The Unlicense
- Geist and Geist Mono fonts (embedded in flox.exe), SIL Open Font License 1.1

Each tool in tools\ can be replaced by a newer build, or pointed elsewhere from
Settings. See LICENSES\ for the licence texts and source locations.
"@
Set-Content -Path "$app\README.txt" -Value ($readme -replace "`r?`n", "`r`n") -Encoding utf8 -NoNewline

New-Item -ItemType Directory -Force $dist | Out-Null
$zip = "$dist\Flox-$version-win-x64.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $app -DestinationPath $zip -CompressionLevel Optimal
Write-Host "packaged $zip"
