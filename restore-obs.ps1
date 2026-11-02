<#
.SYNOPSIS
  Puts OBS back the way it was before Clipper: restores the oldest backup made by install.ps1
  (profiles, scene collections, global.ini, user.ini, obs-websocket config) and removes the
  Clipper profile and scene collection. Does not touch the companion; see uninstall.ps1.

.EXAMPLE
  .\restore-obs.ps1                     # oldest backup = state before the first install
  .\restore-obs.ps1 -Backup 20261102-141500
#>
[CmdletBinding()]
param(
    [string]$Backup,
    [string]$TranscriptPath    # internal: set when re-run outside an app package
)
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'lib\outside-package.ps1')
if (Invoke-OutsidePackage $PSCommandPath $PSBoundParameters) { exit 0 }
if ($TranscriptPath) { Start-Transcript -Path $TranscriptPath | Out-Null }
trap { Write-Host "ERROR: $_" -ForegroundColor Red; exit 1 }

$ObsDir  = Join-Path $env:APPDATA 'obs-studio'
$Backups = Join-Path $env:LOCALAPPDATA 'clipper\obs-backup'

if (-not (Test-Path $Backups)) { throw "No backups in $Backups" }
if ($Backup) { $dir = Join-Path $Backups $Backup }
else { $dir = (Get-ChildItem $Backups -Directory | Sort-Object Name | Select-Object -First 1).FullName }
if (-not $dir -or -not (Test-Path $dir)) { throw "Backup not found: $dir" }

# The companion would relaunch OBS, and OBS writes its config on exit.
Get-Process clipper -ErrorAction SilentlyContinue | Stop-Process -Force
if (Get-Process obs64 -ErrorAction SilentlyContinue) {
    Write-Host 'Closing OBS...'
    Get-Process obs64 | Stop-Process -Force
    Start-Sleep -Seconds 2
}

Write-Host "Restoring OBS settings from $dir"
if (Test-Path (Join-Path $dir 'basic')) {
    # Mirror so the Clipper profile/collection (not in the backup) disappear too.
    robocopy (Join-Path $dir 'basic') (Join-Path $ObsDir 'basic') /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "robocopy failed ($LASTEXITCODE)" }
}
foreach ($f in 'global.ini', 'user.ini') {
    if (Test-Path (Join-Path $dir $f)) { Copy-Item (Join-Path $dir $f) (Join-Path $ObsDir $f) -Force }
}
$ws = Join-Path $dir 'plugin_config\obs-websocket'
if (Test-Path $ws) { Copy-Item "$ws\*" (Join-Path $ObsDir 'plugin_config\obs-websocket') -Recurse -Force }

Write-Host 'OBS restored. The Clipper companion (if still installed) would relaunch OBS with the now-missing Clipper profile; run .\uninstall.ps1 to remove it.' -ForegroundColor Green
if ($TranscriptPath) { Stop-Transcript | Out-Null }
