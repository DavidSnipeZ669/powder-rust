@echo off
REM Builds Rewind as ONE file: dist-app\Rewind.exe. Share just that file.
REM On first run it sets itself up: gets Powder's recorder/AI engine (from the user's Powder install,
REM or via Steam where Powder is free), downloads ONNX Runtime + DirectML (Microsoft) and the text
REM models, then installs itself with Start menu / desktop shortcuts and an uninstall entry.
setlocal
REM Powder folder used to make Rewind-Full.exe (the offline version). Change it, or pass it:
REM   build-app.bat "D:\Powder"
set POWDER_SRC=B:\Powder_v8.0.2_standalone
if not "%~1"=="" set POWDER_SRC=%~1
where cargo >nul 2>nul
if errorlevel 1 (
  echo Rust is not installed. Install it from https://rustup.rs  ^(default MSVC option, say yes to the
  echo "Visual Studio C++ Build Tools"^), then run this file again.
  start https://rustup.rs
  pause
  exit /b 1
)
set OUT=%~dp0dist-app
if not exist "%OUT%" mkdir "%OUT%"

echo === 1/2 Building the custom-game engine (packed inside Rewind.exe) ===
cd /d "%~dp0powder-rs"
cargo build --release
if errorlevel 1 ( echo Engine build failed. & pause & exit /b 1 )

echo === 2/2 Building Rewind.exe ===
REM a running Rewind locks its exe: close it first (it lives in the tray when its window is closed)
taskkill /im Rewind.exe /f >nul 2>nul
taskkill /im Rewind-Full.exe /f >nul 2>nul
cd /d "%~dp0powder-app"
set REWIND_ENGINE_EXE=%~dp0powder-rs\target\release\powder-rs.exe
cargo build --release
if errorlevel 1 ( echo Build failed. & pause & exit /b 1 )
copy /y target\release\Rewind.exe "%OUT%\Rewind.exe" >nul
if errorlevel 1 ( echo Could not replace %OUT%\Rewind.exe - close Rewind and run this again. & pause & exit /b 1 )

echo === Making Rewind-Full.exe (Rewind with Powder's files inside, no download needed) ===
if exist "%POWDER_SRC%\resources" (
  echo Packing about 1 GB of Powder files - this takes a minute or two...
  if exist "%OUT%\Rewind-Full.exe" del "%OUT%\Rewind-Full.exe"
  start "" /wait "%~dp0powder-app\target\release\Rewind.exe" --make-full "%POWDER_SRC%" "%OUT%\Rewind-Full.exe"
  if exist "%OUT%\Rewind-Full.exe" ( echo Made Rewind-Full.exe ) else ( echo Could not make Rewind-Full.exe - see %%LOCALAPPDATA%%\Powder\powder-rs.log )
) else (
  echo Skipped: no Powder folder at %POWDER_SRC%
)

echo.
echo Done:
echo   %OUT%\Rewind.exe       small; downloads Powder's files during setup
echo   %OUT%\Rewind-Full.exe  everything inside; installs with no internet
echo Either file is the whole app. Upload both to the GitHub release.
echo (Testing setup again on this PC: delete %%LOCALAPPDATA%%\Rewind, or run Rewind.exe --setup)
pause
