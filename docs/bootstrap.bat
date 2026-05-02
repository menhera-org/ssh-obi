@echo off
setlocal EnableExtensions

set "want=0.1"
set "target=x86_64-pc-windows-msvc"
set "base_url=https://obi.menhera.org"

if "%~1"=="--" shift
if /I "%~1"=="--install" shift

if not "%~1"=="" (
    set "want=%~1"
    shift
)

if /I "%~1"=="--install" shift

if not "%~1"=="" (
    set "obi_error=unsupported argument %~1"
    goto fail
)

if not defined USERPROFILE (
    set "obi_error=USERPROFILE is not set"
    goto fail
)

if /I "%PROCESSOR_ARCHITECTURE%"=="AMD64" goto arch_ok
if /I "%PROCESSOR_ARCHITEW6432%"=="AMD64" goto arch_ok
set "obi_error=unsupported Windows architecture %PROCESSOR_ARCHITECTURE%; only x86_64 is published"
goto fail

:arch_ok
set "install_root=%USERPROFILE%\.ssh-obi"
set "bin_dir=%install_root%\bin"
set "client=%bin_dir%\ssh-obi.exe"
set "archive_url=%base_url%/release-%target%.tar.gz"

if not defined TEMP (
    set "obi_error=TEMP is not set"
    goto fail
)

set "tmp=%TEMP%\ssh-obi-install-%RANDOM%-%RANDOM%"
set "archive=%tmp%\release.tar.gz"

if exist "%tmp%" rmdir /s /q "%tmp%" >nul 2>nul
mkdir "%tmp%" >nul 2>nul
if errorlevel 1 (
    set "obi_error=failed to create temporary directory"
    goto fail
)

where tar.exe >nul 2>nul
if errorlevel 1 (
    set "obi_error=tar.exe is required to unpack release archives"
    goto fail
)

set "SSH_OBI_ARCHIVE_URL=%archive_url%"
set "SSH_OBI_ARCHIVE=%archive%"

where curl.exe >nul 2>nul
if errorlevel 1 goto download_with_powershell

curl.exe -fsSL "%archive_url%" -o "%archive%"
if errorlevel 1 (
    set "obi_error=failed to download release archive"
    goto fail
)
goto downloaded

:download_with_powershell
where powershell.exe >nul 2>nul
if errorlevel 1 (
    set "obi_error=no supported downloader found; install curl.exe or powershell.exe"
    goto fail
)

powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "$ErrorActionPreference='Stop'; Invoke-WebRequest -UseBasicParsing -Uri $env:SSH_OBI_ARCHIVE_URL -OutFile $env:SSH_OBI_ARCHIVE"
if errorlevel 1 (
    set "obi_error=failed to download release archive"
    goto fail
)

:downloaded
tar.exe -xzf "%archive%" -C "%tmp%"
if errorlevel 1 (
    set "obi_error=failed to unpack release archive"
    goto fail
)

if not exist "%tmp%\ssh-obi.exe" (
    set "obi_error=release archive does not contain ssh-obi.exe"
    goto fail
)

mkdir "%bin_dir%" >nul 2>nul
if errorlevel 1 (
    set "obi_error=failed to create install directory"
    goto fail
)

copy /Y "%tmp%\ssh-obi.exe" "%client%" >nul
if errorlevel 1 (
    set "obi_error=failed to install ssh-obi.exe"
    goto fail
)

set "SSH_OBI_BIN_DIR=%bin_dir%"
where powershell.exe >nul 2>nul
if errorlevel 1 (
    set "obi_error=powershell.exe is required to update the user PATH"
    goto fail
)

powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "$ErrorActionPreference='Stop'; $bin=$env:SSH_OBI_BIN_DIR; $path=[Environment]::GetEnvironmentVariable('Path','User'); if ([string]::IsNullOrEmpty($path)) { [Environment]::SetEnvironmentVariable('Path',$bin,'User') } elseif (($path -split ';') -notcontains $bin) { [Environment]::SetEnvironmentVariable('Path',($path.TrimEnd(';') + ';' + $bin),'User') }"
if errorlevel 1 (
    set "obi_error=failed to update the user PATH"
    goto fail
)

if exist "%tmp%" rmdir /s /q "%tmp%" >nul 2>nul
echo OBI-INSTALL-COMPLETE
echo OBI-PATH %bin_dir%
echo OBI-NOTE restart your terminal if ssh-obi.exe is not found on PATH
exit /b 0

:fail
if defined tmp if exist "%tmp%" rmdir /s /q "%tmp%" >nul 2>nul
if defined obi_error (
    echo OBI-ERROR %obi_error%
) else (
    echo OBI-ERROR bootstrap failed
)
exit /b 1
