@echo off
cd /d "%~dp0"
if not exist "bin\ai-tutor.exe" (
  echo Please run Install AI Tutor.cmd first.
  pause
  exit /b 1
)
start "" "%~dp0bin\ai-tutor.exe" --root "%~dp0."
