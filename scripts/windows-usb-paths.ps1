# SPDX-License-Identifier: GPL-3.0-only
# Filesystem helpers only. Loading this file never touches USB or the registry.
function Test-RustCarPlayWindows { [Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT }

function Get-RustCarPlaySha256Hex([byte[]]$Bytes) {
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try { ([BitConverter]::ToString($algorithm.ComputeHash($Bytes))).Replace('-', '') }
    finally { $algorithm.Dispose() }
}

function ConvertTo-RustCarPlayHashtable($Value) {
    if ($null -eq $Value) { return $null }
    if ($Value -is [Management.Automation.PSCustomObject]) {
        $map = @{}
        foreach ($property in $Value.PSObject.Properties) { $map[$property.Name] = ConvertTo-RustCarPlayHashtable $property.Value }
        return $map
    }
    if ($Value -is [Array]) {
        $items = @(); foreach ($item in $Value) { $items += ,(ConvertTo-RustCarPlayHashtable $item) }
        return ,$items
    }
    return $Value
}

function ConvertFrom-RustCarPlayJson {
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)][string]$InputObject)
    process {
        $converted = ConvertTo-RustCarPlayHashtable (ConvertFrom-Json -InputObject $InputObject -ErrorAction Stop)
        if ($converted -is [Array]) { foreach ($item in $converted) { Write-Output $item } }
        else { Write-Output $converted }
    }
}

function Write-RustCarPlayUtf8([string]$Path, [string]$Text) {
    [IO.File]::WriteAllText($Path, $Text, [Text.UTF8Encoding]::new($false))
}

function Resolve-RustCarPlayUsbStateDirectory([string]$Explicit) {
    $value = $Explicit
    if (-not $value) { $value = $env:RUSTCARPLAY_USB_STATE_DIR }
    if (-not $value) {
        if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA or an explicit USB state directory is required.' }
        $value = Join-Path $env:LOCALAPPDATA 'RustCarPlay/.local/windows-usb'
    }
    $root = [IO.Path]::GetPathRoot($value)
    if (-not [IO.Path]::IsPathRooted($value) -or ((Test-RustCarPlayWindows) -and
        ($root -eq '\' -or $root -eq '/' -or $root -match '^[A-Za-z]:$'))) {
        throw 'The USB state directory must be an absolute path.'
    }
    [IO.Path]::GetFullPath($value)
}

function Import-RustCarPlayUsbRestoreRecords {
    param(
        [Parameter(Mandatory)][string]$LegacyDirectory,
        [Parameter(Mandatory)][string]$StateDirectory,
        [Parameter(Mandatory)][ValidatePattern('^winusb-[0-9A-F]{16}$')][string]$DeviceToken
    )
    $sourceDirectory = Resolve-RustCarPlayUsbStateDirectory $LegacyDirectory
    $destinationDirectory = Resolve-RustCarPlayUsbStateDirectory $StateDirectory
    if ($sourceDirectory -ieq $destinationDirectory -or -not [IO.Directory]::Exists($sourceDirectory)) { return }
    # Dot-source in this function's scope to reuse the complete existing record
    # validators. Neither script invokes its hardware actions when dot-sourced.
    . (Join-Path $PSScriptRoot 'windows-usb-config.ps1') -StateDirectory $destinationDirectory -DeviceToken $DeviceToken
    . (Join-Path $PSScriptRoot 'windows-usb-filter.ps1') -StateDirectory $destinationDirectory -DeviceToken $DeviceToken
    $copies = @()
    foreach ($suffix in @('.json', '.filter.json')) {
        $source = Join-Path $sourceDirectory ($DeviceToken + $suffix)
        if (-not [IO.File]::Exists($source)) { continue }
        if ((Get-Item -LiteralPath $source).Length -gt 1048576) { throw 'A legacy USB restore record is too large.' }
        $record = Get-Content -LiteralPath $source -Raw -Encoding utf8 | ConvertFrom-RustCarPlayJson
        if ($suffix -eq '.json') { Assert-UsbBackup $record $DeviceToken }
        else { Assert-UsbFilterBackup $record $DeviceToken }
        $destination = Join-Path $destinationDirectory ($DeviceToken + $suffix)
        if ([IO.File]::Exists($destination)) {
            if ((Get-FileHash -LiteralPath $source).Hash -cne (Get-FileHash -LiteralPath $destination).Hash) {
                throw 'A different USB restore record already exists in the user data directory; no records were replaced.'
            }
        } else { $copies += @{ Source=$source; Destination=$destination } }
    }
    if ($copies.Count) {
        [IO.Directory]::CreateDirectory($destinationDirectory) | Out-Null
        foreach ($copy in $copies) { [IO.File]::Copy($copy.Source, $copy.Destination, $false) }
    }
    # Preserve every original record and all descriptor reports. Only exact
    # device records validated above are copied; no phone identifiers are logged.
}
