# Builds the installer from target/release/voicechanger.exe (run `cargo build --release` first).
# Uses Inno Setup 6, installing it with Chocolatey when missing (CI runners).
# Prints the path of the built installer.
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot

$iscc = Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe"
if (-not (Test-Path $iscc)) {
    choco install innosetup -y --no-progress | Out-Null
    if (-not (Test-Path $iscc)) { throw "Inno Setup not found at $iscc" }
}

$version = (Select-String -Path (Join-Path $root "Cargo.toml") -Pattern '^version = "(.+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
& $iscc /Qp "/DAppVersion=$version" (Join-Path $PSScriptRoot "voicechanger.iss") | Out-Host
if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }

$setup = Join-Path $root "target\installer\VoiceChanger-Setup-$version.exe"
if (-not (Test-Path $setup)) { throw "installer not found at $setup" }
$setup
