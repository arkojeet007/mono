@echo off
rem SPDX-License-Identifier: MPL-2.0

rem Koru Android task runner for Windows.
setlocal EnableExtensions EnableDelayedExpansion
cd /d "%~dp0"
for %%I in ("%CD%\..\..") do set "REPO_ROOT=%%~fI"

set "ARG1=%~2"
set "ARG2=%~3"
set "SUB=%~1"
set "DEVICE_OPTION_COUNT=0"
if "%SUB%"=="" goto do_help
if /i "%SUB%"=="run-local" (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\run-local.ps1" -AndroidDirectory "%~dp0." %*
    exit /b !ERRORLEVEL!
)
if /i "%SUB%"=="run-prod" (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\run-local.ps1" -AndroidDirectory "%~dp0." %*
    exit /b !ERRORLEVEL!
)

rem Collect everything after the subcommand for Gradle passthrough commands.
if not "%~1"=="" shift
set "ARGS="
:collect_args
if "%~1"=="" goto load_config
if /i "%~1"=="--contracts" (
    set "GENERATE_CONTRACTS=1"
    shift
    goto collect_args
)
rem cmd.exe splits "--device=tablet" on the "=", so the value is the next token.
if /i "%~1"=="--device" goto take_device
if /i "%~1"=="-d" goto take_device
set "MAYBE_DEVICE=%~1"
if /i "!MAYBE_DEVICE:~0,9!"=="--device=" (
    set "DEVICE_WANTED=!MAYBE_DEVICE:~9!"
    set /a DEVICE_OPTION_COUNT+=1
    shift
    goto collect_args
)
if not defined POS1 (
    set "POS1=%~1"
) else (
    if not defined POS2 set "POS2=%~1"
)
set "ARGS=!ARGS! "%~1""
shift
goto collect_args

:take_device
shift
if "%~1"=="" (
    echo koru: --device needs an alias, a serial, or part of a device name.
    exit /b 2
)
set "DEVICE_WANTED=%~1"
set /a DEVICE_OPTION_COUNT+=1
shift
goto collect_args

:load_config
if exist "koru.local.conf" (
    for /f "usebackq tokens=1,* delims==" %%A in ("koru.local.conf") do (
        if /i "%%A"=="java_home" if not defined KORU_JAVA_HOME set "KORU_JAVA_HOME=%%B"
        if /i "%%A"=="android_home" if not defined KORU_ANDROID_HOME set "KORU_ANDROID_HOME=%%B"
        if /i "%%A"=="device" if not defined KORU_DEVICE set "KORU_DEVICE=%%B"
        for /f "tokens=1,* delims=." %%X in ("%%A") do (
            if /i "%%X"=="device" if not "%%Y"=="" (
                set "ALIAS_%%Y=%%B"
                set "ALIASES=!ALIASES! %%Y"
            )
        )
    )
)
if /i not "%SUB%"=="clean-install" if defined DEVICE_WANTED call :apply_alias "!DEVICE_WANTED!"

rem Positional arguments, with any --device flag already taken out.
set "ARG1=!POS1!"
set "ARG2=!POS2!"

if /i "%SUB%"=="help" goto do_help
if /i "%SUB%"=="-h" goto do_help
if /i "%SUB%"=="--help" goto do_help
if /i "%SUB%"=="setup" goto do_setup
if /i "%SUB%"=="contracts" goto do_contracts
if /i "%SUB%"=="clean-install" goto do_clean_install

call :ensure_environment || exit /b 1

if /i "%SUB%"=="run" goto do_run
if /i "%SUB%"=="build" goto do_build
if /i "%SUB%"=="test" goto do_test
if /i "%SUB%"=="check" goto do_check
if /i "%SUB%"=="install" goto do_install
if /i "%SUB%"=="launch" goto do_launch
if /i "%SUB%"=="clean" goto do_clean
if /i "%SUB%"=="doctor" goto do_doctor
if /i "%SUB%"=="env" goto do_doctor
if /i "%SUB%"=="avds" goto do_avds
if /i "%SUB%"=="emulator" goto do_emulator
if /i "%SUB%"=="devices" goto do_devices
if /i "%SUB%"=="alias" goto do_alias
if /i "%SUB%"=="stop" goto do_stop
if /i "%SUB%"=="logcat" goto do_logcat
if /i "%SUB%"=="gradle" goto do_gradle

echo koru: unknown command "%SUB%"
echo.
goto help_error

:do_setup
if not "%ARG1%"=="" (
    set "JAVA_READY="
    call :try_jdk "%ARG1%"
    if not defined JAVA_READY (
        set "SETUP_BAD_JDK=%ARG1%"
        goto setup_bad_jdk
    )
    set "KORU_JAVA_HOME=%ARG1%"
)
if not "%ARG2%"=="" (
    set "ANDROID_READY="
    call :try_android "%ARG2%"
    if not defined ANDROID_READY (
        set "SETUP_BAD_SDK=%ARG2%"
        goto setup_bad_sdk
    )
    set "KORU_ANDROID_HOME=%ARG2%"
)
call :ensure_environment || exit /b 1
>"koru.local.conf" echo java_home=%JAVA_HOME%
>>"koru.local.conf" echo android_home=%ANDROID_HOME%
if defined KORU_DEVICE >>"koru.local.conf" echo device=%KORU_DEVICE%
for %%N in (!ALIASES!) do call :write_alias "%%N"
echo [koru] wrote %CD%\koru.local.conf
echo [koru] this machine-local file is ignored by git.
call :show_environment
exit /b 0

:setup_bad_jdk
echo koru: "%SETUP_BAD_JDK%" is not a JDK 17 or newer.
exit /b 1

:setup_bad_sdk
echo koru: "%SETUP_BAD_SDK%" is not an Android SDK with Platform-Tools.
exit /b 1

:do_build
if defined GENERATE_CONTRACTS call :generate_contracts
if errorlevel 1 exit /b 1
call gradlew.bat :app:assembleDebug%ARGS%
if errorlevel 1 exit /b %ERRORLEVEL%
echo [koru] APK: %CD%\app\build\outputs\apk\debug\app-debug.apk
exit /b 0

:do_test
if defined GENERATE_CONTRACTS call :generate_contracts
if errorlevel 1 exit /b 1
call gradlew.bat :app:testDebugUnitTest%ARGS%
exit /b %ERRORLEVEL%

:do_check
if defined GENERATE_CONTRACTS call :generate_contracts
if errorlevel 1 exit /b 1
call gradlew.bat :app:compileDebugKotlin%ARGS%
exit /b %ERRORLEVEL%

:do_clean
call gradlew.bat clean%ARGS%
exit /b %ERRORLEVEL%

:do_gradle
if "%ARGS%"=="" (
    echo koru: gradle needs a task or argument.
    echo example: koru.bat gradle :app:dependencies
    exit /b 2
)
call gradlew.bat%ARGS%
exit /b %ERRORLEVEL%

:do_install
call :resolve_device || exit /b 1
if defined GENERATE_CONTRACTS call :generate_contracts
if errorlevel 1 exit /b 1
call gradlew.bat :app:assembleDebug%ARGS%
if errorlevel 1 exit /b %ERRORLEVEL%
"%ADB%" -s "%DEVICE%" install -r "app\build\outputs\apk\debug\app-debug.apk"
exit /b %ERRORLEVEL%

:do_clean_install
rem Destructive selection never uses aliases, remembered defaults or a picker.
if not "!DEVICE_OPTION_COUNT!"=="1" (
    echo koru: clean-install requires exactly one --device with an exact serial or model.
    exit /b 2
)
if not defined DEVICE_WANTED (
    echo koru: clean-install requires an explicit connected device.
    exit /b 2
)
if defined ARGS (
    echo koru: clean-install accepts only --device and optional --contracts.
    exit /b 2
)
call :ensure_environment || exit /b 1
rem Existing launcher environment carries data; do not interpolate the target
rem into a PowerShell command string or invoke the regular alias resolver.
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\clean-install.ps1"
exit /b %ERRORLEVEL%

:do_launch
call :resolve_device || exit /b 1
call :launch_app
exit /b %ERRORLEVEL%

:do_run
call :resolve_device || exit /b 1
if defined GENERATE_CONTRACTS call :generate_contracts
if errorlevel 1 exit /b 1
call gradlew.bat :app:assembleDebug%ARGS%
if errorlevel 1 exit /b %ERRORLEVEL%
if defined ANDROID_RUN_LOCAL_API_PORT (
    echo [koru] forwarding device localhost:!ANDROID_RUN_LOCAL_API_PORT! to the local API
    "%ADB%" -s "%DEVICE%" reverse "tcp:!ANDROID_RUN_LOCAL_API_PORT!" "tcp:!ANDROID_RUN_LOCAL_API_PORT!"
    if errorlevel 1 (
        echo [koru] API port forwarding failed. Check the selected device connection. 1>&2
        exit /b 1
    )
)
REM The byte plane is a second port and a second origin. Forwarding only the
REM API gets a phone as far as allocating an upload and no further: it is then
REM handed the worker's own origin, dials a port nothing answers on, and
REM reports `byte_origin_refused` - which reads as a broken transfer engine
REM rather than a missing forward. Loopback here is deliberate: `adb reverse`
REM is what makes a phone's localhost this machine, so the allowlist's
REM loopback-only development rule holds without naming a LAN address that
REM changes with DHCP.
if defined ANDROID_RUN_LOCAL_OBJECT_PORT (
    echo [koru] forwarding device localhost:!ANDROID_RUN_LOCAL_OBJECT_PORT! to the local object worker
    "%ADB%" -s "%DEVICE%" reverse "tcp:!ANDROID_RUN_LOCAL_OBJECT_PORT!" "tcp:!ANDROID_RUN_LOCAL_OBJECT_PORT!"
    if errorlevel 1 (
        echo [koru] object worker port forwarding failed. Check the selected device connection. 1>&2
        exit /b 1
    )
)
"%ADB%" -s "%DEVICE%" install -r "app\build\outputs\apk\debug\app-debug.apk"
if errorlevel 1 exit /b %ERRORLEVEL%
call :launch_app
if errorlevel 1 exit /b %ERRORLEVEL%
goto do_logcat

:do_doctor
echo Koru Android environment
echo.
echo   JAVA_HOME:    %JAVA_HOME%
echo   Android SDK:  %ANDROID_HOME%
if defined KORU_DEVICE (echo   Device:       %KORU_DEVICE%) else (echo   Device:       auto-select one connected device)
echo.
"%JAVA_HOME%\bin\java.exe" -version
echo.
if exist "%ANDROID_HOME%\platforms\android-37.0\android.jar" (
    echo [ok] Android platform 37 installed
) else (
    echo [missing] Android platform 37
)
if exist "%ANDROID_HOME%\build-tools" (
    echo [ok] Android build-tools directory found
) else (
    echo [missing] Android build-tools
)
echo.
"%ADB%" devices -l
exit /b %ERRORLEVEL%

:do_avds
"%EMULATOR%" -list-avds
exit /b %ERRORLEVEL%

:do_emulator
if "%ARG1%"=="" (
    echo koru: emulator needs an AVD name.
    echo.
    echo Available AVDs:
    "%EMULATOR%" -list-avds
    exit /b 2
)
echo [koru] starting AVD "%ARG1%"...
start "Koru Android Emulator" "%EMULATOR%" -avd "%ARG1%"
echo [koru] use "koru.bat devices" to see when it connects.
echo [koru] use "koru.bat run" after sys.boot_completed reports 1.
exit /b 0

:do_devices
call :list_devices
if "!DEV_COUNT!"=="0" (
    echo No Android device or emulator is connected.
    echo Run "koru.bat avds", then "koru.bat emulator AVDNAME".
    exit /b 0
)
echo Connected devices:
for /l %%I in (1,1,!DEV_COUNT!) do call :show_device_row %%I
echo.
echo Run "koru.bat alias" to name the unnamed ones.
exit /b 0

:show_device_row
set "IDX=%~1"
call set "ROW_SERIAL=%%DEV_!IDX!%%"
call set "ROW_MODEL=%%DEVMODEL_!IDX!%%"
call :alias_of "!ROW_SERIAL!"
set "BOOT="
for /f "delims=" %%C in ('""%ADB%" -s "!ROW_SERIAL!" shell getprop sys.boot_completed"') do set "BOOT=%%C"
if "!BOOT!"=="1" (set "ROW_STATE=ready") else (set "ROW_STATE=booting")
if defined ALIAS_NAME (
    echo   !IDX!. !ALIAS_NAME! - !ROW_MODEL! - !ROW_STATE!
) else (
    echo   !IDX!. ^(unnamed^) - !ROW_MODEL! - !ROW_STATE!
)
echo        !ROW_SERIAL!
exit /b 0

:do_alias
rem With no arguments this walks the connected devices and asks for a name for
rem each one that has none, rather than printing a command for you to retype.
if "%ARG1%"=="" (
    call :name_devices
    exit /b !ERRORLEVEL!
)
call :valid_alias_name "%ARG1%" || exit /b 2
if "%ARG2%"=="" goto alias_ask
call :match_device "%ARG2%"
if errorlevel 1 goto alias_ask
goto alias_save

:alias_ask
call :pick_device "%ARG1%" || exit /b 1

:alias_save
set "ALIAS_%ARG1%=!MATCH!"
call :remember_alias "%ARG1%"
call :write_alias "%ARG1%"
echo [koru] device.%ARG1%=!MATCH! saved in %CD%\koru.local.conf
echo [koru] use it with: koru.bat run --device=%ARG1%
exit /b 0

:write_alias
findstr /i /v /b /c:"device.%~1=" "koru.local.conf" >"koru.local.conf.tmp" 2>nul
for /f "delims=" %%V in ('echo !ALIAS_%~1!') do >>"koru.local.conf.tmp" echo device.%~1=%%V
move /y "koru.local.conf.tmp" "koru.local.conf" 1>nul
exit /b 0

:show_aliases
if not defined ALIASES (
    echo No device aliases yet.
    exit /b 0
)
echo Device aliases:
for %%N in (!ALIASES!) do call :show_one_alias "%%N"
exit /b 0

:show_one_alias
echo   %~1 = !ALIAS_%~1!
exit /b 0

:remember_alias
rem Keep !ALIASES! a set: naming the same device twice must not list it twice.
for %%N in (!ALIASES!) do if /i "%%N"=="%~1" exit /b 0
set "ALIASES=!ALIASES! %~1"
exit /b 0

:valid_alias_name
rem An alias becomes a variable name, a koru.local.conf key after "device.",
rem and a --device= value, so letters, digits, "_" and "-" are the whole of it.
if "%~1"=="" goto bad_alias_name
echo(%~1| findstr /r /c:"^[A-Za-z0-9_-][A-Za-z0-9_-]*$" 1>nul 2>nul
if errorlevel 1 goto bad_alias_name
exit /b 0

:bad_alias_name
echo koru: "%~1" is not a usable alias. Use a short word: phone, tablet, pixel.
exit /b 1

:alias_of
rem Answers ALIAS_NAME with the name given to this serial, if it has one.
set "ALIAS_NAME="
for %%N in (!ALIASES!) do if /i "!ALIAS_%%N!"=="%~1" set "ALIAS_NAME=%%N"
exit /b 0

:list_devices
rem Fills DEV_COUNT and DEV_<n> / DEVMODEL_<n> / DEVDESC_<n> for every ready
rem device. Everything here goes through this one enumeration, because an mDNS
rem serial can contain a space -- "adb-HA1HAMMT-zBoAgN (2)._adb-tls-connect._tcp"
rem is one serial, not two tokens -- and splitting the line on whitespace drops
rem that device silently, which looks like "only one device is connected".
set "DEV_COUNT=0"
for /f "skip=1 delims=" %%L in ('""%ADB%" devices -l"') do call :take_device_line "%%L"
exit /b 0

:take_device_line
set "LINE=%~1"
if "!LINE!"=="" exit /b 0
rem Split on the state column instead: everything before " device " is the
rem serial, however many spaces it holds. The trailing space is what keeps
rem this off the "device:shiba" field further along the line.
set "TAIL=!LINE:* device =!"
if "!TAIL!"=="!LINE!" exit /b 0
call set "SERIAL=%%LINE: device !TAIL!=%%"
:take_device_trim
if "!SERIAL!"=="" exit /b 0
if "!SERIAL:~-1!"==" " (
    set "SERIAL=!SERIAL:~0,-1!"
    goto take_device_trim
)
set "MODEL="
for %%T in (!TAIL!) do (
    set "TOK=%%T"
    if /i "!TOK:~0,6!"=="model:" set "MODEL=!TOK:~6!"
)
if not defined MODEL set "MODEL=!SERIAL!"
set /a DEV_COUNT+=1
set "DEV_!DEV_COUNT!=!SERIAL!"
set "DEVMODEL_!DEV_COUNT!=!MODEL!"
set "DEVDESC_!DEV_COUNT!=!TAIL!"
exit /b 0

:name_devices
rem Ask for a name for every connected device that has none, and save each one.
call :list_devices
if "!DEV_COUNT!"=="0" (
    echo koru: no ready Android device or emulator is connected.
    echo Run "koru.bat avds", then "koru.bat emulator AVDNAME".
    exit /b 1
)
if defined KORU_NO_PROMPT (
    call :show_aliases
    exit /b 0
)
echo Connected devices:
for /l %%I in (1,1,!DEV_COUNT!) do call :name_one_device %%I
rem A pick straight after this one reuses the numbers already on screen.
set "DEVICES_SHOWN=1"
exit /b 0

:name_one_device
set "IDX=%~1"
call set "NAME_SERIAL=%%DEV_!IDX!%%"
call set "NAME_MODEL=%%DEVMODEL_!IDX!%%"
call :alias_of "!NAME_SERIAL!"
echo.
if defined ALIAS_NAME (
    echo   !IDX!. !NAME_MODEL! is named !ALIAS_NAME!
    echo      !NAME_SERIAL!
    exit /b 0
)
echo   !IDX!. !NAME_MODEL! has no name.
echo      !NAME_SERIAL!

:name_one_ask
set "NEWNAME="
set /p "NEWNAME=   name it (blank to skip): "
if not defined NEWNAME exit /b 0
call :valid_alias_name "!NEWNAME!" || goto name_one_ask
set "ALIAS_!NEWNAME!=!NAME_SERIAL!"
call :remember_alias "!NEWNAME!"
call :write_alias "!NEWNAME!"
echo   [koru] device.!NEWNAME!=!NAME_SERIAL! saved in %CD%\koru.local.conf
exit /b 0

:pick_device
rem Answers MATCH with the device the person points at. %1 is what the choice
rem is for -- an alias being defined, or the command about to run.
call :list_devices
if "!DEV_COUNT!"=="0" (
    echo koru: no ready Android device or emulator is connected.
    echo Run "koru.bat avds", then "koru.bat emulator AVDNAME".
    exit /b 1
)
if defined KORU_NO_PROMPT (
    echo koru: KORU_NO_PROMPT is set, so nothing was asked.
    echo Name a device with "koru.bat alias NAME SERIAL", or pass --device.
    exit /b 1
)
if defined DEVICES_SHOWN goto pick_device_ask
echo.
echo Connected devices:
for /l %%I in (1,1,!DEV_COUNT!) do call :show_pick_row %%I

:pick_device_ask
echo.
set "PICK="
set /p "PICK=   device for %~1 (number, blank to cancel): "
if not defined PICK exit /b 1
for /f "delims=0123456789" %%X in ("!PICK!") do goto pick_device_again
if !PICK! LSS 1 goto pick_device_again
if !PICK! GTR !DEV_COUNT! goto pick_device_again
call set "MATCH=%%DEV_!PICK!%%"
exit /b 0

:pick_device_again
echo   koru: answer with a number between 1 and !DEV_COUNT!.
goto pick_device_ask

:show_pick_row
set "IDX=%~1"
call set "ROW_SERIAL=%%DEV_!IDX!%%"
call set "ROW_MODEL=%%DEVMODEL_!IDX!%%"
call :alias_of "!ROW_SERIAL!"
if defined ALIAS_NAME (
    echo   !IDX!. !ROW_MODEL! ^(already named !ALIAS_NAME!^)
) else (
    echo   !IDX!. !ROW_MODEL!
)
echo        !ROW_SERIAL!
exit /b 0

:do_stop
if not "%ARG1%"=="" set "KORU_DEVICE=%ARG1%"
call :resolve_device || exit /b 1
echo [koru] stopping %DEVICE%...
"%ADB%" -s "%DEVICE%" emu kill
exit /b %ERRORLEVEL%

:do_logcat
call :resolve_device || exit /b 1
if /i "%ARG1%"=="process" goto logcat_process
if /i "%ARG1%"=="crash" goto logcat_crash
if /i "%ARG1%"=="all" goto logcat_all
if /i "%ARG1%"=="sync" goto logcat_sync
if /i "%ARG1%"=="dump" goto logcat_dump
echo [koru] streaming KoruMobile logs from %DEVICE%. Press Ctrl+C to stop.
"%ADB%" -s "%DEVICE%" logcat -v threadtime KoruMobile:D "*:S"
exit /b %ERRORLEVEL%

:logcat_sync
rem The Rust replication host traces to stderr, which logcat captures under
rem the native stream tags — KoruMobile at debug, the streams at info so
rem workspace_sync/inventory lines come through, everything else at warn.
echo [koru] streaming KoruMobile and replication logs from %DEVICE%. Press Ctrl+C to stop.
"%ADB%" -s "%DEVICE%" logcat -v threadtime KoruMobile:D stderr:I stdout:I *:W
exit /b %ERRORLEVEL%

:logcat_dump
rem Write the device's current log buffer to a file and exit, so it can be
rem grepped or paged instead of scrolling past in a terminal. Same filters as
rem "logcat sync": KoruMobile structured events plus the Rust replication
rem traces on the native stderr stream.
set "LOGCAT_OUT=%ARG2%"
if "%LOGCAT_OUT%"=="" set "LOGCAT_OUT=koru-logcat.txt"
"%ADB%" -s "%DEVICE%" logcat -d -v threadtime KoruMobile:D stderr:I stdout:I *:W > "%LOGCAT_OUT%"
if errorlevel 1 exit /b %ERRORLEVEL%
echo [koru] wrote "%LOGCAT_OUT%"
exit /b 0

:logcat_process
set "KORU_PID="
for /f "delims=" %%P in ('""%ADB%" -s "%DEVICE%" shell pidof -s do.koru.app"') do set "KORU_PID=%%P"
if not defined KORU_PID (
    echo koru: Koru is not running on %DEVICE%. Use "koru.bat launch" first.
    exit /b 1
)
echo [koru] streaming PID !KORU_PID! from %DEVICE%. Press Ctrl+C to stop.
"%ADB%" -s "%DEVICE%" logcat --pid=!KORU_PID! -v threadtime
exit /b %ERRORLEVEL%

:logcat_crash
"%ADB%" -s "%DEVICE%" logcat -b crash -v threadtime
exit /b %ERRORLEVEL%

:logcat_all
echo [koru] streaming all logs from %DEVICE%. Press Ctrl+C to stop.
"%ADB%" -s "%DEVICE%" logcat -v threadtime
exit /b %ERRORLEVEL%

:launch_app
echo [koru] launching Koru on %DEVICE%...
set "PEER_LOG_ARG="
set "ACCOUNT_LOG_ARG="
if "%ANDROID_RUN_LOCAL_ACCOUNT_LOGS%"=="1" set "ACCOUNT_LOG_ARG=--ez koru_account_logs true"
if /i "%KORU_PEER_OBJECT_LOGS%"=="1" set "PEER_LOG_ARG=--ez koru_peer_object_logs true"
if /i "%KORU_PEER_OBJECT_LOGS%"=="true" set "PEER_LOG_ARG=--ez koru_peer_object_logs true"
if /i "%KORU_PEER_OBJECT_LOGS%"=="on" set "PEER_LOG_ARG=--ez koru_peer_object_logs true"
"%ADB%" -s "%DEVICE%" shell am start %PEER_LOG_ARG% %ACCOUNT_LOG_ARG% -n do.koru.app/app.koru.mobile.MainActivity
exit /b %ERRORLEVEL%

:do_contracts
call :generate_contracts
exit /b %ERRORLEVEL%

:generate_contracts
where.exe buf 1>nul 2>nul
if errorlevel 1 (
    echo koru: Buf was not found on PATH.
    echo Install it from https://buf.build/docs/cli/installation/ and try again.
    exit /b 1
)
echo [koru] generating Rust, Kotlin, and Swift contract bindings...
pushd "%REPO_ROOT%" || exit /b 1
buf generate contracts --template contracts\buf.gen.yaml
set "CONTRACT_RESULT=!ERRORLEVEL!"
popd
if not "!CONTRACT_RESULT!"=="0" exit /b !CONTRACT_RESULT!
echo [koru] generated contracts\gen from the repository schemas.
exit /b 0

:resolve_device
if not defined DEVICE_WANTED goto resolve_configured
call :match_device "!DEVICE_WANTED!"
if not errorlevel 1 (
    set "DEVICE=!MATCH!"
    exit /b 0
)
rem A --device name that matches nothing is read as a name not given yet: ask
rem which device it is, save that, and carry on with the command.
call :valid_alias_name "!DEVICE_WANTED!" || exit /b 1
call :pick_device "!DEVICE_WANTED!" || exit /b 1
set "ALIAS_!DEVICE_WANTED!=!MATCH!"
call :remember_alias "!DEVICE_WANTED!"
call :write_alias "!DEVICE_WANTED!"
echo [koru] device.!DEVICE_WANTED!=!MATCH! saved in %CD%\koru.local.conf
set "DEVICE=!MATCH!"
exit /b 0

:resolve_configured
if defined KORU_DEVICE (
    set "DEVICE=%KORU_DEVICE%"
    rem Delayed expansion: %DEVICE% would still be the value this block started with.
    "%ADB%" -s "!DEVICE!" get-state 1>nul 2>nul
    if errorlevel 1 (
        echo koru: configured device "!DEVICE!" is not connected.
        exit /b 1
    )
    exit /b 0
)
set "DEVICE="
call :list_devices
if "!DEV_COUNT!"=="0" (
    echo koru: no ready Android device or emulator is connected.
    echo Run "koru.bat avds", then "koru.bat emulator AVDNAME".
    exit /b 1
)
if "!DEV_COUNT!"=="1" (
    set "DEVICE=!DEV_1!"
    exit /b 0
)
rem More than one, and nothing said which. Name the unnamed ones first -- that
rem answer is worth keeping -- then ask which of them this command is for.
echo koru: more than one device is connected.
call :name_devices || exit /b 1
call :pick_device "this command" || exit /b 1
set "DEVICE=!MATCH!"
call :alias_of "!DEVICE!"
if defined ALIAS_NAME echo [koru] using !ALIAS_NAME!. Skip this next time with --device=!ALIAS_NAME!
exit /b 0

:apply_alias
rem An alias named in koru.local.conf as device.<name>=SERIAL wins outright.
if not defined ALIAS_%~1 exit /b 0
call set "KORU_DEVICE=%%ALIAS_%~1%%"
set "DEVICE_WANTED="
exit /b 0

:match_device
rem %1 is an alias that did not resolve, a serial, or part of a model or product name.
set "MATCH="
set "MATCH_COUNT=0"
set "MATCH_EXACT="
call :list_devices
for /l %%I in (1,1,!DEV_COUNT!) do call :match_one_device %%I "%~1"
if "!MATCH_COUNT!"=="0" (
    echo koru: no connected device matches "%~1".
    if defined ALIASES echo Known aliases:!ALIASES!
    exit /b 1
)
if not "!MATCH_COUNT!"=="1" (
    echo koru: "%~1" matches more than one connected device.
    exit /b 1
)
exit /b 0

:match_one_device
if defined MATCH_EXACT exit /b 0
set "IDX=%~1"
call set "TRY_SERIAL=%%DEV_!IDX!%%"
call set "TRY_DESC=%%DEVDESC_!IDX!%%"
if /i "!TRY_SERIAL!"=="%~2" (
    set "MATCH=!TRY_SERIAL!"
    set "MATCH_COUNT=1"
    set "MATCH_EXACT=1"
    exit /b 0
)
echo(!TRY_SERIAL! !TRY_DESC!| findstr /i /c:"%~2" 1>nul 2>nul
if errorlevel 1 exit /b 0
set /a MATCH_COUNT+=1
set "MATCH=!TRY_SERIAL!"
exit /b 0

:ensure_environment
call :discover_java || exit /b 1
call :discover_android || exit /b 1
set "ADB=%ANDROID_HOME%\platform-tools\adb.exe"
set "EMULATOR=%ANDROID_HOME%\emulator\emulator.exe"
if not exist "%ADB%" (
    echo koru: adb not found at "%ADB%".
    echo Install Android SDK Platform-Tools in Android Studio.
    exit /b 1
)
if not exist "%EMULATOR%" (
    echo koru: emulator not found at "%EMULATOR%".
    echo Install Android Emulator in Android Studio's SDK Manager.
    exit /b 1
)
exit /b 0

:discover_java
set "JAVA_READY="
if defined KORU_JAVA_HOME call :try_jdk "%KORU_JAVA_HOME%"
if not defined JAVA_READY if defined JAVA_HOME call :try_jdk "%JAVA_HOME%"
if not defined JAVA_READY call :try_jdk "C:\dev\jdk\azul-18.0.2"
if not defined JAVA_READY call :try_jdk "%ProgramFiles%\Android\Android Studio\jbr"
if not defined JAVA_READY for /d %%D in ("%ProgramFiles%\Eclipse Adoptium\jdk-*") do call :try_jdk "%%~fD"
if not defined JAVA_READY for /d %%D in ("%ProgramFiles%\Microsoft\jdk-*") do call :try_jdk "%%~fD"
if not defined JAVA_READY for /d %%D in ("%ProgramFiles%\Java\jdk-*") do call :try_jdk "%%~fD"
if not defined JAVA_READY for /d %%D in ("%ProgramFiles%\Zulu\zulu-*") do call :try_jdk "%%~fD"
if not defined JAVA_READY for /d %%D in ("%USERPROFILE%\.jdks\*") do call :try_jdk "%%~fD"
if not defined JAVA_READY (
    echo koru: no JDK 17 or newer was found.
    echo Run: koru.bat setup "C:\path\to\jdk" "%LOCALAPPDATA%\Android\Sdk"
    exit /b 1
)
exit /b 0

:try_jdk
if defined JAVA_READY exit /b 0
if not exist "%~1\bin\java.exe" exit /b 0
if not exist "%~1\release" exit /b 0
set "FOUND_JAVA_VERSION="
for /f "tokens=2 delims==" %%V in ('findstr /b /c:"JAVA_VERSION=" "%~1\release"') do set "FOUND_JAVA_VERSION=%%~V"
if not defined FOUND_JAVA_VERSION exit /b 0
for /f "tokens=1 delims=." %%M in ("!FOUND_JAVA_VERSION!") do set "FOUND_JAVA_MAJOR=%%M"
for /f "delims=0123456789" %%X in ("!FOUND_JAVA_MAJOR!") do exit /b 0
if !FOUND_JAVA_MAJOR! LSS 17 exit /b 0
set "JAVA_HOME=%~1"
set "JAVA_READY=1"
exit /b 0

:discover_android
set "ANDROID_READY="
if defined KORU_ANDROID_HOME call :try_android "%KORU_ANDROID_HOME%"
if not defined ANDROID_READY if defined ANDROID_HOME call :try_android "%ANDROID_HOME%"
if not defined ANDROID_READY if defined ANDROID_SDK_ROOT call :try_android "%ANDROID_SDK_ROOT%"
if not defined ANDROID_READY call :try_android "%LOCALAPPDATA%\Android\Sdk"
if not defined ANDROID_READY (
    echo koru: Android SDK was not found.
    echo Install it with Android Studio, then run:
    echo   koru.bat setup "%JAVA_HOME%" "C:\path\to\Android\Sdk"
    exit /b 1
)
set "ANDROID_SDK_ROOT=%ANDROID_HOME%"
exit /b 0

:try_android
if defined ANDROID_READY exit /b 0
if not exist "%~1\platform-tools\adb.exe" exit /b 0
set "ANDROID_HOME=%~1"
set "ANDROID_READY=1"
exit /b 0

:show_environment
echo Koru Android environment
echo.
echo   JAVA_HOME:    %JAVA_HOME%
echo   Android SDK:  %ANDROID_HOME%
if defined KORU_DEVICE (echo   Device:       %KORU_DEVICE%) else (echo   Device:       auto-select one connected device)
echo.
"%JAVA_HOME%\bin\java.exe" -version
exit /b 0

:help_error
call :do_help
exit /b 2

:do_help
echo Koru Android
echo.
echo usage: koru.bat ^<command^> [arguments]
echo.
echo   run                 build, install, launch, then stream KoruMobile logs
echo   run-local           load .env.local account settings, then run
echo   run-prod            load .env.production HTTPS account settings, then run
echo   build               build app\build\outputs\apk\debug\app-debug.apk
echo   test                run Android unit tests
echo   check               compile Kotlin without building the APK
echo   install             build and install without launching
echo   clean-install       build, then erase and reinstall on an explicit exact --device
echo   launch              launch the already-installed app
echo   clean               remove Android build output
echo   doctor ^| env        show JDK, SDK, platform and connected devices
echo   setup [jdk] [sdk]   discover or save machine-local JDK and SDK paths
echo   contracts           regenerate Rust, Kotlin, and Swift contract bindings
echo   avds                list configured Android Virtual Devices
echo   emulator ^<avd^>    start an AVD in a visible emulator window
echo   devices             list devices, aliases, and boot readiness
echo   alias               ask for a name for each unnamed connected device
echo   alias ^<name^> [dev]  name one device so --device=^<name^> finds it
echo   stop [serial]       stop an emulator
echo   logcat              stream only structured KoruMobile logs
echo   logcat sync         stream KoruMobile plus replication (workspace_sync) logs
echo   logcat dump [file]  write the current log buffer to a file (default koru-logcat.txt)
echo   logcat process      stream every log from the running Koru process
echo   logcat crash        show Android's crash log buffer
echo   logcat all          stream all device logs
echo.
echo Set KORU_PEER_OBJECT_LOGS=1 before run to include per-profile,
echo per-workspace and per-document receive logs. Unset it to keep them quiet.
echo   gradle ^<args...^>    pass arguments directly to the Gradle wrapper
echo   help                this message
echo.
echo Build/test/check arguments after the command are forwarded to Gradle:
echo   koru.bat test --stacktrace
echo   koru.bat gradle :app:dependencies
echo.
echo Add --contracts to build, test, check, install, or run to generate first:
echo   koru.bat build --contracts
echo.
echo The runner auto-detects a JDK 17+ and the standard Android SDK location.
echo Use "koru.bat setup" to save detected paths in ignored koru.local.conf.
echo With more than one device connected, a command asks which one it is for
echo and remembers the names you give:
echo   koru.bat alias                  name every unnamed device, once
echo   koru.bat run --device=tablet    a name it does not know is asked about
echo --device also takes a serial, or any part of a model or product name:
echo   koru.bat logcat --device=pixel
echo Set KORU_DEVICE=emulator-5554 for a default, or KORU_NO_PROMPT=1 to make
echo an ambiguous device an error instead of a question.
echo clean-install always requires an exact serial or complete model, never an alias/default:
echo   koru.bat clean-install --device emulator-5554
echo It erases the selected app's Store, trust, device seeds and account vault.
exit /b 0
