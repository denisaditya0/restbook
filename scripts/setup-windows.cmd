@echo off
rem The cmd.exe entry point for setup-windows.ps1: a machine with no PowerShell
rem profile still has powershell.exe, and `-ExecutionPolicy Bypass` keeps a
rem restrictive policy from blocking the script this wrapper was shipped with.
rem Flags are forwarded unchanged; see the .ps1 for what they mean.
rem
rem   scripts\setup-windows.cmd
rem   scripts\setup-windows.cmd -Install
rem   scripts\setup-windows.cmd -Install -Yes -Verify

setlocal EnableExtensions EnableDelayedExpansion

set "ARGS="

:collect
if "%~1"=="" goto run
set "ARGS=!ARGS! %1"
shift
goto collect

:run
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup-windows.ps1"!ARGS!
exit /b %ERRORLEVEL%
