@echo off
rem Trawl 元搜浏览器 启动脚本（调试版）
cd /d %~dp0
if exist "src-tauri\target\debug\trawl.exe" (
  start "" "src-tauri\target\debug\trawl.exe"
) else (
  echo [Trawl] 尚未编译完成，请先运行：cargo build（在 src-tauri 目录）
  pause
)
