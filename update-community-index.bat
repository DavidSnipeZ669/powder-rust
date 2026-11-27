@echo off
REM Rewrites games\index.json after adding or changing community game packs.
setlocal
set EXE=
for %%P in ("%~dp0dist-app\Rewind.exe" "%~dp0..\dist-app\Rewind.exe" "%LOCALAPPDATA%\Rewind\Rewind.exe") do if not defined EXE if exist %%P set EXE=%%~P
if not defined EXE (
  echo Rewind.exe not found. Run build-app.bat first.
  pause
  exit /b 1
)
start "" /wait "%EXE%" --community-index "%~dp0games"
if exist "%~dp0games\index.json" ( type "%~dp0games\index.json" ) else ( echo Failed - see %%LOCALAPPDATA%%\Powder\powder-rs.log )
echo.
echo Now commit and push: git add games ^&^& git commit -m "Update community games" ^&^& git push
pause
