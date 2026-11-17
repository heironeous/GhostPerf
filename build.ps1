<#
.SYNOPSIS
    Builds the GhostPerf Cargo workspace (agent, tray, ui).

.PARAMETER Debug
    Build in debug mode instead of release.

.PARAMETER Test
    Run the workspace test suite after building.

.PARAMETER Package
    After a release build, also produce dist\GhostPerf-portable-<version>.zip
    (the three exes plus an empty `data` folder — see
    crates/agent_config/src/lib.rs's portable-mode detection) and, if
    `makensis` is on PATH, dist\GhostPerf-Setup-<version>.exe (installer\ghostperf.nsi).
    Implies a release build; incompatible with -Debug.
#>
param(
    [switch]$Debug,
    [switch]$Test,
    [switch]$Package
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Push-Location $repoRoot

try {
    if ($Package -and $Debug) {
        throw "-Package produces a release build; don't combine it with -Debug."
    }

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
    $exeNames = @("ghostperf-agent.exe", "ghostperf-tray.exe", "ghostperf-ui.exe")
    foreach ($name in $exeNames) {
        $path = Join-Path $outDir $name
        if (Test-Path $path) {
            Write-Host "  $path"
        }
    }

    if ($Package) {
        $distDir = Join-Path $repoRoot "dist"
        New-Item -ItemType Directory -Force -Path $distDir | Out-Null

        # Workspace.package.version in Cargo.toml, e.g. `version = "0.1.0"`
        # under `[workspace.package]`. All three crates inherit it
        # (`version.workspace = true`), so there's one number for the
        # whole product.
        $versionLine = Select-String -Path (Join-Path $repoRoot "Cargo.toml") -Pattern '^\s*version\s*=\s*"([^"]+)"' | Select-Object -First 1
        $version = if ($versionLine) { $versionLine.Matches[0].Groups[1].Value } else { "0.0.0" }

        Write-Host ""
        Write-Host "Packaging v$version..." -ForegroundColor Cyan

        # Portable zip: the three exes plus an empty `data` folder, which is
        # the marker crates/agent_config/src/lib.rs's data_dir() checks for
        # — its presence next to the exe is what makes this build portable
        # with no separate flag or rebuild (see TODO.md Phase 5).
        $portableStage = Join-Path $distDir "portable-stage"
        if (Test-Path $portableStage) { Remove-Item -Recurse -Force $portableStage }
        New-Item -ItemType Directory -Force -Path (Join-Path $portableStage "data") | Out-Null
        # A placeholder file, not a truly empty folder: Compress-Archive
        # doesn't reliably zip empty directories, and an empty `data`
        # folder is exactly the portable-mode marker data_dir() looks for
        # — losing it on extraction would silently fall back to installed
        # mode (%APPDATA%) instead.
        Set-Content -Path (Join-Path $portableStage "data\README.txt") `
            -Value "GhostPerf stores its config, database, and logs in this folder when it's present (portable mode). Delete this whole 'data' folder to use %APPDATA% instead."
        foreach ($name in $exeNames) {
            Copy-Item (Join-Path $outDir $name) (Join-Path $portableStage $name)
        }
        $portableZip = Join-Path $distDir "GhostPerf-portable-$version.zip"
        if (Test-Path $portableZip) { Remove-Item $portableZip }
        Compress-Archive -Path (Join-Path $portableStage "*") -DestinationPath $portableZip
        Remove-Item -Recurse -Force $portableStage
        Write-Host "  $portableZip" -ForegroundColor Green

        # Installer: skipped (with a warning, not a failure — it's an
        # optional extra, not a build requirement) if NSIS isn't installed.
        # See AGENTS.md for installing it and for how this is verified
        # without a Windows machine.
        $makensis = Get-Command makensis -ErrorAction SilentlyContinue
        if ($makensis) {
            $nsiPath = Join-Path $repoRoot "installer\ghostperf.nsi"
            & $makensis.Path "/DPRODUCT_VERSION=$version" $nsiPath
            if ($LASTEXITCODE -ne 0) { throw "makensis failed with exit code $LASTEXITCODE" }
            Write-Host "  $distDir\GhostPerf-Setup-$version.exe" -ForegroundColor Green
        }
        else {
            Write-Warning "makensis not found on PATH; skipping the installer. Install NSIS (https://nsis.sourceforge.io/) to produce dist\GhostPerf-Setup-$version.exe too."
        }
    }
}
finally {
    Pop-Location
}
