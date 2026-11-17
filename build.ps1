$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
cargo test --release
if ($LASTEXITCODE -ne 0) { throw "Release tests failed" }
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "Release build failed" }
# Portable local build: keep the user's MP3 filenames and bytes intact. Nothing
# is downloaded, embedded in the exe, or added to Git by this script.
if (Test-Path (Join-Path $PSScriptRoot "MIHassets")) {
    $destination = Join-Path $PSScriptRoot "target\release\MIHassets"
    New-Item -ItemType Directory -Force -Path $destination | Out-Null
    Get-ChildItem (Join-Path $PSScriptRoot "MIHassets") -File | Copy-Item -Destination $destination
}
Write-Host "Built: target\release\cursorfect.exe"
