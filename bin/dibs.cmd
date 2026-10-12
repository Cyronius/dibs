@echo off
setlocal
set "RUN=%~dp0..\src\run_dibs.py"
set "PY="
if defined DIBS_PYTHON set PY="%DIBS_PYTHON%"
if not defined PY where /q py && set "PY=py -3"
if not defined PY where /q python && set "PY=python"
if not defined PY goto nopython
%PY% "%RUN%" %*
exit /b %errorlevel%

:nopython
if /i "%~1"=="hook" exit /b 0
echo dibs: needs Python 3.11 or newer on PATH. Get it at https://www.python.org/downloads/ or set DIBS_PYTHON. 1>&2
exit /b 127
