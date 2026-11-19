# Regenerates THIRD-PARTY-RUST.txt (licences of every crate in Cargo.lock) with cargo-about.
# The result is committed, so packing a release does not need cargo-about installed.
# Needs: cargo install cargo-about --locked --features cli
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

if (-not (Get-Command cargo-about -ErrorAction SilentlyContinue)) {
    throw 'cargo-about is not installed. Run: cargo install cargo-about --locked --features cli'
}
Push-Location $repo
try {
    $out = Join-Path $repo 'THIRD-PARTY-RUST.txt'
    cargo about generate about.hbs --locked -o $out
    if ($LASTEXITCODE -ne 0) { throw 'cargo about failed; a new licence may need adding to "accepted" in about.toml.' }
    Write-Host "Wrote $out"
}
finally {
    Pop-Location
}
