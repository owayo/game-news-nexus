@echo off
setlocal

pushd "%~dp0" || exit /b 1
where mise.exe >nul 2>&1
if errorlevel 1 (
    echo mise.exe was not found. Install mise and add it to PATH. 1>&2
    popd
    exit /b 1
)

mise.exe install
if not "%errorlevel%"=="0" goto :done

mise.exe run release -- --locked
if not "%errorlevel%"=="0" goto :done

mise.exe run installer -- -ForceInstall

:done
set "install_exit=%errorlevel%"
popd
exit /b %install_exit%
