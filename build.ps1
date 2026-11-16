<#
.SYNOPSIS
    Builds the GhostPerf Cargo workspace (agent, tray, ui).

.PARAMETER Debug
    Build in debug mode instead of release.

.PARAMETER Test
    Run the workspace test suite after building.
#>
param(
    [switch]$Debug,
    [switch]$Test
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Push-Location $repoRoot

try {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "cargo not found on PATH. Install the Rust toolchain from https://rustup.rs first."
    }

    # Wrap in @() so a single-element result stays an array; otherwise splatting a
    # bare string passes it to cargo character by character.
    $profileArgs = @(if (-not $Debug) { "--release" })
    $profileName = if ($Debug) { "debug" } else { "release" }

    Write-Host "Building workspace ($profileName)..." -ForegroundColor Cyan
    & cargo build --workspace @profileArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }

    if ($Test) {
        Write-Host "Running tests..." -ForegroundColor Cyan
        & cargo test --workspace @profileArgs
        if ($LASTEXITCODE -ne 0) { throw "cargo test failed with exit code $LASTEXITCODE" }
    }

    $outDir = Join-Path $repoRoot "target\$profileName"
    Write-Host ""
    Write-Host "Build succeeded. Binaries in $outDir" -ForegroundColor Green
    foreach ($name in @("ghostperf-agent.exe", "ghostperf-tray.exe", "ghostperf-ui.exe")) {
        $path = Join-Path $outDir $name
        if (Test-Path $path) {
            Write-Host "  $path"
        }
    }
}
finally {
    Pop-Location
}
