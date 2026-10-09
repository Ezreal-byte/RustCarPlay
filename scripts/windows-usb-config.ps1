# SPDX-License-Identifier: GPL-3.0-only
# Windows PowerShell 5.1 / PowerShell 7. Inspect is read-only.
[CmdletBinding()]
param(
    [ValidateSet('Inspect', 'Prepare', 'Arm', 'Disarm', 'Restore')]
    [string]$Action = 'Inspect',
    [string]$DeviceToken,
    [string]$DescriptorReport,
    [switch]$DetachAppleLowerFilter,
    [string]$StateDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-usb-paths.ps1')
$StateDirectory = Resolve-RustCarPlayUsbStateDirectory $StateDirectory

function Get-UsbToken([string]$Instance) {
    $digest = Get-RustCarPlaySha256Hex ([Text.Encoding]::UTF8.GetBytes($Instance.ToUpperInvariant()))
    'winusb-' + $digest.Substring(0, 16)
}

function Assert-UsbInstance([string]$Instance) {
    if ($Instance -notmatch '^USB\\VID_05AC&PID_[0-9A-F]{4}\\[^\\/:*?"<>|]+$') {
        throw 'The saved device is not an Apple USB composite parent.'
    }
}

function Get-UsbParents {
    @(Get-PnpDevice -PresentOnly | Where-Object {
        $_.InstanceId -match '^USB\\VID_05AC&PID_[0-9A-F]{4}\\'
    })
}

function Get-UsbPaths([string]$Instance) {
    Assert-UsbInstance $Instance
    $relative = 'SYSTEM\CurrentControlSet\Enum\' + $Instance
    $key = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey($relative)
    if ($null -eq $key) { throw 'The selected USB device registry key no longer exists.' }
    try {
        if ($key.GetValue('Service') -ine 'usbccgp') {
            throw 'The selected USB parent is not managed by usbccgp; no changes were made.'
        }
        $driver = [string]$key.GetValue('Driver')
        if ($driver -notmatch '^\{[0-9a-f-]{36}\}\\[0-9]{4}$') {
            throw 'The USB parent software key is missing or malformed.'
        }
    } finally { $key.Dispose() }
    @{
        Parent = $relative
        Parameters = $relative + '\Device Parameters'
        Software = 'SYSTEM\CurrentControlSet\Control\Class\' + $driver
        Driver = $driver
    }
}

function Get-UsbRegistryState([string]$Path, [string]$Name) {
    $key = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey($Path)
    if ($null -eq $key) { throw 'An expected USB registry key is missing.' }
    try {
        if ($key.GetValueNames() -inotcontains $Name) {
            return @{ Present = $false; Kind = $null; Value = $null }
        }
        @{ Present = $true; Kind = $key.GetValueKind($Name).ToString(); Value = $key.GetValue($Name) }
    } finally { $key.Dispose() }
}

function Test-UsbRegistryState($Left, $Right) {
    if ([bool]$Left.Present -ne [bool]$Right.Present) { return $false }
    if (-not $Left.Present) { return $true }
    if ($Left.Kind -cne $Right.Kind) { return $false }
    $a = @($Left.Value); $b = @($Right.Value)
    if ($a.Count -ne $b.Count) { return $false }
    for ($i = 0; $i -lt $a.Count; $i++) {
        if ([string]$a[$i] -cne [string]$b[$i]) { return $false }
    }
    return $true
}

function Set-UsbRegistryState([string]$Path, [string]$Name, $State) {
    $key = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey($Path, $true)
    if ($null -eq $key) { throw 'The USB registry key cannot be opened for writing.' }
    try {
        if (-not $State.Present) { $key.DeleteValue($Name, $false); return }
        $kind = [Microsoft.Win32.RegistryValueKind]::Parse([Microsoft.Win32.RegistryValueKind], $State.Kind)
        $value = $null
        switch ($kind) {
            DWord { $value = [int]$State.Value }
            Binary { $value = [byte[]]$State.Value }
            MultiString { $value = [string[]]$State.Value }
            default { throw 'Unexpected registry value type; refusing to write.' }
        }
        $key.SetValue($Name, $value, $kind)
        $key.Flush()
    } finally { $key.Dispose() }
}

function Get-UsbFilterState($Current) {
    if (-not $Current.Present) { return $Current }
    if ($Current.Kind -ne 'MultiString') { throw 'LowerFilters is not REG_MULTI_SZ.' }
    $filters = @($Current.Value | Where-Object { $_ -ine 'AppleLowerFilter' })
    if ($filters.Count -eq 0) { return @{ Present = $false; Kind = $null; Value = $null } }
    @{ Present = $true; Kind = 'MultiString'; Value = $filters }
}

function Get-UsbEntries {
    @(
        @{ Scope = 'Parameters'; Name = 'OriginalConfigurationValue' },
        @{ Scope = 'Parameters'; Name = 'AltConfigurationValue' },
        @{ Scope = 'Parent'; Name = 'LowerFilters' },
        @{ Scope = 'Software'; Name = 'EnumeratorClass' }
    )
}

function Assert-UsbBackup($Backup, [string]$Token) {
    Assert-UsbInstance $Backup.Instance
    if ($Backup.Version -ne 1 -or (Get-UsbToken $Backup.Instance) -cne $Token) {
        throw 'USB restore record identity or version does not match.'
    }
    if ($Backup.Driver -notmatch '^\{[0-9a-f-]{36}\}\\[0-9]{4}$') { throw 'Invalid saved software key.' }
    $expected = @(Get-UsbEntries)
    if (@($Backup.Entries).Count -ne $expected.Count) { throw 'Incomplete USB restore record.' }
    for ($i = 0; $i -lt $expected.Count; $i++) {
        $entry = $Backup.Entries[$i]
        if ($entry.Scope -cne $expected[$i].Scope -or $entry.Name -cne $expected[$i].Name) {
            throw 'Unexpected registry target in USB restore record.'
        }
        foreach ($state in @($entry.Before, $entry.After, $entry.Previous)) {
            if (-not $state.Present) { continue }
            $kind = switch ($entry.Name) {
                LowerFilters { 'MultiString' }
                EnumeratorClass { 'Binary' }
                default { 'DWord' }
            }
            if ($state.Kind -cne $kind) { throw 'Unexpected registry type in USB restore record.' }
        }
    }
}

function Save-UsbBackup([string]$Path, $Backup, [bool]$Create) {
    $bytes = [Text.Encoding]::UTF8.GetBytes(($Backup | ConvertTo-Json -Depth 10))
    if ($Create) {
        $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try { $stream.Write($bytes, 0, $bytes.Length); $stream.Flush($true) } finally { $stream.Dispose() }
    } else {
        $temp = $Path + '.' + [Guid]::NewGuid().ToString('N') + '.tmp'
        [IO.File]::WriteAllBytes($temp, $bytes)
        try { [IO.File]::Replace($temp, $Path, [NullString]::Value) }
        finally { if ([IO.File]::Exists($temp)) { [IO.File]::Delete($temp) } }
    }
}

function Set-UsbTransaction([string]$Path, $Backup, $Paths, [object[]]$Desired) {
    # Validate every target first. A crash midway leaves a durable record that
    # accepts either the original value or this transaction's intended value.
    for ($i = 0; $i -lt $Backup.Entries.Count; $i++) {
        $entry = $Backup.Entries[$i]
        $current = Get-UsbRegistryState $Paths[$entry.Scope] $entry.Name
        if (-not (Test-UsbRegistryState $current $entry.After) -and
            -not (Test-UsbRegistryState $current $entry.Before) -and
            -not (Test-UsbRegistryState $current $entry.Previous)) {
            throw 'A USB setting was changed by another program; refusing to overwrite it.'
        }
    }
    for ($i = 0; $i -lt $Backup.Entries.Count; $i++) {
        $entry = $Backup.Entries[$i]
        $entry.Previous = Get-UsbRegistryState $Paths[$entry.Scope] $entry.Name
        $entry.After = $Desired[$i]
    }
    Save-UsbBackup $Path $Backup $false
    for ($i = 0; $i -lt $Backup.Entries.Count; $i++) {
        $entry = $Backup.Entries[$i]
        $current = Get-UsbRegistryState $Paths[$entry.Scope] $entry.Name
        if (-not (Test-UsbRegistryState $current $entry.After)) {
            Set-UsbRegistryState $Paths[$entry.Scope] $entry.Name $entry.After
        }
    }
}

function Get-UsbDescriptorTarget([string]$Path, [int]$ProductId, [string]$Token) {
    if (-not $Path -or (Get-Item -LiteralPath $Path).Length -gt 1048576) {
        throw 'Arm requires a small JSON descriptor report produced by usb_probe.'
    }
    $phones = @(Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-RustCarPlayJson)
    if ($phones.Count -ne 1 -or [int]$phones[0].product_id -ne $ProductId -or
        -not $phones[0].Contains('windows_device_token') -or $phones[0].windows_device_token -cne $Token) {
        throw 'The descriptor report must contain exactly the selected USB iPhone.'
    }
    $config = $phones[0].configuration
    if ($null -eq $config -or -not $config.Contains('configuration_index') -or
        -not $config.Contains('configuration_value')) {
        throw 'The report does not contain a parsed USBMUX + NCM CarPlay configuration.'
    }
    $index = [int]$config.configuration_index
    $value = [int]$config.configuration_value
    if ($index -lt 0 -or $index -gt 254 -or $value -lt 1 -or $value -gt 255) {
        throw 'Invalid descriptor index/value.'
    }
    @{ Index = $index; Value = $value }
}

function Invoke-UsbConfig {
    if (-not (Test-RustCarPlayWindows)) { throw 'This script requires Windows.' }
    $parents = @(Get-UsbParents)
    if ($Action -eq 'Inspect') {
        $rows = foreach ($parent in $parents) {
            $paths = Get-UsbPaths $parent.InstanceId
            $original = Get-UsbRegistryState $paths.Parameters 'OriginalConfigurationValue'
            $filters = Get-UsbRegistryState $paths.Parent 'LowerFilters'
            $enum = Get-UsbRegistryState $paths.Software 'EnumeratorClass'
            [pscustomobject]@{
                device_token = Get-UsbToken $parent.InstanceId
                product_id = [Convert]::ToInt32(($parent.InstanceId -split 'PID_')[1].Substring(0, 4), 16)
                status = $parent.Status
                service = 'usbccgp'
                configuration_index = $(if ($original.Present) { $original.Value } else { 0 })
                apple_lower_filter = $filters.Present -and ($filters.Value -icontains 'AppleLowerFilter')
                cdc_enumerator = $enum.Present -and (Test-UsbRegistryState $enum @{ Present=$true; Kind='Binary'; Value=@(2,0,0) })
            }
        }
        ConvertTo-Json -InputObject @($rows) -Depth 4
        return
    }
    $principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'This action requires an administrator PowerShell terminal. Inspect is available without elevation.'
    }
    if (-not $DeviceToken) {
        if ($parents.Count -ne 1) { throw 'Select exactly one device_token returned by Inspect.' }
        $DeviceToken = Get-UsbToken $parents[0].InstanceId
    }
    if ($DeviceToken -cnotmatch '^winusb-[0-9A-F]{16}$') { throw 'Invalid device_token.' }
    $directory = [IO.Path]::GetFullPath($StateDirectory)
    $path = Join-Path $directory ($DeviceToken + '.json')
    $mutex = [Threading.Mutex]::new($false, ('Global\RustCarPlayUsb-' + $DeviceToken))
    $locked = $false
    try {
        try { $locked = $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $locked = $true }
        if (-not $locked) { throw 'Another USB configuration transaction is running for this device.' }
        if ([IO.File]::Exists($path)) {
            if ((Get-Item -LiteralPath $path).Length -gt 1048576) { throw 'USB restore record is too large.' }
            $backup = Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-RustCarPlayJson
            Assert-UsbBackup $backup $DeviceToken
            $paths = Get-UsbPaths $backup.Instance
            if ($paths.Driver -cne $backup.Driver) { throw 'The USB parent driver instance changed; manual restore review is required.' }
        } else {
            if ($Action -ne 'Prepare') { throw 'Prepare must save a restore record before this action.' }
            $selected = @($parents | Where-Object { (Get-UsbToken $_.InstanceId) -ceq $DeviceToken })
            if ($selected.Count -ne 1) { throw 'The selected USB parent is not present.' }
            $instance = $selected[0].InstanceId
            $paths = Get-UsbPaths $instance
            $entries = @(foreach ($entry in Get-UsbEntries) {
                $before = Get-UsbRegistryState $paths[$entry.Scope] $entry.Name
                @{ Scope = $entry.Scope; Name = $entry.Name; Before = $before; After = $before; Previous = $before }
            })
            $backup = @{ Version = 1; Instance = $instance; Driver = $paths.Driver; Entries = $entries }
            Assert-UsbBackup $backup $DeviceToken
            [IO.Directory]::CreateDirectory($directory) | Out-Null
            Save-UsbBackup $path $backup $true
        }
        $desired = @($backup.Entries | ForEach-Object { $_.After })
        switch ($Action) {
            Prepare {
                $desired[3] = @{ Present=$true; Kind='Binary'; Value=@(2,0,0) }
                if ($DetachAppleLowerFilter) { $desired[2] = Get-UsbFilterState $backup.Entries[2].Before }
            }
            Arm {
                $selected = @($parents | Where-Object { $_.InstanceId -ieq $backup.Instance })
                if ($selected.Count -ne 1) { throw 'The selected USB parent must be present before arming.' }
                $pidValue = [Convert]::ToInt32(($backup.Instance -split 'PID_')[1].Substring(0,4), 16)
                $target = Get-UsbDescriptorTarget $DescriptorReport $pidValue $DeviceToken
                $desired[0] = @{ Present=$true; Kind='DWord'; Value=$target.Index }
                $fallback = $(if ($backup.Entries[0].Before.Present) { [int]$backup.Entries[0].Before.Value } else { 0 })
                $desired[1] = @{ Present=$true; Kind='DWord'; Value=$fallback }
            }
            Disarm { $desired[0] = $backup.Entries[0].Before; $desired[1] = $backup.Entries[1].Before }
            Restore { $desired = @($backup.Entries | ForEach-Object { $_.Before }) }
        }
        Set-UsbTransaction $path $backup $paths $desired
        # No device identifiers, saved registry paths or serial numbers are printed.
        @{ action=$Action; device_token=$DeviceToken; registry_written=$true; device_restarted=$false; driver_installed=$false } | ConvertTo-Json -Compress
        if ($Action -eq 'Restore') {
            [IO.File]::Move($path, ($path + '.restored-' + [DateTime]::UtcNow.ToString('yyyyMMddHHmmss') + '.json'))
        }
    } finally {
        if ($locked) { $mutex.ReleaseMutex() }
        $mutex.Dispose()
    }
}

# Dot-source in the isolated regression test to use mocked registry functions.
if ($MyInvocation.InvocationName -ne '.') { Invoke-UsbConfig }
