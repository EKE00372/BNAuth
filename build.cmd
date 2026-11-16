@echo off
setlocal
if not defined BNAUTH_TOOLCHAIN set "BNAUTH_TOOLCHAIN=D:\Github\RustAuth\.self\2026-11-11-C-workspace\workspace\tmp\rust\bin"
if not exist "%BNAUTH_TOOLCHAIN%\cargo.exe" (
  echo Rust toolchain not found. Set BNAUTH_TOOLCHAIN to a folder containing cargo.exe and rustc.exe.
  exit /b 1
)
set "PATH=%BNAUTH_TOOLCHAIN%;%PATH%"
set "CARGO_HOME=%~dp0.build\cargo-home"
set "CARGO_TARGET_DIR=%~dp0.build\target"
set "CARGO_BUILD_JOBS=2"
set "BNAUTH_VCVARS=%ProgramFiles(x86)%\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvarsall.bat"
if not exist "%BNAUTH_VCVARS%" (
  echo Visual Studio C++ Build Tools 18 not found.
  exit /b 1
)
call "%BNAUTH_VCVARS%" x64 >nul
if errorlevel 1 exit /b 1
cd /d "%~dp0"
if "%~1"=="" (
  cargo build --release --offline
  if errorlevel 1 exit /b 1
  if not exist "%~dp0dist\" mkdir "%~dp0dist"
  if errorlevel 1 exit /b 1
  copy /y "%CARGO_TARGET_DIR%\release\BNAuth.exe" "%~dp0dist\BNAuth.exe" >nul
  if errorlevel 1 exit /b 1
  echo Release executable: "%~dp0dist\BNAuth.exe"
) else (
  cargo %*
)
exit /b %errorlevel%
