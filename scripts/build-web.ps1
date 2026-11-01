# Build the browser viewer into crates/web/www. Serve that folder over
# http(s), e.g. `python -m http.server -d crates/web/www 8000`.
#
# Needs: rustup target add wasm32-unknown-unknown
#        cargo install wasm-bindgen-cli --version <same as Cargo.lock>
#
# Usage: scripts\build-web.ps1 [-CargoProfile dev]   (or set $env:PROFILE)
param(
    [string]$CargoProfile = $(if ($env:PROFILE) { $env:PROFILE } else { "release" })
)
$ErrorActionPreference = "Stop"

Push-Location (Join-Path $PSScriptRoot "..")
try {
    cargo build -p schematic-wgpu-web --target wasm32-unknown-unknown --profile $CargoProfile
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    $dir = if ($CargoProfile -eq "dev") { "debug" } else { $CargoProfile }
    $bindgen = Get-Command wasm-bindgen -ErrorAction SilentlyContinue
    if ($bindgen) {
        $bindgen = $bindgen.Source
    } else {
        $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME ".cargo" }
        $bindgen = Join-Path $cargoHome "bin\wasm-bindgen.exe"
    }
    & $bindgen --target web --no-typescript --out-dir crates/web/www/pkg "target/wasm32-unknown-unknown/$dir/web.wasm"
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    Write-Host "built crates/web/www (serve it and open index.html)"
} finally {
    Pop-Location
}
