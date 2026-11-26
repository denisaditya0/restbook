#!/usr/bin/env pwsh
# Get a Windows machine ready to build restbook.
#
# This is the Windows setup section of docs/06-cross-platform.md as one script.
# It checks for, and with `-Install` puts in place, the four things a GNU-target
# build needs:
#
#   rustup      with the toolchain both `rust-toolchain.toml` files pin
#   rustfmt     and clippy, which `cargo fmt --check` and clippy require
#   MinGW       `gcc`, plus the `dlltool`/`as` GPUI's build scripts call
#   user PATH   `%USERPROFILE%\.cargo\bin` and scoop's MinGW `bin`
#
# OpenSSL is not installed here: `scoop install mingw` carries a dev tree in its
# own `opt` directory, and `dev.ps1` finds it there. That script, not this one,
# is what every cargo command goes through afterwards, so this one never
# duplicates its detection - it asks it (`--print-openssl-dir`).
#
#   .\scripts\setup-windows.ps1                 # report; change nothing
#   .\scripts\setup-windows.ps1 -Install        # fix what is missing, asking first
#   .\scripts\setup-windows.ps1 -Install -Yes   # no prompts
#   .\scripts\setup-windows.ps1 -Verify         # then run `dev.ps1 check`
#
# Exit code 0 means the machine is ready, 1 means something is still missing or
# an install failed. Every step is idempotent, so running it again is safe, and
# nothing here needs an elevated shell: scoop and rustup both install under the
# user profile, which is why the build never asks for admin either.

[CmdletBinding()]
param(
    [switch]$Install,
    [switch]$Yes,
    [switch]$Verify
)

$ErrorActionPreference = 'Stop'

$toolchain = 'stable-x86_64-pc-windows-gnu'
$cargoBin = Join-Path $HOME '.cargo\bin'
$scoopRoot = if ([string]::IsNullOrWhiteSpace($env:SCOOP)) { Join-Path $HOME 'scoop' } else { $env:SCOOP }
$scoopShims = Join-Path $scoopRoot 'shims'
$mingwBin = Join-Path $scoopRoot 'apps\mingw\current\bin'
$msys2Bin = 'C:\msys64\mingw64\bin'
$scriptDir = $PSScriptRoot
$repoRoot = Split-Path -Parent $scriptDir
$devPs1 = Join-Path $scriptDir 'dev.ps1'

# ---------------------------------------------------------------------------
# Lookups
# ---------------------------------------------------------------------------

function Test-Command {
    param([string]$Name)
    return [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

function Test-SessionPath {
    param([string]$Directory)
    if ([string]::IsNullOrEmpty($env:PATH)) { return $false }
    $needle = $Directory.TrimEnd('\')
    foreach ($entry in $env:PATH -split ';') {
        if ($entry -and ($entry.TrimEnd('\') -ieq $needle)) { return $true }
    }
    return $false
}

function Add-SessionPath {
    param([string]$Directory)
    if ((Test-Path -LiteralPath $Directory -PathType Container) -and -not (Test-SessionPath $Directory)) {
        $env:PATH = $Directory + ';' + $env:PATH
    }
}

# A prefix is "persisted" when either scope already lists it; only the user
# scope is ever written, so a machine-wide entry is left alone.
function Test-PersistedPath {
    param([string]$Directory)
    $needle = $Directory.TrimEnd('\')
    foreach ($scope in @('Machine', 'User')) {
        $raw = [Environment]::GetEnvironmentVariable('Path', $scope)
        if ([string]::IsNullOrEmpty($raw)) { continue }
        foreach ($entry in $raw -split ';') {
            if ($entry -and ($entry.TrimEnd('\') -ieq $needle)) { return $true }
        }
    }
    return $false
}

# The user Path may be REG_EXPAND_SZ, and Windows ships a `%USERPROFILE%` entry
# in a fresh profile's copy. Reading it unexpanded and writing it back as the
# same kind keeps such an entry literal: `[Environment]::SetEnvironmentVariable`
# always writes REG_SZ, which would freeze today's absolute path (or, worse,
# leave `%VAR%` as text that never expands again).
function Get-UserPathRaw {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
    if (-not $key) { return '' }
    try {
        return [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    } finally {
        $key.Close()
    }
}

function Set-UserPathRaw {
    param([string]$Value)
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    if (-not $key) { throw 'cannot open HKCU\Environment for writing' }
    try {
        # Write the value back as the kind it already had. Forcing REG_EXPAND_SZ
        # on a REG_SZ Path would start expanding a literal `%name%` in it, and
        # forcing REG_SZ on an expandable one would freeze today's paths.
        $kind = [Microsoft.Win32.RegistryValueKind]::String
        try {
            if ($key.GetValueKind('Path') -eq [Microsoft.Win32.RegistryValueKind]::ExpandString) {
                $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
            }
        } catch {
            # No Path value yet; a new one is written as REG_SZ.
        }
        $key.SetValue('Path', $Value, $kind)
    } finally {
        $key.Close()
    }
}

# Explorer hands new processes the cached environment it started with, so a
# PATH change made here reaches a freshly opened terminal only after this
# broadcast. Best-effort: a locked-down shell without Add-Type loses nothing but
# the convenience.
function Send-EnvironmentChange {
    try {
        if (-not ('Restbook.NativeMethods' -as [type])) {
            Add-Type -Namespace Restbook -Name NativeMethods -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint msg, UIntPtr wParam, string lParam, uint flags, uint timeout, out UIntPtr result);
'@
        }
        $result = [UIntPtr]::Zero
        [void][Restbook.NativeMethods]::SendMessageTimeout([IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 0x2, 5000, [ref]$result)
    } catch {
        # Cosmetic only; the value is already written.
    }
}

# `rustup` may be installed but not yet on this session's PATH, which is the
# normal state right after `winget` finishes.
function Resolve-Rustup {
    $command = Get-Command rustup -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    $exe = Join-Path $cargoBin 'rustup.exe'
    if (Test-Path -LiteralPath $exe -PathType Leaf) { return $exe }
    return $null
}

function Resolve-Scoop {
    $command = Get-Command scoop -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    $cmd = Join-Path $scoopShims 'scoop.cmd'
    if (Test-Path -LiteralPath $cmd -PathType Leaf) { return $cmd }
    return $null
}

# All three binaries matter: GPUI's build scripts call `dlltool` and `as`, and
# `-Z gnu` links with MinGW's `ld`, so a tree with only `gcc` is half-installed.
function Test-Mingw {
    return ((Test-Command gcc) -and (Test-Command dlltool) -and (Test-Command as))
}

# The `bin` a MinGW tree would use, when it is installed but not yet on PATH.
function Resolve-MingwDir {
    foreach ($dir in @($mingwBin, $msys2Bin)) {
        if (Test-Path -LiteralPath (Join-Path $dir 'gcc.exe') -PathType Leaf) { return $dir }
    }
    return $null
}

function Test-Toolchain {
    $rustup = Resolve-Rustup
    if (-not $rustup) { return $false }
    $pattern = '^' + [regex]::Escape($toolchain) + '(\s|$)'
    foreach ($line in (& $rustup toolchain list 2>$null)) {
        if ($line -match $pattern) { return $true }
    }
    return $false
}

function Test-Component {
    param([string]$Name)
    $rustup = Resolve-Rustup
    if (-not $rustup) { return $false }
    $pattern = '^' + [regex]::Escape($Name) + '-\S+ \(installed\)'
    foreach ($line in (& $rustup component list --toolchain $toolchain 2>$null)) {
        if ($line -match $pattern) { return $true }
    }
    return $false
}

function Get-OpensslDir {
    if (-not (Test-Path -LiteralPath $devPs1 -PathType Leaf)) { return $null }
    $dir = & $devPs1 --print-openssl-dir 2>$null | Select-Object -First 1
    if ([string]::IsNullOrWhiteSpace($dir)) { return $null }
    return $dir
}

# ---------------------------------------------------------------------------
# Fixes
# ---------------------------------------------------------------------------

function Install-Rustup {
    if (-not (Test-Command winget)) {
        throw 'rustup is missing and winget is not available. Install it from https://rustup.rs, then re-run with -Install.'
    }
    Write-Host 'installing rustup with winget'
    & winget install --id Rustlang.Rustup --exact --accept-source-agreements --accept-package-agreements
    Add-SessionPath $cargoBin
    if (-not (Resolve-Rustup)) {
        throw 'winget finished but rustup is still not found. Install it from https://rustup.rs and re-run.'
    }
}

function Install-Toolchain {
    $rustup = Resolve-Rustup
    if (-not $rustup) { throw 'rustup is missing; install it first' }
    Write-Host "installing $toolchain with rustfmt and clippy"
    & $rustup toolchain install $toolchain --component rustfmt --component clippy --no-self-update
    if ($LASTEXITCODE -ne 0) { throw "rustup exited $LASTEXITCODE" }
}

function Install-Components {
    $rustup = Resolve-Rustup
    if (-not $rustup) { throw 'rustup is missing; install it first' }
    & $rustup component add --toolchain $toolchain rustfmt clippy
    if ($LASTEXITCODE -ne 0) { throw "rustup exited $LASTEXITCODE" }
}

function Install-Scoop {
    # scoop's own installer refuses to run elevated, and refusing here says why
    # before it prints its own error.
    $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'scoop will not install from an elevated shell; run this without "Run as administrator"'
    }
    Write-Host 'installing scoop from https://get.scoop.sh'
    Invoke-Expression (Invoke-RestMethod -Uri 'https://get.scoop.sh')
    Add-SessionPath $scoopShims
}

function Install-Mingw {
    $scoop = Resolve-Scoop
    if (-not $scoop) {
        Install-Scoop
        $scoop = Resolve-Scoop
    }
    if (-not $scoop) { throw 'scoop could not be installed; see https://scoop.sh' }

    # Bucket commands shell out to git; a fresh machine may not have it.
    if (-not (Test-Command git) -and (Test-Command winget)) {
        Write-Host 'installing git with winget (scoop needs it for buckets)'
        & winget install --id Git.Git --exact --accept-source-agreements --accept-package-agreements
        Add-SessionPath (Join-Path $env:ProgramFiles 'Git\cmd')
    }

    Write-Host 'installing mingw with scoop (brings gcc, dlltool, as and an OpenSSL opt tree)'
    & $scoop install mingw
    Add-SessionPath $mingwBin
    if (-not (Test-Path -LiteralPath (Join-Path $mingwBin 'gcc.exe') -PathType Leaf)) {
        throw "scoop finished but gcc.exe is not in $mingwBin; read the scoop output above"
    }
}

function Add-UserPath {
    param([string]$Directory)
    Add-SessionPath $Directory
    if (Test-PersistedPath $Directory) { return }
    $entries = @()
    $raw = Get-UserPathRaw
    if (-not [string]::IsNullOrEmpty($raw)) { $entries = @($raw -split ';' | Where-Object { $_ -ne '' }) }
    Set-UserPathRaw ((@($entries) + $Directory) -join ';')
    $script:pathChanged = $true
    Write-Host "added to user PATH: $Directory"
}

# ---------------------------------------------------------------------------
# Status
# ---------------------------------------------------------------------------

$status = New-Object System.Collections.ArrayList
$script:pathChanged = $false

function Add-Status {
    param([string]$Name, [bool]$Ok, [string]$Detail, [scriptblock]$Fix, [string]$FixLabel)
    [void]$status.Add([pscustomobject]@{
            Name     = $Name
            Ok       = $Ok
            Detail   = $Detail
            Fix      = $Fix
            FixLabel = $FixLabel
        })
}

function Update-Status {
    $status.Clear()
    $mingwDir = Resolve-MingwDir

    Add-Status -Name 'rustup' `
        -Ok ([bool](Resolve-Rustup)) `
        -Detail 'not found; install from https://rustup.rs' `
        -Fix { Install-Rustup } `
        -FixLabel 'winget install Rustlang.Rustup'

    Add-Status -Name "toolchain $toolchain" `
        -Ok (Test-Toolchain) `
        -Detail 'not installed; both rust-toolchain.toml files pin it' `
        -Fix { Install-Toolchain } `
        -FixLabel "rustup toolchain install $toolchain"

    Add-Status -Name 'rustfmt + clippy' `
        -Ok ((Test-Component 'rustfmt') -and (Test-Component 'clippy')) `
        -Detail 'a pinned component is missing; fmt and clippy runs need both' `
        -Fix { Install-Components } `
        -FixLabel "rustup component add --toolchain $toolchain rustfmt clippy"

    Add-Status -Name 'MinGW (gcc, dlltool, as)' `
        -Ok (Test-Mingw) `
        -Detail 'not on PATH; scoop install mingw provides all three' `
        -Fix { Install-Mingw } `
        -FixLabel 'scoop install mingw'

    Add-Status -Name 'user PATH: .cargo\bin' `
        -Ok (Test-PersistedPath $cargoBin) `
        -Detail "missing; no cargo outside this script's session" `
        -Fix { Add-UserPath $cargoBin } `
        -FixLabel "append $cargoBin to the user PATH"

    # Only meaningful once a tree exists; the entry points at scoop's location
    # because that is the one this script installs.
    if ($mingwDir -or (Test-Path -LiteralPath $mingwBin -PathType Container)) {
        $mingwPathDir = if ($mingwDir) { $mingwDir } else { $mingwBin }
        Add-Status -Name "user PATH: $mingwPathDir" `
            -Ok (Test-PersistedPath $mingwPathDir) `
            -Detail 'missing; dev.ps1 still finds it, but `cargo build` from a plain shell will not' `
            -Fix ({ Add-UserPath $mingwPathDir }.GetNewClosure()) `
            -FixLabel "append $mingwPathDir to the user PATH"
    }

    # SQLCipher's build script needs this and panics without it. It comes from
    # MinGW's own tree, so it has no fix of its own: installing MinGW above is
    # what produces it.
    $openssl = Get-OpensslDir
    Add-Status -Name 'OpenSSL dev tree (sqlcipher)' `
        -Ok ([bool]$openssl) `
        -Detail 'not found; it ships with MinGW (scoop apps\mingw\current\opt)' `
        -Fix $null `
        -FixLabel ''
}

function Show-Status {
    foreach ($item in $status) {
        if ($item.Ok) {
            Write-Host ("  [ ok ] {0}" -f $item.Name) -ForegroundColor Green
        } else {
            Write-Host ("  [ .. ] {0} - {1}" -f $item.Name, $item.Detail) -ForegroundColor Yellow
        }
    }
}

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

Write-Host 'restbook: Windows dev setup'
Write-Host ''
Update-Status
Show-Status

$missing = @($status | Where-Object { -not $_.Ok })

if ($missing.Count -gt 0 -and -not $Install) {
    Write-Host ''
    Write-Host 'Re-run with -Install to fix:'
    foreach ($item in $missing) {
        if ($item.FixLabel) {
            Write-Host ("  {0}: {1}" -f $item.Name, $item.FixLabel)
        } else {
            Write-Host ("  {0}: no automatic fix - {1}" -f $item.Name, $item.Detail)
        }
    }
}

if ($Install -and $missing.Count -gt 0) {
    foreach ($item in $missing) {
        if (-not $item.Fix) {
            Write-Host ''
            Write-Host ("{0}: nothing to install automatically ({1})" -f $item.Name, $item.Detail)
            continue
        }
        if (-not $Yes) {
            $answer = Read-Host ("{0} - {1}. Do it? [y/N]" -f $item.Name, $item.FixLabel)
            if ($answer -notmatch '^(y|yes)$') {
                Write-Host 'skipped'
                continue
            }
        }
        Write-Host ''
        try {
            & $item.Fix
        } catch {
            Write-Host ("failed: {0}" -f $_.Exception.Message) -ForegroundColor Red
        }
    }

    Write-Host ''
    Write-Host 'after the installs:'
    Update-Status
    Show-Status
    $missing = @($status | Where-Object { -not $_.Ok })

    if ($script:pathChanged) { Send-EnvironmentChange }
}

if ($missing.Count -eq 0) {
    Write-Host ''
    Write-Host 'Everything restbook needs is present. New terminals pick up PATH changes.'
    Write-Host 'Next: sh scripts/dev.sh build  (or .\scripts\dev.ps1 build here)'
    $ready = $true
} else {
    $ready = $false
}

if ($Verify -and $ready) {
    Write-Host ''
    Write-Host 'running: dev.ps1 check'
    Push-Location $repoRoot
    try {
        & $devPs1 check --message-format short
    } finally {
        Pop-Location
    }
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

if ($ready) { exit 0 }
exit 1
