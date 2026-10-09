# SPDX-License-Identifier: GPL-3.0-only
# Filesystem fixtures only: no device, driver, registry or elevation operations.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-usb-config.ps1')
function Assert([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Assert-Throws([scriptblock]$Action, [string]$Message) {
    $thrown = $false; try { & $Action } catch { $thrown = $true }; Assert $thrown $Message
}
$root = Join-Path ([IO.Path]::GetTempPath()) ('rustcarplay-usb-paths-' + [Guid]::NewGuid().ToString('N'))
$legacy = Join-Path $root 'old development records'
$current = Join-Path $root 'user data records'
$beforeEnvironment = $env:RUSTCARPLAY_USB_STATE_DIR
try {
    [IO.Directory]::CreateDirectory($legacy) | Out-Null
    $env:RUSTCARPLAY_USB_STATE_DIR = $current
    Assert ((Resolve-RustCarPlayUsbStateDirectory) -eq $current) 'The launcher state directory must be used by scripts.'
    Assert ((Resolve-RustCarPlayUsbStateDirectory $legacy) -eq $legacy) 'An explicit UAC argument must override inherited environment.'
    Assert-Throws { Resolve-RustCarPlayUsbStateDirectory 'relative/state' } 'Relative state paths must be rejected.'
    $env:RUSTCARPLAY_USB_STATE_DIR = $null
    Assert ((Resolve-RustCarPlayUsbStateDirectory) -eq (Join-Path $env:LOCALAPPDATA 'RustCarPlay/.local/windows-usb')) 'Development and installed helpers must share one user state directory.'

    $fakeInstance = 'USB\VID_05AC&PID_12A8\UNITTEST'
    $token = Get-UsbToken $fakeInstance
    $absent = @{Present=$false;Kind=$null;Value=$null}
    $config = @{Version=1;Instance=$fakeInstance;Driver='{88bae032-5a81-49f0-bc3d-a4ff138216d6}\0002';Entries=@(
        foreach ($entry in Get-UsbEntries) { @{Scope=$entry.Scope;Name=$entry.Name;Before=$absent;After=$absent;Previous=$absent} }
    )}
    $filter = @{Version=1;Instance=$fakeInstance;Before=$absent;After=@{Present=$true;Kind='MultiString';Value=@('libusb0')};SurpriseBefore=$absent;SysHash=('A' * 64)}
    $configFile = Join-Path $legacy ($token + '.json')
    $filterFile = Join-Path $legacy ($token + '.filter.json')
    Save-UsbBackup $configFile $config $true
    Save-UsbBackup $configFile $config $false
    Assert ((Get-Content -LiteralPath $configFile -Raw -Encoding utf8 | ConvertFrom-RustCarPlayJson).Version -eq 1) 'Creating and atomically replacing recovery records must work on .NET Framework.'
    $filter.Instance = 'USB\VID_05AC&PID_12A8\OTHERPHONE'
    Write-RustCarPlayUtf8 $filterFile ($filter | ConvertTo-Json -Depth 10)
    Assert-Throws { Import-RustCarPlayUsbRestoreRecords -LegacyDirectory $legacy -StateDirectory $current -DeviceToken $token } 'Records from another physical phone must be rejected.'
    Assert (-not [IO.Directory]::Exists($current)) 'All legacy records must validate before any destination is created.'
    $filter.Instance = $fakeInstance
    Write-RustCarPlayUtf8 $filterFile ($filter | ConvertTo-Json -Depth 10)
    Import-RustCarPlayUsbRestoreRecords -LegacyDirectory $legacy -StateDirectory $current -DeviceToken $token
    foreach ($suffix in @('.json', '.filter.json')) {
        $source = Join-Path $legacy ($token + $suffix)
        $target = Join-Path $current ($token + $suffix)
        Assert ([IO.File]::Exists($source)) 'Original rollback records must be preserved.'
        Assert ((Get-FileHash -LiteralPath $source).Hash -eq (Get-FileHash -LiteralPath $target).Hash) 'Migration must preserve exact original bytes.'
    }
    Import-RustCarPlayUsbRestoreRecords -LegacyDirectory $legacy -StateDirectory $current -DeviceToken $token
    $destination = Join-Path $current ($token + '.json')
    [IO.File]::AppendAllText($destination, ' ')
    $hash = (Get-FileHash -LiteralPath $destination).Hash
    Assert-Throws { Import-RustCarPlayUsbRestoreRecords -LegacyDirectory $legacy -StateDirectory $current -DeviceToken $token } 'An existing different recovery record must never be overwritten.'
    Assert ((Get-FileHash -LiteralPath $destination).Hash -eq $hash) 'A conflicting destination must remain unchanged.'
} finally {
    $env:RUSTCARPLAY_USB_STATE_DIR = $beforeEnvironment
    # This unique temporary tree contains only fixtures created above.
    $resolved = [IO.Path]::GetFullPath($root)
    $temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $resolved.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($resolved) -notlike 'rustcarplay-usb-paths-*') { throw 'Unexpected fixture cleanup directory.' }
    if (Test-Path -LiteralPath $resolved) { Remove-Item -LiteralPath $resolved -Recurse -Force }
}
Write-Output 'Windows USB state/legacy migration checks passed (filesystem fixtures only).'
