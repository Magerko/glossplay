# Builds the release exe without the builder's directories baked into panic messages.
# cargo's trim-paths is still nightly-only, so I remap the paths with rustc flags instead.
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }

# Encoded flags are separated by 0x1f, so a profile path with spaces survives.
$flags = @()
if ($env:CARGO_ENCODED_RUSTFLAGS) {
    $flags += $env:CARGO_ENCODED_RUSTFLAGS -split [char]0x1f
} elseif ($env:RUSTFLAGS) {
    $flags += $env:RUSTFLAGS -split '\s+' | Where-Object { $_ }
}
$flags += "--remap-path-prefix=$cargoHome=/cargo"
$flags += "--remap-path-prefix=$rustupHome=/rustup"
$flags += "--remap-path-prefix=$repo=/src"
$env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]0x1f

Push-Location $repo
try {
    cargo build --release --locked -p glossplay
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
} finally {
    Pop-Location
}
