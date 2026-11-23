@echo off
rem Run cargo with a development environment this machine can build.
rem
rem The cmd.exe twin of dev.sh: the `sqlcipher` feature links libcrypto and its
rem build script reads OPENSSL_DIR, and `libsqlite3-sys` panics without it.
rem Detect an OpenSSL dev tree here, then forward every argument to cargo:
rem
rem   scripts\dev.cmd check
rem   cd crates\app && ..\..\scripts\dev.cmd run
rem   scripts\dev.cmd --print-openssl-dir
rem   scripts\dev.cmd --logs run        (also tee both streams to target\dev-logs)
rem
rem An explicit OPENSSL_DIR is never overwritten. If nothing is found the script
rem still runs cargo, which then reports what is missing.
rem
rem A GUI run detaches from the console, so its stdout and stderr are easy to
rem miss. `--logs` keeps a copy of everything cargo and the app print under
rem `target\dev-logs\<stamp>-<logfile>`, and points RUST_LOG at the log sink the
rem app already installs (see `crates/app/src/logging.rs`).

setlocal EnableExtensions EnableDelayedExpansion

rem `--logs` tees every stream to a file in the workspace target directory; it
rem is consumed here so cargo never sees it. `%*` is a single token, so it is
rem rebuilt by shifting the positionals and accumulating the rest; this survives
rem argument quoting better than trusting a bare `shift`.
set "LOGS="
set "ARGS="

rem Capture the script's own directory before any `shift`: shifting moves `%0`
rem too, so `%~dp0` stops naming this script after the first argument is dropped.
set "SCRIPT_DIR=%~dp0"

if /i "%~1"=="--logs" (
    set "LOGS=1"
    shift
)

rem The first command argument, captured before `:collect` shifts the positionals
rem away. `--print-openssl-dir` is checked below, after detection has run.
set "FIRST=%~1"

:collect
if "%~1"=="" goto collected
set "ARGS=!ARGS! %1"
shift
goto collect

:collected
if not defined OPENSSL_DIR (
    set "detected="

    rem MINGW_PREFIX, then the `opt` beside the gcc on PATH. The compiler's
    rem own tree comes first: a `pkg-config` earlier on PATH can belong to
    rem another toolchain (Strawberry Perl ships one) and name an OpenSSL this
    rem compiler does not link.
    if not defined detected if defined MINGW_PREFIX if exist "%MINGW_PREFIX%\include\openssl\ssl.h" set "detected=%MINGW_PREFIX%"
    if not defined detected (
        for /f "delims=" %%G in ('where gcc 2^>nul') do (
            if exist "%%~dpG..\opt\include\openssl\ssl.h" (
                for %%O in ("%%~dpG..\opt") do set "detected=%%~fO"
            )
        )
    )
    if not defined detected (
        if not defined SCOOP set "SCOOP=%USERPROFILE%\scoop"
        if exist "!SCOOP!\apps\mingw\current\opt\include\openssl\ssl.h" set "detected=!SCOOP!\apps\mingw\current\opt"
    )

    rem A pkg-config .pc file names the prefix, when one is installed. Only
    rem after the compiler's own tree, for the reason above.
    if not defined detected (
        where pkg-config >nul 2>nul
        if not errorlevel 1 (
            for /f "delims=" %%P in ('pkg-config --variable=prefix openssl 2^>nul') do (
                if exist "%%P\include\openssl\ssl.h" for %%O in ("%%P") do set "detected=%%~fO"
            )
        )
    )

    rem Scoop's other usual prefix.
    if not defined detected (
        if not defined SCOOP set "SCOOP=%USERPROFILE%\scoop"
        if exist "!SCOOP!\apps\openssl\current\include\openssl\ssl.h" set "detected=!SCOOP!\apps\openssl\current"
    )

    rem The prefixes a plain MSYS2 install uses.
    if not defined detected if exist "C:\msys64\mingw64\include\openssl\ssl.h" set "detected=C:\msys64\mingw64"
    if not defined detected if exist "C:\msys64\usr\include\openssl\ssl.h" set "detected=C:\msys64\usr"

    if defined detected (
        set "OPENSSL_DIR=!detected!"
        echo dev: OPENSSL_DIR=!detected! 1>&2
    )
)

rem libcrypto is linked dynamically, so the loader needs the DLL that matches it
rem at run time; put the detected prefix first.
if defined OPENSSL_DIR if exist "%OPENSSL_DIR%\bin" set "PATH=%OPENSSL_DIR%\bin;%PATH%"

if /i "%FIRST%"=="--print-openssl-dir" (
    if defined OPENSSL_DIR echo %OPENSSL_DIR%
    exit /b 0
)

if defined LOGS (
    rem Where the log lands, beside the build output so `cargo clean` clears it
    rem too. `SCRIPT_DIR` is `scripts\`; PowerShell canonicalizes the `..` so the
    rem printed path is the real one and the directory is created in one step.
    for /f "delims=" %%D in ('powershell -NoProfile -Command "$d=Join-Path '!SCRIPT_DIR!..' 'target\dev-logs'; $d=[System.IO.Path]::GetFullPath($d); New-Item -ItemType Directory -Force -Path $d ^| Out-Null; Write-Output $d"') do set "LOG_DIR=%%D"

    rem One file per run, stamped so runs do not overwrite each other.
    for /f "delims=" %%T in ('powershell -NoProfile -Command "Get-Date -Format yyyyMMdd-HHmmss"') do set "STAMP=%%T"
    if not defined STAMP set "STAMP=run"
    set "LOG_FILE=!LOG_DIR!\!STAMP!-cargo.log"

    rem `--logs` is the developer path, so it runs the full debug transcript
    rem (the app's own capture window is for release builds). A caller's own
    rem `RUST_LOG` always wins.
    if not defined RUST_LOG set "RUST_LOG=warn,restbook_app=debug,restbook_core=debug,restbook_store=debug,restbook_transport=debug"

    echo dev: logging to !LOG_FILE! 1>&2
    echo dev: RUST_LOG=!RUST_LOG! 1>&2

    rem Tee by writing each stream to the file and to its console, line by line.
    rem PowerShell owns the whole run so the exit code survives: cmd's pipeline
    rem would report the last stage, not cargo. It runs cargo, tees every line,
    rem and exits with cargo's own code. A stderr line arrives as an ErrorRecord
    rem (Windows PowerShell 5.1), so its text is read from the exception; the
    rem blank line between cargo's error paragraphs is one with no message.
    powershell -NoProfile -Command "$ErrorActionPreference='Continue'; $out=New-Object System.IO.StreamWriter('!LOG_FILE!'); $out.AutoFlush=$true; try { $lines = & cargo !ARGS! 2>&1; foreach ($line in $lines) { $text = if ($line -is [System.Management.Automation.ErrorRecord]) { $line.Exception.Message } else { [string]$line }; [Console]::Out.WriteLine($text); $out.WriteLine($text) }; exit $LASTEXITCODE } finally { $out.Dispose() }"
    exit /b !ERRORLEVEL!
)

cargo!ARGS!
exit /b %ERRORLEVEL%
