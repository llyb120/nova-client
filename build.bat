@echo off
setlocal
chcp 65001 >nul
cd /d "%~dp0"

where node >nul 2>nul || (echo [错误] 未找到 Node.js，请先安装 && exit /b 1)
where cargo >nul 2>nul || (echo [错误] 未找到 Rust/cargo，请先安装 && exit /b 1)

echo [1/2] 安装前端依赖...
call npm install
if errorlevel 1 goto :fail

echo [2/2] 构建 release 版本（首次编译需要几分钟）...
call npx tauri build --no-bundle
if errorlevel 1 goto :fail

echo.
echo ============================================================
echo  构建成功！
echo  可执行文件: %~dp0src-tauri\target\release\Nova.exe
echo  运行前提: 目标机器已安装 devin CLI 并完成登录
echo ============================================================
exit /b 0

:fail
echo.
echo 构建失败，请检查上方错误信息。
exit /b 1
