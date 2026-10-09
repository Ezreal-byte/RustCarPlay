# SPDX-License-Identifier: GPL-3.0-only
# Explicit per-device administrator preparation. No global driver replacement.
[CmdletBinding()]
param(
    [ValidateSet('Prepare', 'Restore')][string]$Action = 'Prepare',
    [ValidatePattern('^usb-[0-9a-f]{16}$')][string]$DeviceId,
    [ValidatePattern('^winusb-[0-9A-F]{16}$')][string]$DeviceToken,
    [switch]$UseExistingFilter,
    [switch]$ResumeAfterReplug,
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
$state = Join-Path $workspace '.local/windows-usb'
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

function Read-SelectedPhone {
    $arguments = @(); if ($DeviceId) { $arguments += $DeviceId }
    $json = & $probe @arguments 2>$null
    if ($LASTEXITCODE -ne 0) { throw 'USB enumeration failed. Reconnect the selected iPhone.' }
    $phones = @($json | ConvertFrom-Json)
    if ($phones.Count -ne 1) { throw 'Connect exactly one iPhone or pass its USB device ID.' }
    $phones[0]
}

function Find-SelectedParent([string]$Token) {
    $matches = @(Get-PnpDevice -PresentOnly | Where-Object {
        if ($_.InstanceId -notmatch '^USB\\VID_05AC&PID_[0-9A-F]{4}\\[^\\]+$') { return $false }
        $digest = [Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($_.InstanceId.ToUpperInvariant()))
        ('winusb-' + [Convert]::ToHexString($digest).Substring(0,16)) -ceq $Token
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

$token = $null
$prepared = $false
$armed = $false
$success = $false
$result = $null
$workflowMutex = $null
$workflowLocked = $false
try {
    if (-not $IsWindows) { throw 'Windows is required.' }
    $principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Run this preparation helper in administrator PowerShell 7.'
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
    if ($Action -eq 'Restore') {
        if (Test-Path -LiteralPath $configRecord) {
            & $configScript -Action Restore -DeviceToken $token | Out-Null
        }
        if (Test-Path -LiteralPath $filterRecord) {
            & $filterScript -Action Restore -DeviceToken $token | Out-Null
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
                & $configScript -Action Disarm -DeviceToken $token | Out-Null
            }
            $result = @{ action='Prepare'; success=$true; device_id=$DeviceId; already_configured=$true; ncm_connection_still_requires_handshake=$true }
            $success = $true
        } else {
            # Package verification occurs before persistent changes. Installation
            # touches only this exact parent and preserves the child Apple stack.
            $verified = & $filterScript -Action Verify -UseExistingFilter:$UseExistingFilter | ConvertFrom-Json
            $env:RUSTCARPLAY_USB_FILTER_DIR = $verified.runtime_directory
            $installNeeded = -not (Test-Path -LiteralPath $filterRecord)
            if (-not $installNeeded) {
                if ((Get-Item -LiteralPath $filterRecord).Length -gt 1048576) { throw 'The filter restore record is too large.' }
                $record = Get-Content -LiteralPath $filterRecord -Raw -Encoding utf8 | ConvertFrom-Json -AsHashtable
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
                & $filterScript -Action Install -DeviceToken $token -UseExistingFilter:$UseExistingFilter | Out-Null
            }
            $prepared = $true
            & $configScript -Action Prepare -DeviceToken $token -DetachAppleLowerFilter | Out-Null
            & $configScript -Action Disarm -DeviceToken $token | Out-Null
            [IO.Directory]::CreateDirectory($state) | Out-Null
            $report = Join-Path $state ($token + '.descriptors.json')
            if ($ResumeAfterReplug) {
                # Some Windows stacks refuse a PnP restart with ERROR_REBOOT_REQUIRED.
                # A deliberate cable replug can still restore the initial mode.
                # Accept only a fresh descriptor report from this same phone;
                # Arm validates its token and the result is rechecked below.
                if ($null -ne (Read-SelectedPhone).configuration) {
                    throw 'ResumeAfterReplug requires the initial USB mode. Unplug, wait five seconds, reconnect and unlock the same phone.'
                }
                if (-not (Test-Path -LiteralPath $report) -or
                    (Get-Item -LiteralPath $report).LastWriteTimeUtc -lt [DateTime]::UtcNow.AddMinutes(-30)) {
                    throw 'A fresh descriptor report from this phone is required before resuming after a cable replug.'
                }
            } else {
                Restart-SelectedParent $token
                Switch-CarPlay
                $observed = Read-SelectedPhone
                if ($null -eq $observed.configuration) { throw 'iPhone did not expose a USBMUX + NCM CarPlay configuration.' }
                [IO.File]::WriteAllText($report, (ConvertTo-Json -InputObject @($observed) -Depth 8), [Text.UTF8Encoding]::new($false))
                # A PnP restart resets iPhone to its initial mode. Keep its initial
                # configuration valid until the subsequent explicit mode request.
                & $configScript -Action Disarm -DeviceToken $token | Out-Null
                Restart-SelectedParent $token
            }
            $armed = $true
            try {
                & $configScript -Action Arm -DeviceToken $token -DescriptorReport $report | Out-Null
                Switch-CarPlay
                $active = Read-SelectedPhone
                if ($null -eq $active.configuration -or $active.active_configuration -ne $active.configuration.configuration_value) {
                    throw 'Windows did not select the descriptor-proven CarPlay USB configuration.'
                }
                $result = @{ action='Prepare'; success=$true; device_id=$DeviceId; active_configuration=$active.active_configuration; ncm_connection_still_requires_handshake=$true }
                $success = $true
            } finally {
                # Preserve the active configuration, but make the next physical
                # attach safe even if the app crashes or the cable is removed.
                & $configScript -Action Disarm -DeviceToken $token | Out-Null
                $armed = $false
            }
        }
    }
} catch {
    $success = $false
    $result = @{ action=$Action; success=$false; error=$_.Exception.Message; restore_available=($null -ne $token) }
    $safeToRestart = $false
    if ($prepared -and $token) {
        try {
            & $configScript -Action Disarm -DeviceToken $token | Out-Null
            $safeToRestart = $true
        }
        catch { $result.cleanup_error = $_.Exception.Message }
    }
    if ($prepared -and $token -and $safeToRestart -and -not $ResumeAfterReplug) {
        try { Restart-SelectedParent $token }
        catch { $result.restart_error = $_.Exception.Message }
    }
    if ($token) {
        $result.persistent_setup_may_remain = (Test-Path -LiteralPath (Join-Path $state ($token + '.json'))) -or (Test-Path -LiteralPath (Join-Path $state ($token + '.filter.json')))
        $result.restore_device_token = $token
    }
} finally {
    if ($workflowLocked) { $workflowMutex.ReleaseMutex() }
    if ($null -ne $workflowMutex) { $workflowMutex.Dispose() }
}
$json = $result | ConvertTo-Json -Depth 5
if ($ResultFile) { [IO.File]::WriteAllText([IO.Path]::GetFullPath($ResultFile), $json, [Text.UTF8Encoding]::new($false)) }
Write-Output $json
if (-not $success) { exit 1 }
