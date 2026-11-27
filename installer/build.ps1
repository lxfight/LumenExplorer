<#
.SYNOPSIS
    Build the Lumen Windows 11 installer (dist\LumenSetup-<version>-x64.exe).

.PARAMETER SkipCargoBuild
    Skip `cargo build --release` and package the existing target\release\lumen.exe.

.PARAMETER IsccPath
    Explicit path to Inno Setup 6 compiler (ISCC.exe).
#>
[CmdletBinding()]
param(
    [switch]$SkipCargoBuild,
    [string]$IsccPath
)

$ErrorActionPreference = 'Stop'
$RepoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $RepoRoot

try {
    # 1. Read version from Cargo.toml (single source of truth).
    $cargoToml = Get-Content -Path (Join-Path $RepoRoot 'Cargo.toml') -Raw
    if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
        throw 'Failed to read package version from Cargo.toml'
    }
    $version = $Matches[1]
    Write-Host "==> Lumen version: $version" -ForegroundColor Cyan

    # 2. Build release binary unless skipped.
    if (-not $SkipCargoBuild) {
        $cargo = (Get-Command cargo -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -First 1)
        if (-not $cargo) {
            $fallback = 'D:\DevCache\rustup\toolchains\stable-x86_64-pc-windows-msvc\bin\cargo.exe'
            if (Test-Path $fallback) { $cargo = $fallback }
        }
        if (-not $cargo) {
            throw 'cargo.exe not found in PATH.'
        }
        Write-Host "==> Building release binary with $cargo ..." -ForegroundColor Cyan
        & $cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed ($LASTEXITCODE)" }
    }

    $exePath = Join-Path $RepoRoot 'target\release\lumen.exe'
    if (-not (Test-Path $exePath)) {
        throw "Release binary not found at $exePath"
    }

    # 3. Locate Inno Setup 6 compiler (ISCC.exe).
    if (-not $IsccPath) {
        $cmd = Get-Command iscc -ErrorAction SilentlyContinue
        if ($cmd) {
            $IsccPath = $cmd.Source
        } else {
            $candidates = @(
                "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
                'C:\Program Files (x86)\Inno Setup 6\ISCC.exe',
                'C:\Program Files\Inno Setup 6\ISCC.exe'
            )
            foreach ($c in $candidates) {
                if (Test-Path $c) {
                    $IsccPath = $c
                    break
                }
            }
        }
    }

    if (-not $IsccPath -or -not (Test-Path $IsccPath)) {
        throw @"
Inno Setup 6 (ISCC.exe) was not found.
Install it with:
    winget install --id JRSoftware.InnoSetup
or:
    choco install innosetup -y
Then re-run:
    powershell -ExecutionPolicy Bypass -File installer\build.ps1
"@
    }

    # 4. Compile installer.
    $issPath = Join-Path $RepoRoot 'installer\lumen.iss'
    Write-Host "==> Compiling installer with $IsccPath ..." -ForegroundColor Cyan
    & $IsccPath "/DMyAppVersion=$version" $issPath
    if ($LASTEXITCODE -ne 0) { throw "ISCC.exe failed ($LASTEXITCODE)" }

    $outExe = Join-Path $RepoRoot "dist\LumenSetup-$version-x64.exe"
    if (-not (Test-Path $outExe)) {
        throw "Expected installer not found at $outExe"
    }

    # 5. Write SHA-256 checksum sidecar.
    $hash = (Get-FileHash -Path $outExe -Algorithm SHA256).Hash.ToLowerInvariant()
    $shaPath = "$outExe.sha256"
    "$hash  $(Split-Path -Leaf $outExe)" | Set-Content -Path $shaPath -Encoding ascii

    $sizeMB = [math]::Round((Get-Item $outExe).Length / 1MB, 2)
    Write-Host "==> Installer ready: $outExe ($sizeMB MB)" -ForegroundColor Green
    Write-Host "    SHA256: $hash" -ForegroundColor DarkGray
}
finally {
    Pop-Location
}
