@echo off
setlocal EnableExtensions DisableDelayedExpansion
pushd "%~dp0" >nul
if errorlevel 1 exit /b 1
set "BNAUTH_EXIT_CODE=1"

call :find_rust
if errorlevel 1 goto :finish
call :find_visual_studio
if errorlevel 1 goto :finish

if not defined CARGO_HOME set "CARGO_HOME=%~dp0.build\cargo-home"
if not defined CARGO_TARGET_DIR set "CARGO_TARGET_DIR=%~dp0.build\target"
if not "%~1"=="" goto :cargo_command

cargo.exe build --release --locked
set "BNAUTH_EXIT_CODE=%errorlevel%"
if not "%BNAUTH_EXIT_CODE%"=="0" goto :finish
if not exist "dist\" mkdir "dist"
if errorlevel 1 goto :failed
copy /y "%CARGO_TARGET_DIR%\release\BNAuth.exe" "dist\BNAuth.exe" >nul
if errorlevel 1 goto :failed
echo Release executable: "%~dp0dist\BNAuth.exe"
goto :finish

:cargo_command
cargo.exe %*
set "BNAUTH_EXIT_CODE=%errorlevel%"
goto :finish

:failed
set "BNAUTH_EXIT_CODE=1"

:finish
popd
exit /b %BNAUTH_EXIT_CODE%

:find_rust
if defined BNAUTH_TOOLCHAIN goto :rust_override
where cargo.exe >nul 2>&1
if not errorlevel 1 goto :rust_ready
if defined CARGO_HOME if exist "%CARGO_HOME%\bin\cargo.exe" (
  set "PATH=%CARGO_HOME%\bin;%PATH%"
  goto :rust_ready
)
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" (
  set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
  goto :rust_ready
)
goto :missing_rust

:rust_override
if not exist "%BNAUTH_TOOLCHAIN%\cargo.exe" goto :missing_rust
if not exist "%BNAUTH_TOOLCHAIN%\rustc.exe" goto :missing_rust
set "PATH=%BNAUTH_TOOLCHAIN%;%PATH%"

:rust_ready
where rustc.exe >nul 2>&1
if errorlevel 1 goto :missing_rust
exit /b 0

:missing_rust
>&2 echo Rust was not found. Install the Windows MSVC Rust toolchain and reopen the terminal.
>&2 echo For a custom installation, set BNAUTH_TOOLCHAIN to its bin directory.
exit /b 1

:find_visual_studio
set "BNAUTH_VCVARS="
if defined VSINSTALLDIR if exist "%VSINSTALLDIR%\VC\Auxiliary\Build\vcvarsall.bat" (
  set "BNAUTH_VCVARS=%VSINSTALLDIR%\VC\Auxiliary\Build\vcvarsall.bat"
  goto :visual_studio_ready
)
set "BNAUTH_VSWHERE="
for /f "delims=" %%I in ('where vswhere.exe 2^>nul') do if not defined BNAUTH_VSWHERE set "BNAUTH_VSWHERE=%%I"
if not defined BNAUTH_VSWHERE if exist "%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe" set "BNAUTH_VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not defined BNAUTH_VSWHERE goto :missing_visual_studio
set "BNAUTH_VSROOT="
for /f "usebackq delims=" %%I in (`"%BNAUTH_VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "BNAUTH_VSROOT=%%I"
if not defined BNAUTH_VSROOT goto :missing_visual_studio
set "BNAUTH_VCVARS=%BNAUTH_VSROOT%\VC\Auxiliary\Build\vcvarsall.bat"
if not exist "%BNAUTH_VCVARS%" goto :missing_visual_studio

:visual_studio_ready
call "%BNAUTH_VCVARS%" x64 >nul
if errorlevel 1 (
  >&2 echo Could not initialize the Visual Studio x64 build environment.
  exit /b 1
)
exit /b 0

:missing_visual_studio
>&2 echo Visual Studio C++ Build Tools were not found.
>&2 echo Install Desktop development with C++ and a Windows SDK, then retry.
exit /b 1
