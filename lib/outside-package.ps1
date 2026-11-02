# Processes started from an MSIX-packaged app (the Claude desktop app, Windows Terminal from the
# Store, ...) get new folders under AppData silently redirected into the package's private
# LocalCache, where Task Scheduler, OBS and everything else can't see them. Scripts that install
# into %LOCALAPPDATA% dot-source this and call Invoke-OutsidePackage first: inside a package it
# re-runs the script through a one-shot scheduled task (always unpackaged) and relays its output.

function Test-InPackage {
    # Children of a packaged app can have AppData redirected without reporting a package identity
    # (GetCurrentPackageFamilyName says "no package"), so test the redirection itself.
    $probe = 'clipper-probe-' + [guid]::NewGuid().ToString('N')
    $path = Join-Path $env:LOCALAPPDATA $probe
    New-Item -ItemType Directory $path | Out-Null
    try {
        return [bool](Get-ChildItem "$env:LOCALAPPDATA\Packages\*\LocalCache\Local\$probe" -Directory -ErrorAction SilentlyContinue)
    }
    finally {
        Remove-Item $path -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# Returns $true when the script was re-run outside the package (the caller should exit).
function Invoke-OutsidePackage([string]$ScriptPath, [hashtable]$BoundParameters) {
    if (-not (Test-InPackage)) { return $false }

    $task = 'Clipper-' + [IO.Path]::GetFileNameWithoutExtension($ScriptPath)
    $log  = Join-Path (Split-Path $ScriptPath) ".$task.log"
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$ScriptPath`"")
    foreach ($kv in $BoundParameters.GetEnumerator()) {
        if ($kv.Value -is [switch]) { if ($kv.Value) { $argList += "-$($kv.Key)" } }
        else { $argList += "-$($kv.Key)"; $argList += "`"$($kv.Value)`"" }
    }
    $argList += @('-TranscriptPath', "`"$log`"")

    Write-Host "Running inside an app package (AppData would be virtualized); re-running $([IO.Path]::GetFileName($ScriptPath)) outside it via Task Scheduler..." -ForegroundColor Yellow
    Remove-Item $log -ErrorAction SilentlyContinue
    $user = "$env:USERDOMAIN\$env:USERNAME"
    Register-ScheduledTask -TaskName $task -Force `
        -Action (New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ($argList -join ' ') -WorkingDirectory (Split-Path $ScriptPath)) `
        -Principal (New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited) `
        -Settings (New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Minutes 30)) | Out-Null
    Start-ScheduledTask -TaskName $task
    Start-Sleep -Seconds 2
    $deadline = (Get-Date).AddMinutes(30)
    while ((Get-ScheduledTask -TaskName $task).State -in 'Running', 'Queued' -and (Get-Date) -lt $deadline) { Start-Sleep -Seconds 1 }
    $result = (Get-ScheduledTaskInfo -TaskName $task).LastTaskResult
    Unregister-ScheduledTask -TaskName $task -Confirm:$false

    if (Test-Path $log) {
        # Drop the transcript header/footer.
        Get-Content $log | Where-Object { $_ -notmatch '^\*{10}|^(Windows PowerShell transcript|Start time|End time|Username|RunAs User|Configuration Name|Machine|Host Application|Process ID|PSVersion|PSEdition|PSCompatibleVersions|BuildVersion|CLRVersion|WSManStackVersion|PSRemotingProtocolVersion|SerializationVersion|Transcript started)' } | Write-Host
        Remove-Item $log -ErrorAction SilentlyContinue
    }
    if ($result -ne 0) { throw "$([IO.Path]::GetFileName($ScriptPath)) failed outside the package (exit $result)" }
    return $true
}
