# Packs a Windows release zip: linguaplay.exe, libmpv-2.dll, licences and READMEs.
param(
    [string]$Exe,
    [string]$Mpv,
    [string]$OutDir,
    [switch]$AllowOtherMpv
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'mpv-version.ps1')

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
$dllHash = (Get-FileHash $Mpv -Algorithm SHA256).Hash
if ($dllHash -ne $MpvDllSha256 -and -not $AllowOtherMpv) {
    throw "libmpv-2.dll ($Mpv) has SHA-256 $dllHash, not the pinned $MpvDllSha256. Run scripts/fetch-mpv.ps1, or pass -AllowOtherMpv to pack it anyway."
}
$rustNotices = Join-Path $repo 'THIRD-PARTY-RUST.txt'
if (-not (Test-Path $rustNotices)) {
    throw 'THIRD-PARTY-RUST.txt is missing. Generate it with scripts/update-licenses.ps1.'
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
    Copy-Item $rustNotices (Join-Path $stage 'THIRD-PARTY-RUST.txt')

    # The images are not shipped, so the archived READMEs must point at the hosted copies.
    $rawBase = 'https://raw.githubusercontent.com/Magerko/linguaplay/main/docs/screenshots/'
    foreach ($name in 'README.md', 'README.ru.md', 'README.uk.md', 'README.zh.md') {
        $text = [IO.File]::ReadAllText((Join-Path $repo $name), [Text.Encoding]::UTF8)
        $text = $text.Replace('docs/screenshots/', $rawBase)
        [IO.File]::WriteAllText((Join-Path $stage $name), $text, $utf8)
    }

    $mpvText = if ($AllowOtherMpv -and $dllHash -ne $MpvDllSha256) {
@"
libmpv-2.dll
  A build of mpv that is NOT the pinned one (SHA-256 $dllHash). This archive was
  packed with -AllowOtherMpv, so the exact source of the DLL is not documented here.
  License: GPL. It includes FFmpeg - https://ffmpeg.org
"@
    }
    else {
@"
libmpv-2.dll
  mpv-winbuild-cmake build $MpvBuildTag by shinchiro (mpv commit $MpvMpvCommit).
  SHA-256 of the DLL: $MpvDllSha256
  License: GPL.
  Build release: https://github.com/shinchiro/mpv-winbuild-cmake/releases/tag/$MpvBuildTag
  Source of mpv at this commit: https://github.com/mpv-player/mpv/tree/$MpvMpvCommit
  The build scripts of that tag decide which versions of FFmpeg and of every other
  library are linked in, and where their sources come from:
  https://github.com/shinchiro/mpv-winbuild-cmake/tree/$MpvBuildTag
  FFmpeg - https://ffmpeg.org (source at the version pinned by that tag's scripts).
"@
    }
    $notice = {
        param($name)
        [IO.File]::ReadAllText((Join-Path $repo "licenses\$name"), [Text.Encoding]::UTF8).TrimEnd()
    }
    $rule = '-' * 78
    $third = @"
Third-party components shipped with LinguaPlay
==============================================

LinguaPlay
  License: GPL-3.0-or-later (see LICENSE).

$mpvText

Rust crates
  Every crate linked into linguaplay.exe, with its licence and copyright notices, is
  listed in THIRD-PARTY-RUST.txt.

Fonts built into the egui user interface (crate epaint_default_fonts 0.37.0)
  With the features LinguaPlay enables, three fonts are embedded:
  - Hack Regular, MIT License, Copyright 2018 Source Foundry Authors. It builds on
    Bitstream Vera (Bitstream Vera License) and DejaVu (public domain).
  - Ubuntu Light, Ubuntu Font Licence 1.0.
  - egui-icons, a five-glyph subset of the emoji-icon-font by John Slegers, MIT License.
  The same crate also contains Noto Emoji (SIL Open Font License 1.1), which is not
  compiled into this build; its licence is included below because the crate carries it.
  The licence texts follow.

$rule
Hack Regular (MIT, Bitstream Vera, DejaVu)
$rule
$(& $notice 'Hack-Regular.txt')

$rule
emoji-icon-font (MIT)
$rule
$(& $notice 'emoji-icon-font-mit-license.txt')

$rule
Ubuntu Font Licence 1.0 (Ubuntu Light)
$rule
$(& $notice 'UFL.txt')

$rule
SIL Open Font License 1.1 (Noto Emoji, not compiled in)
$rule
$(& $notice 'OFL.txt')

Dictionary for Japanese word splitting (crate lindera-ipadic 6.2.0)
  The IPADIC dictionary built into the program is mecab-ipadic-2.7.0-20070801 from NAIST
  and ICOT. The notice that comes with it in the lindera-ipadic crate follows.

$rule
mecab-ipadic notice
$rule
$(& $notice 'mecab-ipadic-NOTICE.txt')
"@
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
