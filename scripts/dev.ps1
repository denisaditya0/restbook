#!/usr/bin/env pwsh
# Run cargo with a development environment this machine can build.
#
# The PowerShell twin of dev.sh: the `sqlcipher` feature links libcrypto and its
# build script reads OPENSSL_DIR, and `libsqlite3-sys` panics without it. Rather
# than pinning one machine's path, detect an OpenSSL dev tree here and forward
# every argument to cargo:
#
#   .\scripts\dev.ps1 check
#   cd crates\app; ..\..\scripts\dev.ps1 run
#   .\scripts\dev.ps1 --print-openssl-dir   # what a packaging script should copy
#
# An explicit OPENSSL_DIR is never overwritten. If nothing is found the script
# still runs cargo, which then reports what is missing.

$ErrorActionPreference = 'Stop'

function Test-OpenSsl {
    param([string]$Prefix)
    if ([string]::IsNullOrWhiteSpace($Prefix)) { return $null }
    if (Test-Path -LiteralPath (Join-Path $Prefix 'include/openssl/ssl.h') -PathType Leaf) {
        return $Prefix
    }
    return $null
}

$wasSet = -not [string]::IsNullOrWhiteSpace($env:OPENSSL_DIR)

if (-not $wasSet) {
    $detected = $null

    # A pkg-config .pc file names the prefix, when one is installed.
    $pkgConfig = Get-Command pkg-config -ErrorAction SilentlyContinue
    if ($pkgConfig) {
        try {
            $prefix = & pkg-config --variable=prefix openssl 2>$null | Select-Object -First 1
            $detected = Test-OpenSsl $prefix
        } catch {
            $detected = $null
        }
    }

    # A MinGW toolchain usually keeps a sibling `opt` beside `bin`.
    if (-not $detected) {
        $gcc = Get-Command gcc -ErrorAction SilentlyContinue
        if ($gcc) {
            $gccPath = if ($gcc.Source) { $gcc.Source } else { $gcc.Path }
            $opt = Join-Path (Split-Path (Split-Path $gccPath -Parent) -Parent) 'opt'
            $detected = Test-OpenSsl $opt
        }
    }

    # The usual package-manager prefixes, in case the above found nothing.
    if (-not $detected) {
        $scoop = if ($env:SCOOP) { $env:SCOOP } else { Join-Path $HOME 'scoop' }
        $candidates = @(
            $env:MINGW_PREFIX
            (Join-Path $scoop 'apps/mingw/current/opt')
            (Join-Path $scoop 'apps/openssl/current')
            'C:\msys64\mingw64'
            'C:\msys64\usr'
        )
        foreach ($prefix in $candidates) {
            $detected = Test-OpenSsl $prefix
            if ($detected) { break }
        }
    }

    if ($detected) {
        $env:OPENSSL_DIR = $detected
        [Console]::Error.WriteLine("dev: OPENSSL_DIR=$detected")
    }
}

# libcrypto is linked dynamically, so the loader needs the DLL (or .so) that
# matches it at run time. `cargo run` and `cargo test` do not add the prefix to
# PATH, and another toolchain's copy may win; put this one first.
if ($env:OPENSSL_DIR) {
    $bin = Join-Path $env:OPENSSL_DIR 'bin'
    if (Test-Path -LiteralPath $bin -PathType Container) {
        $env:PATH = $bin + [IO.Path]::PathSeparator + $env:PATH
    }
}

if ($args.Count -ge 1 -and $args[0] -eq '--print-openssl-dir') {
    if ($env:OPENSSL_DIR) { Write-Output $env:OPENSSL_DIR }
    exit 0
}

& cargo @args
exit $LASTEXITCODE
