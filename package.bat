@echo off
rem 打包 Nova 更新 zip（不上传；用 GitHub Release 分发）
cd /d "%~dp0"
python scripts\package.py %*
