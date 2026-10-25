@echo off
set BITCHATD_DEBUG_HEX=1
cd /d "%~dp0"
"%~dp0target\debug\examples\node_chat.exe" "Hello from bitchatd" > "%TEMP%\bitchatd_node.log" 2>&1
