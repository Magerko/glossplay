# Packs a Windows release zip: linguaplay.exe, libmpv-2.dll, licences and READMEs.
param(
    [string]$Exe,
    [string]$Mpv,
    [string]$OutDir
)
$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Exe) {
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
    $Exe = Join-Path $target 'release\linguaplay.exe'
}
if (-not (Test-Path $Exe)) { throw "Executable not found: $Exe. Build it with: cargo build --release -p linguaplay" }
$Exe = (Resolve-Path $Exe).Path
if (-not $Mpv) { $Mpv = Join-Path (Split-Path $Exe -Parent) 'libmpv-2.dll' }
if (-not (Test-Path $Mpv)) {
    throw "libmpv-2.dll not found: $Mpv. Run scripts/fetch-mpv.ps1 first (or pass -Mpv)."
}
if (-not $OutDir) { $OutDir = Join-Path $repo 'dist' }
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

$work = Join-Path ([IO.Path]::GetTempPath()) ("linguaplay-pack-" + [Guid]::NewGuid().ToString('N'))
$stage = Join-Path $work 'linguaplay'
New-Item -ItemType Directory -Force $stage | Out-Null
$utf8 = New-Object System.Text.UTF8Encoding($false)

try {
    Copy-Item $Exe (Join-Path $stage 'linguaplay.exe')
    Copy-Item $Mpv (Join-Path $stage 'libmpv-2.dll')
    Copy-Item (Join-Path $repo 'LICENSE') (Join-Path $stage 'LICENSE')

    # The images are not shipped, so the archived READMEs must point at the hosted copies.
    $rawBase = 'https://raw.githubusercontent.com/Magerko/linguaplay/main/docs/screenshots/'
    foreach ($name in 'README.md', 'README.ru.md', 'README.uk.md', 'README.zh.md') {
        $text = [IO.File]::ReadAllText((Join-Path $repo $name), [Text.Encoding]::UTF8)
        $text = $text.Replace('docs/screenshots/', $rawBase)
        [IO.File]::WriteAllText((Join-Path $stage $name), $text, $utf8)
    }

    $third = @'
Third-party components shipped with LinguaPlay
==============================================

LinguaPlay
  License: GPL-3.0-or-later (see LICENSE).

libmpv-2.dll
  A build of mpv from https://github.com/shinchiro/mpv-winbuild-cmake
  License: GPL.
  Source code: https://github.com/shinchiro/mpv-winbuild-cmake and
  https://github.com/mpv-player/mpv
  It includes FFmpeg - https://ffmpeg.org
'@
    [IO.File]::WriteAllText((Join-Path $stage 'THIRD-PARTY.txt'), $third, $utf8)

    $zip = Join-Path $OutDir 'linguaplay-windows-x64.zip'
    if (Test-Path $zip) { Remove-Item $zip -Force }
    # Compress-Archive in Windows PowerShell 5.1 writes backslashes into entry names, which
    # other unzip tools mishandle; I build the zip by hand to get forward slashes.
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::Open($zip, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($file in Get-ChildItem $stage -File) {
            [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                $archive, $file.FullName, "linguaplay/$($file.Name)", [IO.Compression.CompressionLevel]::Optimal)
        }
    }
    finally {
        $archive.Dispose()
    }
}
finally {
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
}

$size = [Math]::Round((Get-Item $zip).Length / 1MB, 2)
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash
Write-Host "Archive: $zip"
Write-Host "Size:    $size MB"
Write-Host "SHA-256: $hash"
