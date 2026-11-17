$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
cargo test --release
cargo build --release
Write-Host "Built: target\release\cursorfect.exe"
