<#
.SYNOPSIS
    Deploy keep-vm-resolution to Windows VMs over OpenSSH and start it in the
    logged-on user's console session.

.DESCRIPTION
    For each host:
      1. stop any running instance (and remove leftover helper tasks),
      2. copy keep-vm-resolution.exe and keep-vm-resolution-bg.exe,
      3. write the preset (`set W H`) and register autostart (`install --no-start`,
         HKCU Run key of the SSH user),
      4. start keep-vm-resolution-bg.exe in the user's interactive console session
         through a one-off scheduled task (SSH runs in its own non-interactive
         session, where display APIs see nothing),
      5. verify that it runs and show the last log lines.

    Requires OpenSSH key authentication to each host and the user logged on to
    the console. Remote steps are sent as -EncodedCommand, so no quoting issues.

.EXAMPLE
    .\deploy\deploy.ps1 192.0.2.11, 192.0.2.12, 192.0.2.13

.EXAMPLE
    .\deploy\deploy.ps1 -Hosts 192.0.2.20 -User svc-display -Resolution 1600x900
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory, Position = 0)]
    [string[]] $Hosts,
    [string] $User = 'admin',
    [ValidatePattern('^\d{3,5}x\d{3,5}$')]
    [string] $Resolution = '1920x1080',
    [string] $Dest = 'C:\Tools\KeepVMResolution',
    [string] $BuildDir = (Join-Path $PSScriptRoot '..\target\release'),
    [int] $ConnectTimeout = 10
)

$ErrorActionPreference = 'Stop'
$width, $height = $Resolution -split 'x'

$cli = Join-Path $BuildDir 'keep-vm-resolution.exe'
$bg = Join-Path $BuildDir 'keep-vm-resolution-bg.exe'
foreach ($f in $cli, $bg) {
    if (-not (Test-Path $f)) { throw "missing $f - build first: cargo build --release" }
}

$ssh = (Get-Command ssh.exe -ErrorAction Stop).Source
$scp = (Get-Command scp.exe -ErrorAction Stop).Source
$sshOpts = @('-o', 'BatchMode=yes', '-o', "ConnectTimeout=$ConnectTimeout", '-o', 'StrictHostKeyChecking=accept-new')

function Invoke-Remote([string] $Target, [string] $Script) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($Script))
    # Send remote output to the host, so the function returns only the exit code.
    & $ssh @sshOpts $Target "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand $encoded" 2>&1 |
        ForEach-Object { Write-Host "  $_" }
    return $LASTEXITCODE
}

# Step 1: make the folder, stop old instances so the exes can be overwritten.
$prepare = @"
`$ErrorActionPreference = 'SilentlyContinue'
New-Item -ItemType Directory -Force -Path '$Dest' | Out-Null
`$p = Get-Process keep-vm-resolution-bg, keep-vm-resolution
`$p | Stop-Process -Force
`$p | Wait-Process -Timeout 10
'KVR-Daemon', 'KVR-Resize', 'KVR-Test', 'KVR-Start' | ForEach-Object { Unregister-ScheduledTask -TaskName `$_ -Confirm:`$false }
exit 0
"@

# Steps 3-5: preset, autostart, start in the console session, verify.
$install = @"
`$ErrorActionPreference = 'Stop'
`$cli = '$Dest\keep-vm-resolution.exe'
& `$cli set $width $height
if (`$LASTEXITCODE) { Write-Output 'FAIL: set'; exit 11 }
& `$cli install --no-start
if (`$LASTEXITCODE) { Write-Output 'FAIL: install'; exit 12 }

`$action = New-ScheduledTaskAction -Execute '$Dest\keep-vm-resolution-bg.exe' -WorkingDirectory '$Dest'
`$principal = New-ScheduledTaskPrincipal -UserId '$User' -LogonType Interactive
Register-ScheduledTask -TaskName KVR-Start -Action `$action -Principal `$principal -Force | Out-Null
Start-ScheduledTask -TaskName KVR-Start
`$proc = `$null
for (`$i = 0; `$i -lt 20 -and -not `$proc; `$i++) {
    Start-Sleep -Milliseconds 500
    `$proc = Get-Process keep-vm-resolution-bg -ErrorAction SilentlyContinue | Select-Object -First 1
}
Unregister-ScheduledTask -TaskName KVR-Start -Confirm:`$false
if (-not `$proc) { Write-Output 'FAIL: not running (is $User logged on to the console?)'; exit 13 }
Write-Output ('RUNNING pid {0} session {1}' -f `$proc.Id, `$proc.SessionId)
Start-Sleep -Seconds 2
`$log = Join-Path `$env:LOCALAPPDATA 'KeepVMResolution\keep-vm-resolution.log'
if (Test-Path `$log) { Get-Content `$log -Tail 3 }
exit 0
"@

$results = foreach ($h in $Hosts) {
    $target = "$User@$h"
    Write-Host "=== $h" -ForegroundColor Cyan
    $status = 'OK'
    try {
        if ((Invoke-Remote $target $prepare) -ne 0) { throw 'ssh / prepare failed' }

        & $scp @sshOpts -q $cli $bg "${target}:$($Dest -replace '\\', '/')/"
        if ($LASTEXITCODE) { throw 'copy failed' }

        $rc = Invoke-Remote $target $install
        if ($rc -ne 0) { throw "install step failed (exit $rc)" }
    }
    catch {
        $status = "FAILED: $($_.Exception.Message)"
        Write-Host $status -ForegroundColor Red
    }
    [pscustomobject]@{ Host = $h; Status = $status }
}

Write-Host ''
$results | Format-Table -AutoSize
if ($results.Status -ne 'OK') { exit 1 }
