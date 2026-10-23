<#
.SYNOPSIS
  Package the native Windows x64 build of ShellAX into an NSIS installer plus a portable zip.

.DESCRIPTION
  The counterpart of tools/package_win.sh (which cross-builds from Linux) for a build made on
  Windows itself: cargo build --profile dist --bin shellax --bin shellax-cli, then makensis and a
  portable zip into -Out (default: .\dist), followed by SHA256SUMS.txt.

  The version is taken from Cargo.toml. The script refuses to run when packaging/windows/installer.nsi
  or packaging/windows/shellax.rc still carry another version, so a package can never ship a stale
  version number.

.PARAMETER Out
  Output directory. Created when missing. Defaults to .\dist.

.PARAMETER SkipBuild
  Reuse target\dist\*.exe instead of running cargo (for re-packaging without a rebuild).

.PARAMETER Makensis
  Full path to makensis.exe. When omitted the script looks in PATH, %SHELLAX_MAKENSIS%, the
  electron-builder cache and %TEMP%\shellax-pack-tools.

.EXAMPLE
  pwsh -File tools\package_win.ps1

.EXAMPLE
  pwsh -File tools\package_win.ps1 -SkipBuild -Out D:\dist-win
#>
[CmdletBinding()]
param(
    [string]$Out = (Join-Path (Split-Path -Parent $PSScriptRoot) 'dist'),
    [switch]$SkipBuild,
    [string]$Makensis
)

$ErrorActionPreference = 'Stop'
$root = (Split-Path -Parent $PSScriptRoot)
Set-Location $root

function Fail($msg) { Write-Host "error: $msg" -ForegroundColor Red; exit 1 }
function Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

# ---- version, and the two files that must agree with it -------------------------------------------------
$cargoToml = Get-Content (Join-Path $root 'Cargo.toml') -Raw
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') { Fail 'cannot read [package] version from Cargo.toml' }
$version = $Matches[1]

$nsiPath = Join-Path $root 'packaging\windows\installer.nsi'
$nsi = Get-Content $nsiPath -Raw
if ($nsi -notmatch '!define VER "([^"]+)"') { Fail "cannot read !define VER from $nsiPath" }
if ($Matches[1] -ne $version) { Fail "installer.nsi says $($Matches[1]) but Cargo.toml says $version; update packaging/windows/installer.nsi (and VIProductVersion)" }

$rcPath = Join-Path $root 'packaging\windows\shellax.rc'
$rc = Get-Content $rcPath -Raw
if ($rc -notmatch 'FILEVERSION ([0-9]+),([0-9]+),([0-9]+),([0-9]+)') { Fail "cannot read FILEVERSION from $rcPath" }
$rcVersion = "$($Matches[1]).$($Matches[2]).$($Matches[3])"
# 预发布版本（如 0.1.16-beta.1）的数字版本号只取前三段
$numericVersion = $version -replace '-.*$', ''
if ($rcVersion -ne $numericVersion) { Fail "shellax.rc says $rcVersion but Cargo.toml says $version; update packaging/windows/shellax.rc" }

Step "ShellAX $version"

# ---- build ---------------------------------------------------------------------------------------------
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) {
    $guess = Join-Path $HOME '.cargo\bin\cargo.exe'
    if (Test-Path $guess) { $cargo = $guess } else { Fail 'cargo not found (install Rust, or add %USERPROFILE%\.cargo\bin to PATH)' }
}
$distDir = Join-Path $root 'target\dist'
$exes = @{ 'shellax.exe' = Join-Path $distDir 'shellax.exe'; 'shellax-cli.exe' = Join-Path $distDir 'shellax-cli.exe' }

if ($SkipBuild) {
    Step 'skipping build (-SkipBuild)'
} else {
    Step "building (cargo build --profile dist) -> $distDir"
    & $cargo build --profile dist --bin shellax --bin shellax-cli
    if ($LASTEXITCODE -ne 0) { Fail "cargo build failed with exit code $LASTEXITCODE" }
}
foreach ($e in $exes.Keys) { if (-not (Test-Path $exes[$e])) { Fail "missing $($exes[$e]); run without -SkipBuild" } }

# The exe carries the icon and version only when build.rs found a resource compiler; check the
# version resource actually made it in, since a silent miss is exactly how a generic-looking exe ships.
$vi = (Get-Item $exes['shellax.exe']).VersionInfo
if ($vi.FileVersion -notlike "$version*") {
    Write-Host "warning: $($exes['shellax.exe']) reports FileVersion '$($vi.FileVersion)', expected $version" -ForegroundColor Yellow
    Write-Host 'warning: check that windres (GNU) or llvm-rc/rc.exe (MSVC) was on PATH during the build' -ForegroundColor Yellow
}

# ---- makensis ------------------------------------------------------------------------------------------
if (-not $Makensis) {
    $cands = @()
    $onPath = Get-Command makensis -ErrorAction SilentlyContinue
    if ($onPath) { $cands += $onPath.Source }
    if ($env:SHELLAX_MAKENSIS) { $cands += $env:SHELLAX_MAKENSIS }
    $cands += (Get-ChildItem "$env:TEMP\shellax-pack-tools\nsis-*\makensis.exe" -ErrorAction SilentlyContinue | ForEach-Object FullName)
    $cands += (Get-ChildItem "$env:LOCALAPPDATA\electron-builder\Cache\nsis-*\nsis-*\makensis.exe" -ErrorAction SilentlyContinue | ForEach-Object FullName)
    $cands += @('C:\Program Files\NSIS\makensis.exe', 'C:\Program Files (x86)\NSIS\makensis.exe')
    $Makensis = $cands | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
}
if (-not $Makensis) { Fail 'makensis.exe not found; install NSIS or pass -Makensis <path> (or set SHELLAX_MAKENSIS)' }

# ---- stage ---------------------------------------------------------------------------------------------
$stage = Join-Path ([System.IO.Path]::GetTempPath()) ("shellax-pack-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $stage -Force | Out-Null
New-Item -ItemType Directory -Path $Out -Force | Out-Null
$Out = (Resolve-Path $Out).Path

try {
    Copy-Item $exes['shellax.exe'] $stage
    Copy-Item $exes['shellax-cli.exe'] $stage
    Copy-Item (Join-Path $root 'THIRD_PARTY_NOTICES.md') $stage
    Copy-Item (Join-Path $root 'docs\WINDOWS_TEST.md') $stage

    # ---- installer -------------------------------------------------------------------------------------
    $setup = Join-Path $Out 'ShellAX-Setup-x64.exe'
    Remove-Item $setup -ErrorAction SilentlyContinue
    Step "makensis -> $setup"
    # NSIS resolves shellax.ico relative to the working directory, so run it from packaging\windows.
    Push-Location (Join-Path $root 'packaging\windows')
    try {
        & $Makensis -V2 "-DSRC=$stage" "-DOUT=$setup" installer.nsi
        $rc = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    if ($rc -ne 0) { Fail "makensis failed with exit code $rc" }

    # ---- portable zip ----------------------------------------------------------------------------------
    $zip = Join-Path $Out 'ShellAX-portable-x64.zip'
    Remove-Item $zip -ErrorAction SilentlyContinue
    $bundle = Join-Path $stage 'ShellAX'
    New-Item -ItemType Directory -Path $bundle | Out-Null
    Move-Item (Join-Path $stage 'shellax.exe') $bundle
    Move-Item (Join-Path $stage 'shellax-cli.exe') $bundle
    Move-Item (Join-Path $stage 'THIRD_PARTY_NOTICES.md') $bundle
    Move-Item (Join-Path $stage 'WINDOWS_TEST.md') $bundle
    Step "zipping -> $zip"
    Compress-Archive -Path $bundle -DestinationPath $zip -CompressionLevel Optimal
} finally {
    Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
}

# WINDOWS_TEST.md also travels outside the packages, as it did before.
Copy-Item (Join-Path $root 'docs\WINDOWS_TEST.md') $Out -Force

# ---- checksums -----------------------------------------------------------------------------------------
Step 'writing SHA256SUMS.txt'
$lines = foreach ($name in 'ShellAX-Setup-x64.exe', 'ShellAX-portable-x64.zip') {
    $h = (Get-FileHash (Join-Path $Out $name) -Algorithm SHA256).Hash.ToLower()
    "$h  $name"
}
Set-Content -Path (Join-Path $Out 'SHA256SUMS.txt') -Value $lines -Encoding ascii

Get-ChildItem $Out | Sort-Object Name | Format-Table Name, Length, LastWriteTime -AutoSize
$lines | ForEach-Object { Write-Host $_ }
Write-Host "OK: ShellAX $version packaged into $Out" -ForegroundColor Green
