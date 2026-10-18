@echo off
REM sky-tracer unified launcher wrapper.
REM Prefer PowerShell 7 (pwsh), fall back to Windows PowerShell 5.1.
REM Usage: run.cmd [workbench|weather|fixed|smoke|sky|realtime|snapshot|audit|effects|mc|mc-bake|mc-preview|check|help]
REM No argument starts the interactive Chinese menu. Exit code is propagated.
setlocal
where pwsh >nul 2>nul
if %errorlevel%==0 (
  pwsh -NoProfile -ExecutionPolicy Bypass -File "%~dp0run.ps1" %*
) else (
  powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0run.ps1" %*
)
exit /b %errorlevel%
