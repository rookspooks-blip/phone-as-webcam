@echo off
chcp 65001 >nul
rem Registers the "Lensbridge Camera" DirectShow filter (64- and 32-bit).
rem Run as administrator (right click -> Run as administrator).
setlocal
set DIR=%~dp0vcam
net session >nul 2>&1
if errorlevel 1 (
  echo Нужны права администратора: правый клик -^> "Запуск от имени администратора".
  pause
  exit /b 1
)
"%WINDIR%\System32\regsvr32.exe" /s "%DIR%\lensbridge_vcam.dll" || goto :err
if exist "%WINDIR%\SysWOW64\regsvr32.exe" if exist "%DIR%\lensbridge_vcam_x86.dll" "%WINDIR%\SysWOW64\regsvr32.exe" /s "%DIR%\lensbridge_vcam_x86.dll" || goto :err
echo Камера "Lensbridge Camera" установлена. Перезапустите Zoom/OBS/браузер.
pause
exit /b 0
:err
echo Ошибка регистрации (код %errorlevel%).
pause
exit /b 1
