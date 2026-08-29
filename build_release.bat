@echo off
rem Build release: cargo build + optional UPX pack + copy to dist\
rem Usage: build_release.bat [path\to\upx.exe]
rem   Without args, auto-detects tools\upx*-win64\upx.exe; skips packing if absent.

setlocal
cd /d "%~dp0"

echo [1/3] cargo build --release ...
cargo build --release
if errorlevel 1 (
    echo Build failed.
    exit /b 1
)

set EXE=target\release\mini-rtt-viewer.exe
rem NOTE: do NOT name this variable UPX -- the upx.exe tool reads an env var
rem named UPX as extra command-line options and will abort.
set "UPXBIN=%~1"
if "%UPXBIN%"=="" (
    for /d %%D in (tools\upx*-win64) do if exist "%%D\upx.exe" set "UPXBIN=%%D\upx.exe"
)

if exist "%UPXBIN%" (
    echo [2/3] Packing with %UPXBIN% ...
    "%UPXBIN%" --best "%EXE%"
    if errorlevel 1 echo UPX packing failed, keeping uncompressed binary.
) else (
    echo [2/3] UPX not found, skipping packing.
)

echo [3/3] Copying to dist\ ...
if not exist dist mkdir dist
copy /y "%EXE%" dist\ >nul
echo Artifact: dist\mini-rtt-viewer.exe
dir dist | findstr mini-rtt-viewer

echo Done.
endlocal
