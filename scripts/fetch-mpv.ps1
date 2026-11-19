# Downloads libmpv-2.dll from the latest shinchiro mpv build and puts it next to the
# built program, where LinguaPlay looks for it first.
param(
    [string[]]$Destination
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

if (-not $Destination) {
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $PSScriptRoot '..\target' }
    $Destination = @((Join-Path $target 'debug'), (Join-Path $target 'release'))
}

$release = Invoke-RestMethod 'https://api.github.com/repos/shinchiro/mpv-winbuild-cmake/releases/latest'
$asset = $release.assets | Where-Object { $_.name -match '^mpv-dev-x86_64-\d' } | Select-Object -First 1
if (-not $asset) { throw 'No mpv-dev-x86_64 archive in the latest release.' }

$work = Join-Path ([IO.Path]::GetTempPath()) "linguaplay-mpv-$($release.tag_name)"
New-Item -ItemType Directory -Force $work | Out-Null
$archive = Join-Path $work $asset.name
if (-not (Test-Path $archive)) {
    Write-Host "Downloading $($asset.name)"
    Invoke-WebRequest $asset.browser_download_url -OutFile $archive
}
# The tar that ships with Windows 10 and 11 reads 7z archives.
& "$env:SystemRoot\System32\tar.exe" -xf $archive -C $work libmpv-2.dll
if ($LASTEXITCODE -ne 0) { throw 'Could not unpack the archive.' }

foreach ($dir in $Destination) {
    New-Item -ItemType Directory -Force $dir | Out-Null
    Copy-Item (Join-Path $work 'libmpv-2.dll') $dir -Force
    Write-Host "libmpv-2.dll -> $dir"
}
