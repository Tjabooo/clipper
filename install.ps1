<#
.SYNOPSIS
  Clipper one-line install for Windows: downloads Clipper and runs windows\install.ps1.

.EXAMPLE
  irm https://raw.githubusercontent.com/Tjabooo/clipper/main/install.ps1 | iex

.EXAMPLE
  & ([scriptblock]::Create((irm https://raw.githubusercontent.com/Tjabooo/clipper/main/install.ps1))) -Share \\myserver\Clips -Quality high

.NOTES
  Takes the same options as windows\install.ps1 (-Share, -LilinUrl, -Quality, -Bench, -Fps,
  -Native, -Encoder, -Bitrate, -Force, -NoAutostart, -NoStart), plus -Ref (branch or tag, default main).
#>
[CmdletBinding()]
param(
    [string]$Share,
    [string]$LilinUrl,
    [ValidateSet('', 'compact', 'balanced', 'high', 'max', 'smooth')]
    [string]$Quality,
    [ValidateSet('', 'quality', 'smooth', 'balanced', 'small')]
    [string]$Bench,
    [int]$Fps,
    [switch]$Native,
    [string]$Encoder,
    [int]$Bitrate,
    [switch]$Force,
    [switch]$NoAutostart,
    [switch]$NoStart,
    [string]$Ref = 'main',
    [string]$Repo = 'Tjabooo/clipper'
)
$ErrorActionPreference = 'Stop'
# `irm | iex` runs under the machine's policy, which may refuse the installer's .ps1 files
Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass -Force

if ($PSScriptRoot -and (Test-Path (Join-Path $PSScriptRoot 'windows\install.ps1'))) {
    $src = $PSScriptRoot   # run from a checkout: no download
} else {
    $src = Join-Path $env:LOCALAPPDATA 'clipper\src'
    $zip = Join-Path $env:TEMP "clipper-$([guid]::NewGuid().ToString('n')).zip"
    $tmp = "$zip.d"
    Write-Host "==> Downloading $Repo ($Ref)" -ForegroundColor Cyan
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest "https://codeload.github.com/$Repo/zip/$Ref" -OutFile $zip -UseBasicParsing
    Expand-Archive $zip $tmp -Force
    $top = Get-ChildItem $tmp -Directory | Select-Object -First 1
    if (-not (Test-Path (Join-Path $top.FullName 'windows\install.ps1'))) { throw 'The download has no windows\install.ps1' }
    if (Test-Path $src) { Remove-Item $src -Recurse -Force }
    New-Item -ItemType Directory -Force (Split-Path $src) | Out-Null
    Move-Item $top.FullName $src
    Remove-Item $zip, $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

$pass = @{}
foreach ($k in $PSBoundParameters.Keys) { if ($k -notin 'Ref', 'Repo') { $pass[$k] = $PSBoundParameters[$k] } }
$env:CLIPPER_REF = $Ref; $env:CLIPPER_REPO = $Repo   # which release -Bench downloads clipper-bench from
& (Join-Path $src 'windows\install.ps1') @pass
