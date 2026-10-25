@echo off
rem Desktop UI for the bitchatd mesh node (visible window).
rem Logs are appended to %TEMP%\bitchatd_gui.log
set BITCHATD_DEBUG_HEX=1
cd /d "%~dp0"
cargo run -q -p bitchat-node --example node_gui >> "%TEMP%\bitchatd_gui.log" 2>&1
