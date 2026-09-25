<#
  Fetches or builds the third-party binaries (tdjson.dll, libmpv-2.dll, ffmpeg.exe,
  ffprobe.exe, yt-dlp.exe) into .\deps\ and stages them next to the exe in target\<profile>\.
  Pinned URLs, SHA-256 hashes and the TDLib commit live in scripts\deps.lock.json.

  Usage: .\scripts\fetch-deps.ps1 [-Profile release] [-BuildTdlib] [-TdlibZip <path-or-url>] [-Force]

    -Profile     target\<profile>\ to stage into (debug by default)
    -BuildTdlib  build tdjson.dll from tdlib/td at the locked commit
                 (needs Visual Studio 2022 Build Tools with C++, CMake and Git on PATH)
    -TdlibZip    a zip holding a prebuilt tdjson.dll, as a local path or a URL
                 (a URL is checked against tdlib.sha256 in the lock file)
    -Force       download again even when a cached file already matches its hash
#>
param([string]$Profile = "debug", [switch]$BuildTdlib, [string]$TdlibZip, [switch]$Force)
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$root = Split-Path $PSScriptRoot -Parent
$lock = Get-Content "$root\scripts\deps.lock.json" -Raw | ConvertFrom-Json
$deps = "$root\deps"
New-Item -ItemType Directory -Force $deps, "$deps\tools", "$deps\licenses" | Out-Null

# Native commands do not honour $ErrorActionPreference, so check their exit codes.
function Invoke-Native([scriptblock]$Command) {
  & $Command
  if ($LASTEXITCODE -ne 0) { throw "command failed with exit code ${LASTEXITCODE}: $Command" }
}

function Get-Verified([string]$Url, [string]$Sha, [string]$Out) {
  if (-not $Sha) { throw "no SHA-256 pinned for ${Url}" }
  if ((Test-Path $Out) -and -not $Force -and ((Get-FileHash $Out -Algorithm SHA256).Hash -eq $Sha)) {
    Write-Host "cached: $Out"
    return
  }
  Write-Host "downloading: $Url"
  Invoke-WebRequest -Uri $Url -OutFile $Out -UseBasicParsing
  $hash = (Get-FileHash $Out -Algorithm SHA256).Hash
  if ($hash -ne $Sha) {
    Remove-Item $Out -Force
    throw "SHA-256 mismatch for ${Url}: got $hash, expected $Sha"
  }
}

function Find-One([string]$Dir, [string]$Name) {
  $hit = Get-ChildItem $Dir -Recurse -File -Filter $Name | Select-Object -First 1
  if (-not $hit) { throw "$Name not found under $Dir" }
  $hit.FullName
}

# libmpv: a 7z archive. 7-Zip is used when present, otherwise Windows tar.exe (libarchive reads 7z).
Get-Verified $lock.mpv.url $lock.mpv.sha256 "$deps\mpv-dev.7z"
if ($Force -or -not (Test-Path "$deps\libmpv-2.dll")) {
  $sevenZip = (Get-Command 7z -ErrorAction SilentlyContinue).Source
  if (-not $sevenZip -and (Test-Path "$env:ProgramFiles\7-Zip\7z.exe")) { $sevenZip = "$env:ProgramFiles\7-Zip\7z.exe" }
  if ($sevenZip) {
    Invoke-Native { & $sevenZip e "$deps\mpv-dev.7z" "-o$deps" libmpv-2.dll -y | Out-Null }
  } else {
    Invoke-Native { tar -xf "$deps\mpv-dev.7z" -C $deps libmpv-2.dll }
  }
}

# ffmpeg and ffprobe, plus the build's licence text
Get-Verified $lock.ffmpeg.url $lock.ffmpeg.sha256 "$deps\ffmpeg.zip"
if ($Force -or -not (Test-Path "$deps\tools\ffmpeg.exe") -or -not (Test-Path "$deps\tools\ffprobe.exe")) {
  if (Test-Path "$deps\ffmpeg-x") { Remove-Item "$deps\ffmpeg-x" -Recurse -Force }
  Expand-Archive "$deps\ffmpeg.zip" "$deps\ffmpeg-x" -Force
  Copy-Item (Find-One "$deps\ffmpeg-x" "ffmpeg.exe") "$deps\tools\" -Force
  Copy-Item (Find-One "$deps\ffmpeg-x" "ffprobe.exe") "$deps\tools\" -Force
  Copy-Item (Find-One "$deps\ffmpeg-x" "LICENSE") "$deps\licenses\ffmpeg-LICENSE.txt" -Force
  Remove-Item "$deps\ffmpeg-x" -Recurse -Force
}

# yt-dlp
Get-Verified $lock.ytdlp.url $lock.ytdlp.sha256 "$deps\tools\yt-dlp.exe"

# tdjson: a prebuilt zip, a local build at the locked commit, or an existing deps\tdjson.dll
if ($TdlibZip) {
  if ($TdlibZip -match "^https?://") {
    Get-Verified $TdlibZip $lock.tdlib.sha256 "$deps\tdjson.zip"
    $TdlibZip = "$deps\tdjson.zip"
  }
  if (Test-Path "$deps\tdjson-x") { Remove-Item "$deps\tdjson-x" -Recurse -Force }
  Expand-Archive $TdlibZip "$deps\tdjson-x" -Force
  Copy-Item (Find-One "$deps\tdjson-x" "tdjson.dll") $deps -Force
  Remove-Item "$deps\tdjson-x" -Recurse -Force
} elseif ($BuildTdlib) {
  $src = "$deps\td-src"
  $vcpkg = "$deps\vcpkg"
  if (-not (Test-Path "$src\.git")) { Invoke-Native { git clone https://github.com/tdlib/td.git $src } }
  Invoke-Native { git -C $src fetch --quiet origin }
  Invoke-Native { git -C $src checkout --quiet $lock.tdlib.commit }
  if (-not (Test-Path "$vcpkg\vcpkg.exe")) {
    if (-not (Test-Path "$vcpkg\.git")) { Invoke-Native { git clone https://github.com/microsoft/vcpkg.git $vcpkg } }
    Invoke-Native { & "$vcpkg\bootstrap-vcpkg.bat" -disableMetrics }
  }
  Invoke-Native { & "$vcpkg\vcpkg.exe" install gperf:x64-windows openssl:x64-windows-static zlib:x64-windows-static }
  $gperf = Find-One "$vcpkg\installed\x64-windows\tools" "gperf.exe"
  $build = "$src\build"
  # Static OpenSSL, zlib and CRT so tdjson.dll is a single self-contained DLL.
  Invoke-Native {
    cmake -S $src -B $build -A x64 -DCMAKE_BUILD_TYPE=Release `
      "-DCMAKE_TOOLCHAIN_FILE=$vcpkg\scripts\buildsystems\vcpkg.cmake" -DVCPKG_TARGET_TRIPLET=x64-windows-static `
      -DCMAKE_POLICY_DEFAULT_CMP0091=NEW -DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded `
      "-DGPERF_EXECUTABLE=$gperf"
  }
  Invoke-Native { cmake --build $build --config Release --target tdjson --parallel }
  Copy-Item (Find-One "$build\Release" "tdjson.dll") $deps -Force
} elseif (-not (Test-Path "$deps\tdjson.dll")) {
  throw "tdjson.dll missing: pass -BuildTdlib or -TdlibZip"
}

# Stage next to the exe
$out = "$root\target\$Profile"
New-Item -ItemType Directory -Force $out, "$out\tools" | Out-Null
Copy-Item "$deps\tdjson.dll", "$deps\libmpv-2.dll" $out -Force
Copy-Item "$deps\tools\*" "$out\tools\" -Force
Write-Host "deps staged into $out"
