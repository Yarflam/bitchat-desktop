@echo off
rem Desktop UI for the bitchatd mesh node (visible window).
rem Logs are appended to %TEMP%\bitchatd_gui.log
rem The sparse package (sparse\AppxManifest.xml) grants package identity so
rem the BLE advertisement publisher works; register it once with:
rem   powershell -Command "Add-AppxPackage -Path '%~dp0sparse\AppxManifest.xml' -Register -ExternalLocation '%~dp0target\debug\examples'"
set BITCHATD_DEBUG_HEX=1
cd /d "%~dp0"
copy /y "sparse\node_gui.exe.manifest" "target\debug\examples\" >nul
cargo run -q -p bitchat-node --example node_gui >> "%TEMP%\bitchatd_gui.log" 2>&1
