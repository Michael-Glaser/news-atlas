@echo off
rem ATLAS - double-click this file on Windows to open the app.
rem
rem It hands the work to ATLAS.ps1, because a batch file cannot report an error in a
rem sentence a person understands. This file's whole job is to start PowerShell with a
rem policy that allows the script to run, and to keep the window open when it stops.
setlocal
cd /d "%~dp0"

where powershell >nul 2>&1
if errorlevel 1 (
  echo.
  echo ATLAS could not start: this Windows does not have PowerShell.
  echo It is part of every supported version of Windows.
  echo.
  pause
  exit /b 1
)

if not exist "%~dp0ATLAS.ps1" (
  echo.
  echo ATLAS could not start: ATLAS.ps1 is missing from this folder.
  echo It sits next to ATLAS.bat in the ATLAS folder. Copy the whole folder again,
  echo keeping every file that came with it, and double-click ATLAS.bat.
  echo.
  pause
  exit /b 1
)

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0ATLAS.ps1" %*
set "CODE=%ERRORLEVEL%"
if not "%CODE%"=="0" (
  echo.
  echo ATLAS could not start. The lines above say what is missing.
  echo.
  pause
)
exit /b %CODE%
