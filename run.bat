@echo off
setlocal

taskkill /f /im shok.exe
timeout /t 3
cargo run --release

endlocal
