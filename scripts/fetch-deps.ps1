<#
  Fetches or builds the third-party binaries (tdjson.dll, libmpv-2.dll, ffmpeg.exe,
  ffprobe.exe, yt-dlp.exe) into .\deps\ and stages them next to the exe in target\<profile>\.
  Pinned URLs and SHA-256 hashes live in scripts\deps.lock.json.

  Usage: .\scripts\fetch-deps.ps1 [-Profile release] [-BuildTdlib] [-TdlibZip <path-or-url>] [-Force]

  Placeholder: the body is written with the packaging work.
#>
param([string]$Profile = "debug", [switch]$BuildTdlib, [string]$TdlibZip, [switch]$Force)
$ErrorActionPreference = "Stop"
throw "fetch-deps.ps1 is not written yet"
