<#
.SYNOPSIS
  Removes the Clipper logon task and companion binaries, then restores OBS (restore-obs.ps1).
  Keeps config.json, the log, OBS backups and any clips still in the queue.
#>
[CmdletBinding()]
param(
    [switch]$KeepObsSettings,
    [string]$TranscriptPath    # internal: set when re-run outside an app package
)
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'lib\outside-package.ps1')
if (Invoke-OutsidePackage $PSCommandPath $PSBoundParameters) { exit 0 }
if ($TranscriptPath) { Start-Transcript -Path $TranscriptPath | Out-Null }
trap { Write-Host "ERROR: $_" -ForegroundColor Red; exit 1 }

$Root = Join-Path $env:LOCALAPPDATA 'clipper'

Get-Process clipper -ErrorAction SilentlyContinue | Stop-Process -Force
if (Get-ScheduledTask -TaskName 'Clipper' -ErrorAction SilentlyContinue) {
    Unregister-ScheduledTask -TaskName 'Clipper' -Confirm:$false
    Write-Host 'Removed the Clipper logon task.'
}
if (Test-Path "$Root\bin") { Remove-Item "$Root\bin" -Recurse -Force }

if (-not $KeepObsSettings) { & (Join-Path $PSScriptRoot 'restore-obs.ps1') }

$queued = @(Get-ChildItem "$Root\queue" -Recurse -File -ErrorAction SilentlyContinue)
if ($queued.Count -gt 0) {
    Write-Warning "$($queued.Count) clip(s) never reached the server and are still in $Root\queue. Copy them to <share>\Kept\<Game>\ by hand."
}
if ($TranscriptPath) { Stop-Transcript | Out-Null }
