# SPDX-License-Identifier: GPL-3.0-only
# Explicit per-device administrator preparation. No global driver replacement.
[CmdletBinding()]
param(
    [ValidateSet('Prepare', 'Restore')][string]$Action = 'Prepare',
    [ValidatePattern('^usb-[0-9a-f]{16}$')][string]$DeviceId,
    [ValidatePattern('^winusb-[0-9A-F]{16}$')][string]$DeviceToken,
    [switch]$UseExistingFilter,
    [switch]$ResumeAfterReplug,
    [string]$StateDirectory,
    [string]$LegacyStateDirectory,
    [string]$ResultFile
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$portableUsb = Join-Path $workspace 'runtime/usb/bin'
if (-not $env:RUSTCARPLAY_LIBIMOBILEDEVICE_DIR -and
    (Test-Path -LiteralPath (Join-Path $portableUsb 'libimobiledevice-1.0.dll'))) {
    $env:RUSTCARPLAY_LIBIMOBILEDEVICE_DIR = $portableUsb
}
. (Join-Path $PSScriptRoot 'windows-usb-paths.ps1')
$state = Resolve-RustCarPlayUsbStateDirectory $StateDirectory
# Published archives contain prebuilt helpers; source checkouts retain the
# existing debug-example workflow. Driver preparation behavior is identical.
$helperDirectory = Join-Path $workspace 'tools'
if (-not (Test-Path -LiteralPath (Join-Path $helperDirectory 'usb_probe.exe'))) {
    $helperDirectory = Join-Path $workspace 'target/debug/examples'
}
$probe = Join-Path $helperDirectory 'usb_probe.exe'
$mode = Join-Path $helperDirectory 'usb_mode.exe'
$runtimeCheck = Join-Path $helperDirectory 'usb_runtime_check.exe'
$configScript = Join-Path $PSScriptRoot 'windows-usb-config.ps1'
$filterScript = Join-Path $PSScriptRoot 'windows-usb-filter.ps1'

function Set-PreparationStage($Progress, [string]$Stage) {
    $Progress.Stage = $Stage
    if ($ResultFile) {
        # Observability must not interrupt a registry transaction. The reader
        # tolerates a partial write and retries on its next poll.
        try { Write-RustCarPlayUtf8 ($ResultFile + '.progress') $Stage } catch { }
    }
}

function Save-PreparationResume([string]$Directory, [string]$Token, $Result) {
    if ($Token -cnotmatch '^winusb-[0-9A-F]{16}$') { return }
    $path = Join-Path $Directory ($Token + '.resume.json')
    if ($Result.ContainsKey('resume_after_replug') -and $Result.resume_after_replug) {
        Write-RustCarPlayUtf8 $path (ConvertTo-Json -Compress -InputObject @{
            resume_after_replug=$true; stage=$Result.stage
        })
    } elseif (Test-Path -LiteralPath $path) {
        Remove-Item -LiteralPath $path -Force
    }
}

function Read-SelectedPhone {
    $arguments = @(); if ($DeviceId) { $arguments += $DeviceId }
    $json = & $probe @arguments 2>$null
    if ($LASTEXITCODE -ne 0) { throw 'USB enumeration failed. Reconnect the selected iPhone.' }
    $phones = @(($json -join "`n") | ConvertFrom-RustCarPlayJson)
    if ($phones.Count -ne 1) { throw 'Connect exactly one iPhone or pass its USB device ID.' }
    $phones[0]
}

function Find-SelectedParent([string]$Token) {
    $matches = @(Get-PnpDevice -PresentOnly | Where-Object {
        if ($_.InstanceId -notmatch '^USB\\VID_05AC&PID_[0-9A-F]{4}\\[^\\]+$') { return $false }
        $digest = Get-RustCarPlaySha256Hex ([Text.Encoding]::UTF8.GetBytes($_.InstanceId.ToUpperInvariant()))
        ('winusb-' + $digest.Substring(0,16)) -ceq $Token
    })
    if ($matches.Count -ne 1) { throw 'The exact selected USB parent is no longer present.' }
    $matches[0]
}

function Restart-SelectedParent([string]$Token) {
    $parent = Find-SelectedParent $Token
    $service = Get-PnpDeviceProperty -InstanceId $parent.InstanceId -KeyName DEVPKEY_Device_Service
    if ($service.Data -ine 'usbccgp') { throw 'The selected parent driver changed; refusing to restart.' }
    $output = & "$env:SystemRoot/System32/pnputil.exe" /restart-device $parent.InstanceId 2>&1
    if ($LASTEXITCODE -ne 0) {
        $reason = ($output -join ' ').Replace($parent.InstanceId, '[selected iPhone]')
        if ($reason.Length -gt 700) { $reason = $reason.Substring(0,700) }
        throw ('The selected device could not restart (exit ' + $LASTEXITCODE + '): ' + $reason)
    }
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    do {
        Start-Sleep -Milliseconds 250
        try {
            $current = Find-SelectedParent $Token
            if ($current.Status -eq 'OK') { return }
        } catch { }
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'The selected USB parent did not become ready after restart.'
}

function Switch-CarPlay {
    $output = & $mode --switch-carplay $DeviceId 2>&1
    if ($LASTEXITCODE -ne 0) {
        # The Rust example returns sanitized protocol/driver errors only.
        throw ('CarPlay mode switch failed: ' + ($output -join ' '))
    }
}

function Read-PreparationPhone([string]$Token) {
    $selected = Read-SelectedPhone
    if ($selected.windows_device_token -cne $Token) {
        throw 'The physical iPhone changed during preparation. Select the original phone again.'
    }
    $selected
}

function Get-PreparationDescriptorReport([string]$Token, [string]$StateDirectory, [bool]$Resume, $Progress) {
    $report = Join-Path $StateDirectory ($Token + '.descriptors.json')
    if ($Resume) {
        Set-PreparationStage $Progress 'initial_mode_after_replug'
        if ($null -ne (Read-PreparationPhone $Token).configuration) {
            throw 'ResumeAfterReplug requires the initial USB mode. Unplug, wait five seconds, reconnect and unlock the same phone.'
        }
        # A cable replug can replace either failed software restart. Before the
        # first restart there is no descriptor report yet: continue discovery.
        # Only a fresh report can skip discovery and proceed directly to Arm,
        # whose existing validator also checks the report's exact phone token.
        if ((Test-Path -LiteralPath $report) -and
            (Get-Item -LiteralPath $report).LastWriteTimeUtc -ge [DateTime]::UtcNow.AddMinutes(-30)) {
            return $report
        }
    } else {
        Set-PreparationStage $Progress 'initial_restart'
        Restart-SelectedParent $Token
    }
    Set-PreparationStage $Progress 'descriptor_probe'
    Switch-CarPlay
    $observed = Read-PreparationPhone $Token
    if ($null -eq $observed.configuration) { throw 'iPhone did not expose a USBMUX + NCM CarPlay configuration.' }
    Write-RustCarPlayUtf8 $report (ConvertTo-Json -InputObject @($observed) -Depth 8)
    # Return to the initial mode before arming the descriptor-proven index. If
    # this restart also needs a cable replug, the next resume uses this report.
    Set-PreparationStage $Progress 'descriptor_reset'
    & $configScript -Action Disarm -DeviceToken $Token -StateDirectory $StateDirectory | Out-Null
    Restart-SelectedParent $Token
    $initialDeadline = [DateTime]::UtcNow.AddSeconds(3)
    do {
        if ($null -eq (Read-PreparationPhone $Token).configuration) { return $report }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $initialDeadline)
    # A healthy PnP status alone is insufficient. Without a real return to the
    # initial mode, usb_mode would consider mode 4 already active and no-op.
    throw 'Replug required: the selected phone did not return to its initial USB mode after the descriptor reset.'
}

function Test-PreparationCanResume([string]$Stage, [bool]$RequiresReplug, [bool]$HasRestoreRecord) {
    $RequiresReplug -and $HasRestoreRecord -and $Stage -in @(
        'filter_install', 'initial_restart', 'initial_mode_after_replug', 'descriptor_reset'
    )
}

# Loading functions for filesystem/mock regression tests must never prepare a
# device or request elevation.
if ($MyInvocation.InvocationName -eq '.') { return }

$token = $null
$prepared = $false
$armed = $false
$success = $false
$result = $null
$workflowMutex = $null
$workflowLocked = $false
$progress = @{ Stage = 'preflight' }
try {
    Set-PreparationStage $progress 'preflight'
    if (-not (Test-RustCarPlayWindows)) { throw 'Windows is required.' }
    $principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Run this preparation helper in an administrator PowerShell terminal.'
    }
    if ($Action -eq 'Restore' -and $DeviceToken) {
        # Restore must remain available when a failed driver/configuration left
        # the phone absent from nusb, or when the cable has been unplugged.
        $token = $DeviceToken
    } else {
        foreach ($file in @($probe, $mode, $runtimeCheck)) {
            if (-not (Test-Path -LiteralPath $file)) { throw 'Build the carplay-platform examples before USB preparation.' }
        }
        $phone = Read-SelectedPhone
        $DeviceId = $phone.id
        $token = $phone.windows_device_token
        if ($DeviceToken -and $DeviceToken -cne $token) { throw 'USB device ID and Windows token do not identify the same phone.' }
    }
    if ($token -cnotmatch '^winusb-[0-9A-F]{16}$') { throw 'The USB probe did not provide a Windows instance token.' }
    $workflowMutex = [Threading.Mutex]::new($false, ('Global\RustCarPlayUsb-' + $token))
    try { $workflowLocked = $workflowMutex.WaitOne(0) }
    catch [Threading.AbandonedMutexException] { $workflowLocked = $true }
    if (-not $workflowLocked) { throw 'Another USB preparation or restore is running for this phone.' }
    $configRecord = Join-Path $state ($token + '.json')
    $filterRecord = Join-Path $state ($token + '.filter.json')
    if ($LegacyStateDirectory) {
        Import-RustCarPlayUsbRestoreRecords -LegacyDirectory $LegacyStateDirectory -StateDirectory $state -DeviceToken $token
    } elseif (-not (Test-Path -LiteralPath $configRecord) -and -not (Test-Path -LiteralPath $filterRecord)) {
        # Source checkouts and old portable installs can move their own exact
        # records to the shared user data location without losing rollback data.
        Import-RustCarPlayUsbRestoreRecords -LegacyDirectory (Join-Path $workspace '.local/windows-usb') -StateDirectory $state -DeviceToken $token
    }
    if ($Action -eq 'Restore') {
        if (Test-Path -LiteralPath $configRecord) {
            & $configScript -Action Restore -DeviceToken $token -StateDirectory $state | Out-Null
        }
        if (Test-Path -LiteralPath $filterRecord) {
            & $filterScript -Action Restore -DeviceToken $token -StateDirectory $state | Out-Null
        } else {
            $present = $null
            try { $present = Find-SelectedParent $token } catch { }
            if ($present) { Restart-SelectedParent $token }
        }
        $result = @{ action='Restore'; success=$true; device_id=$DeviceId; original_device_configuration_restored=$true }
        $success = $true
    } else {
        # Keep an explicit override or the portable runtime selected above.
        # Source checkouts fall back to the locally prepared development files.
        if (-not $env:RUSTCARPLAY_LIBIMOBILEDEVICE_DIR) {
            $env:RUSTCARPLAY_LIBIMOBILEDEVICE_DIR = Join-Path $workspace '.local/usb-runtime/bin'
        }
        $runtime = & $runtimeCheck 2>$null
        if ($LASTEXITCODE -ne 0) { throw 'USB runtime or Apple local USBMUX service is unavailable; run setup-usb-runtime.ps1 and open iTunes/Apple Devices.' }
        if ($null -ne $phone.configuration -and $phone.active_configuration -eq $phone.configuration.configuration_value) {
            if (Test-Path -LiteralPath $configRecord) {
                $prepared = $true
                & $configScript -Action Disarm -DeviceToken $token -StateDirectory $state | Out-Null
            }
            $result = @{ action='Prepare'; success=$true; device_id=$DeviceId; already_configured=$true; ncm_connection_still_requires_handshake=$true }
            $success = $true
        } else {
            # Package verification occurs before persistent changes. Installation
            # touches only this exact parent and preserves the child Apple stack.
            Set-PreparationStage $progress 'driver_verify'
            $verified = & $filterScript -Action Verify -DeviceToken $token -UseExistingFilter:$UseExistingFilter -StateDirectory $state | ConvertFrom-Json
            $env:RUSTCARPLAY_USB_FILTER_DIR = $verified.runtime_directory
            $installNeeded = -not (Test-Path -LiteralPath $filterRecord)
            if (-not $installNeeded) {
                if ((Get-Item -LiteralPath $filterRecord).Length -gt 1048576) { throw 'The filter restore record is too large.' }
                $record = Get-Content -LiteralPath $filterRecord -Raw -Encoding utf8 | ConvertFrom-RustCarPlayJson
                $installNeeded = $record.ContainsKey('Detached') -and $record.Detached
                if (-not $installNeeded) {
                    $parent = Find-SelectedParent $token
                    $filters = Get-PnpDeviceProperty -InstanceId $parent.InstanceId -KeyName DEVPKEY_Device_UpperFilters -ErrorAction SilentlyContinue
                    if ($null -eq $filters -or $filters.Data -inotcontains 'libusb0') {
                        throw 'A partial filter installation record exists. Run Restore before retrying preparation.'
                    }
                }
            }
            if ($installNeeded) {
                Set-PreparationStage $progress 'filter_install'
                & $filterScript -Action Install -DeviceToken $token -UseExistingFilter:([bool]$verified.reuse_existing_filter) -StateDirectory $state | Out-Null
            }
            Set-PreparationStage $progress 'configuration_prepare'
            $prepared = $true
            & $configScript -Action Prepare -DeviceToken $token -DetachAppleLowerFilter -StateDirectory $state | Out-Null
            & $configScript -Action Disarm -DeviceToken $token -StateDirectory $state | Out-Null
            [IO.Directory]::CreateDirectory($state) | Out-Null
            $report = Get-PreparationDescriptorReport $token $state ([bool]$ResumeAfterReplug) $progress
            Set-PreparationStage $progress 'activate_configuration'
            $armed = $true
            try {
                & $configScript -Action Arm -DeviceToken $token -DescriptorReport $report -StateDirectory $state | Out-Null
                Switch-CarPlay
                $active = Read-PreparationPhone $token
                if ($null -eq $active.configuration -or $active.active_configuration -ne $active.configuration.configuration_value) {
                    throw 'Windows did not select the descriptor-proven CarPlay USB configuration.'
                }
                $result = @{ action='Prepare'; success=$true; device_id=$DeviceId; active_configuration=$active.active_configuration; ncm_connection_still_requires_handshake=$true }
                $success = $true
            } finally {
                # Preserve the active configuration, but make the next physical
                # attach safe even if the app crashes or the cable is removed.
                Set-PreparationStage $progress 'cleanup'
                & $configScript -Action Disarm -DeviceToken $token -StateDirectory $state | Out-Null
                $armed = $false
            }
        }
    }
} catch {
    $success = $false
    $result = @{ action=$Action; success=$false; stage=$progress.Stage; error=('USB preparation failed at ' + $progress.Stage + ': ' + $_.Exception.Message); restore_available=($null -ne $token) }
    $result.requires_replug = $_.Exception.Message -match '(?i)restart|reboot|replug'
    $result.resume_after_replug = $false
    if ($token) {
        $hasRecord = (Test-Path -LiteralPath (Join-Path $state ($token + '.json'))) -or
            (Test-Path -LiteralPath (Join-Path $state ($token + '.filter.json')))
        $result.resume_after_replug = Test-PreparationCanResume $progress.Stage $result.requires_replug $hasRecord
    }
    $safeToRestart = $false
    if ($prepared -and $token) {
        try {
            & $configScript -Action Disarm -DeviceToken $token -StateDirectory $state | Out-Null
            $safeToRestart = $true
        }
        catch {
            $result.cleanup_error = 'Cannot confirm the safe initial USB configuration: ' + $_.Exception.Message
            $result.requires_replug = $false
            $result.resume_after_replug = $false
        }
    }
    if ($prepared -and $token -and $safeToRestart -and -not $ResumeAfterReplug -and -not $result.requires_replug) {
        try { Restart-SelectedParent $token }
        catch { $result.restart_error = $_.Exception.Message }
    }
    if ($token) {
        $result.persistent_setup_may_remain = (Test-Path -LiteralPath (Join-Path $state ($token + '.json'))) -or (Test-Path -LiteralPath (Join-Path $state ($token + '.filter.json')))
        $result.restore_device_token = $token
    }
} finally {
    if ($workflowLocked -and $null -ne $result) {
        try { Save-PreparationResume $state $token $result }
        catch { $result.resume_save_error = 'Cannot save USB resume state; keep this window open until preparation finishes.' }
    }
    if ($workflowLocked) { $workflowMutex.ReleaseMutex() }
    if ($null -ne $workflowMutex) { $workflowMutex.Dispose() }
}
$json = $result | ConvertTo-Json -Depth 5
if ($ResultFile) { [IO.File]::WriteAllText([IO.Path]::GetFullPath($ResultFile), $json, [Text.UTF8Encoding]::new($false)) }
Write-Output $json
if (-not $success) { exit 1 }
