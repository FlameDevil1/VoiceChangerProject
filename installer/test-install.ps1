# Installs the built installer silently (with "start with Windows"), checks what it put where,
# then uninstalls and checks everything is gone. Run by CI after building the installer:
#   ./installer/test-install.ps1 target/installer/VoiceChanger-Setup-0.1.0.exe
param([Parameter(Mandatory)] [string]$Setup)
$ErrorActionPreference = "Stop"

$app = Join-Path $env:LOCALAPPDATA "Programs\Voice Changer"
$exe = Join-Path $app "voicechanger.exe"
$shortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Voice Changer.lnk"
$run = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{E56CA3C2-52E7-4F3D-953D-4F6835C647EE}_is1"

function Check([bool]$ok, [string]$what) {
    if (-not $ok) { throw "FAILED: $what" }
    Write-Host "ok: $what"
}
function RunValue { (Get-ItemProperty $run -ErrorAction SilentlyContinue)."Voice Changer" }

$p = Start-Process $Setup -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/TASKS=startup" -PassThru -Wait
Check ($p.ExitCode -eq 0) "installer exit code 0 (got $($p.ExitCode))"
Check (Test-Path $exe) "app installed to $app"
Check (Test-Path $shortcut) "Start menu shortcut"
Check ((RunValue) -eq "`"$exe`" --startup") "start-with-Windows entry points at the installed app"
$entry = Get-ItemProperty $uninstallKey
Check ($entry.DisplayName -eq "Voice Changer") "listed in Apps & features"
$version = (Get-Item $exe).VersionInfo
Check ($version.ProductName -eq "Voice Changer") "version info embedded ($($version.ProductVersion))"

# The uninstaller copies itself to %TEMP% and continues there: wait for the files to go.
$uninstaller = Join-Path $app "unins000.exe"
Start-Process $uninstaller -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART" -Wait
for ($i = 0; $i -lt 60 -and (Test-Path $exe); $i++) { Start-Sleep -Milliseconds 500 }
Check (-not (Test-Path $exe)) "app removed"
Check (-not (Test-Path $shortcut)) "shortcut removed"
for ($i = 0; $i -lt 20 -and (RunValue); $i++) { Start-Sleep -Milliseconds 500 }
Check (-not (RunValue)) "start-with-Windows entry removed"
Check (-not (Test-Path $uninstallKey)) "removed from Apps & features"
