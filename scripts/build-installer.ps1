# Builds glossplay-setup-x64.exe with Inno Setup from an unpacked release folder
# (the "glossplay" folder inside glossplay-windows-x64.zip).
param(
    [Parameter(Mandatory)][string]$SourceDir,
    [Parameter(Mandatory)][string]$OutDir,
    [string]$Version
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

if (-not $Version) {
    $cargo = Get-Content (Join-Path $repo 'Cargo.toml')
    $inSection = $false
    foreach ($line in $cargo) {
        if ($line -match '^\s*\[(.+)\]\s*$') { $inSection = ($Matches[1] -eq 'workspace.package'); continue }
        if ($inSection -and $line -match '^\s*version\s*=\s*"([^"]+)"') { $Version = $Matches[1]; break }
    }
    if (-not $Version) { throw 'Could not read [workspace.package] version from Cargo.toml; pass -Version.' }
}

$SourceDir = (Resolve-Path $SourceDir).Path
foreach ($f in 'glossplay.exe', 'libmpv-2.dll', 'LICENSE') {
    if (-not (Test-Path (Join-Path $SourceDir $f))) { throw "$f not found in $SourceDir" }
}
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

$iscc = $null
$onPath = Get-Command ISCC.exe -ErrorAction SilentlyContinue
if ($onPath) { $iscc = $onPath.Source }
if (-not $iscc) {
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe')
    )
    $iscc = $candidates | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
}
if (-not $iscc) { throw 'ISCC.exe not found. Install it with: winget install JRSoftware.InnoSetup' }

& $iscc "/DSourceDir=$SourceDir" "/DOutDir=$OutDir" "/DAppVersion=$Version" (Join-Path $repo 'installer\glossplay.iss')
if ($LASTEXITCODE -ne 0) { throw "ISCC failed with exit code $LASTEXITCODE" }

$setup = Join-Path $OutDir 'glossplay-setup-x64.exe'
$size = [Math]::Round((Get-Item $setup).Length / 1MB, 2)
$hash = (Get-FileHash $setup -Algorithm SHA256).Hash
Write-Host "Installer: $setup"
Write-Host "Version:   $Version"
Write-Host "Size:      $size MB"
Write-Host "SHA-256:   $hash"
