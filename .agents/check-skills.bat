@echo off
rem SPDX-License-Identifier: MPL-2.0

rem Windows entry to .agents\check-skills.ps1, the twin of .agents/check-skills.sh:
rem is the installed set of agent skills behind koruworks/skills? One line when it
rem is, nothing when it is not. Same modes as the shell twin: --now, --update,
rem --record. See the .ps1 for the rest.
powershell -NoProfile -ExecutionPolicy RemoteSigned -File "%~dp0check-skills.ps1" %*
exit /b %ERRORLEVEL%
