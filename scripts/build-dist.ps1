$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    $cargoToml = Get-Content -Raw "Cargo.toml"
    if ($cargoToml -match '(?m)^version\s*=\s*"([^"]+)"') {
        $version = $Matches[1]
    } else {
        throw "Could not read version from Cargo.toml"
    }

    $targetRoot = Join-Path $repoRoot "target"
    $defaultReleaseExe = Join-Path $targetRoot "release\puwbar.exe"
    if (Test-Path $defaultReleaseExe) {
        try {
            $fs = [System.IO.File]::Open($defaultReleaseExe, 'Open', 'ReadWrite', 'None')
            $fs.Close()
        } catch {
            $targetRoot = Join-Path $repoRoot "target\dist-build"
            $env:CARGO_TARGET_DIR = $targetRoot
            Write-Host "Note: target\release\puwbar.exe is currently running; building in target\dist-build..." -ForegroundColor Yellow
        }
    }

    Write-Host "[1/3] Building puwbar.exe (release)..." -ForegroundColor Cyan
    cargo build --release --bin puwbar
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    Write-Host "[2/3] Building puwbar-setup.exe (release, embedding puwbar.exe)..." -ForegroundColor Cyan
    cargo build --release -p puwbar-installer --bin puwbar-setup
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    Write-Host "[3/3] Exporting installer to dist/..." -ForegroundColor Cyan
    $distDir = Join-Path $repoRoot "dist"
    New-Item -ItemType Directory -Force -Path $distDir | Out-Null

    $setupSrc = Join-Path $targetRoot "release\puwbar-setup.exe"
    $setupDst = Join-Path $distDir "puwbar-setup-v$version-x64.exe"
    Copy-Item -Force $setupSrc $setupDst

    $sizeMb = [math]::Round((Get-Item $setupDst).Length / 1MB, 2)
    Write-Host "Done: $setupDst ($sizeMb MB)" -ForegroundColor Green
} finally {
    Pop-Location
}
