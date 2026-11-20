# Pins the Anki and AnkiConnect downloads that the installer offers (installer\anki-pins.iss).
# It downloads both files, computes their SHA-256 and rewrites the include, so a new version
# is a one-line change here plus a review of the diff.
#   Anki:        official Windows MSI from https://github.com/ankitects/anki/releases
#   AnkiConnect: official source archive of a tag from https://git.sr.ht/~foosoft/anki-connect
# Set GLOSSPLAY_CACHE_DIR to keep the downloads somewhere other than %TEMP%.
param(
    [Parameter(Mandatory)][string]$AnkiVersion,
    [Parameter(Mandatory)][string]$AnkiConnectVersion
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$cacheRoot = if ($env:GLOSSPLAY_CACHE_DIR) { $env:GLOSSPLAY_CACHE_DIR } else { [IO.Path]::GetTempPath() }
New-Item -ItemType Directory -Force $cacheRoot | Out-Null

function Get-Pinned($url, $name) {
    $path = Join-Path $cacheRoot $name
    if (-not (Test-Path $path)) {
        Write-Host "Downloading $name"
        Invoke-WebRequest $url -OutFile $path
    }
    [pscustomobject]@{
        Sha256 = (Get-FileHash $path -Algorithm SHA256).Hash.ToLowerInvariant()
        Bytes  = (Get-Item $path).Length
    }
}

$ankiName = "anki-$AnkiVersion-win-x64.msi"
$ankiUrl = "https://github.com/ankitects/anki/releases/download/$AnkiVersion/$ankiName"
$acName = "anki-connect-$AnkiConnectVersion.zip"
$acUrl = "https://git.sr.ht/~foosoft/anki-connect/archive/$AnkiConnectVersion.zip"

$anki = Get-Pinned $ankiUrl $ankiName
$ac = Get-Pinned $acUrl $acName
$ankiMb = [Math]::Round($anki.Bytes / 1MB)

$text = @"
; Pinned downloads of the optional Anki step. Written by scripts\update-anki-pins.ps1; do not
; edit by hand. The installer refuses a file whose SHA-256 differs and skips that part.
#define AnkiVersion "$AnkiVersion"
#define AnkiFileName "$ankiName"
#define AnkiUrl "$ankiUrl"
#define AnkiSha256 "$($anki.Sha256)"
#define AnkiSizeMB "$ankiMb"
#define AnkiConnectVersion "$AnkiConnectVersion"
#define AnkiConnectFileName "$acName"
#define AnkiConnectUrl "$acUrl"
#define AnkiConnectSha256 "$($ac.Sha256)"
#define AnkiConnectPinnedAt "$([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())"
"@
$out = Join-Path $PSScriptRoot '..\installer\anki-pins.iss'
[IO.File]::WriteAllText($out, ($text -replace "`r?`n", "`r`n") + "`r`n", (New-Object Text.UTF8Encoding $false))
Write-Host "Wrote $out"
Write-Host "Anki $AnkiVersion  $($anki.Sha256)  $ankiMb MB"
Write-Host "AnkiConnect $AnkiConnectVersion  $($ac.Sha256)"
