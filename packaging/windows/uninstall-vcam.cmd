@echo off
chcp 65001 >nul
rem Unregisters the "Lensbridge Camera" DirectShow filter. Run as administrator.
setlocal
set DIR=%~dp0vcam
"%WINDIR%\System32\regsvr32.exe" /s /u "%DIR%\lensbridge_vcam.dll"
if exist "%WINDIR%\SysWOW64\regsvr32.exe" if exist "%DIR%\lensbridge_vcam_x86.dll" "%WINDIR%\SysWOW64\regsvr32.exe" /s /u "%DIR%\lensbridge_vcam_x86.dll"
echo Готово.
pause
