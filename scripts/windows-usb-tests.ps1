# SPDX-License-Identifier: GPL-3.0-only
# Pure/mock regression tests. This script never reads or writes device registry.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-usb-config.ps1')
function Assert([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Assert-Throws([scriptblock]$Action, [string]$Message) {
    $thrown = $false; try { & $Action } catch { $thrown = $true }; Assert $thrown $Message
}
$absent = @{Present=$false;Kind=$null;Value=$null}
$original = @{Present=$true;Kind='DWord';Value=2}
$filters = @{Present=$true;Kind='MultiString';Value=@('KeepBefore','AppleLowerFilter','KeepAfter')}
$removed = Get-UsbFilterState $filters
Assert (Test-UsbRegistryState $removed @{Present=$true;Kind='MultiString';Value=@('KeepBefore','KeepAfter')}) 'Unrelated lower filters must be preserved in order.'
Assert (-not (Get-UsbFilterState @{Present=$true;Kind='MultiString';Value=@('AppleLowerFilter')}).Present) 'Only-filter removal must restore absent state.'
Assert (-not (Test-UsbRegistryState $absent @{Present=$true;Kind='DWord';Value=0})) 'Absent and zero are different.'
Assert (-not (Test-UsbRegistryState @{Present=$true;Kind='Binary';Value=@(2,0)} @{Present=$true;Kind='Binary';Value=@(2,0,0)})) 'Binary values require exact length.'
$fakeInstance = 'USB\VID_05AC&PID_12A8\UNITTEST'
$token = Get-UsbToken $fakeInstance
Assert ($token -cmatch '^winusb-[0-9A-F]{16}$') 'Token format must be stable and contain no device identity.'
Assert ($token -ceq (Get-UsbToken $fakeInstance.ToLowerInvariant())) 'Instance selection is case insensitive.'
Assert-Throws { Assert-UsbInstance 'USB\VID_05AC&PID_12A8&MI_01\UNITTEST' } 'Child interfaces must not be configuration targets.'
Assert-Throws { Assert-UsbInstance 'USB\VID_05AC&PID_12A8\UNITTEST\Other' } 'Nested registry targets must be rejected.'

$script:registry = @{
    'd/OriginalConfigurationValue' = $original
    'd/AltConfigurationValue' = $absent
    'p/LowerFilters' = $filters
    's/EnumeratorClass' = $absent
}
$paths = @{Parent='p';Parameters='d';Software='s'}
$script:writes = 0
$script:failAt = -1
function Get-UsbRegistryState([string]$Path,[string]$Name) { $script:registry["$Path/$Name"] }
function Set-UsbRegistryState([string]$Path,[string]$Name,$State) {
    $script:writes++
    if ($script:writes -eq $script:failAt) { throw 'Simulated partial-write interruption' }
    $script:registry["$Path/$Name"] = $State
}
function Save-UsbBackup([string]$Path,$Backup,[bool]$Create) {
    $script:durable = $Backup | ConvertTo-Json -Depth 10 | ConvertFrom-Json -AsHashtable
}
$entries = @(foreach ($entry in Get-UsbEntries) {
    $before = Get-UsbRegistryState $paths[$entry.Scope] $entry.Name
    @{Scope=$entry.Scope;Name=$entry.Name;Before=$before;After=$before;Previous=$before}
})
$backup = @{Version=1;Instance=$fakeInstance;Driver='{88bae032-5a81-49f0-bc3d-a4ff138216d6}\0002';Entries=$entries}
Assert-UsbBackup $backup $token
$desired = @($entries | ForEach-Object {$_.After})
$desired[2] = $removed
$desired[3] = @{Present=$true;Kind='Binary';Value=@(2,0,0)}
$script:failAt = 2
Assert-Throws { Set-UsbTransaction 'mock' $backup $paths $desired } 'A write interruption must be surfaced.'
$backup = $script:durable
$script:failAt = -1
Set-UsbTransaction 'mock' $backup $paths @($backup.Entries | ForEach-Object {$_.Before})
Assert (Test-UsbRegistryState $script:registry['p/LowerFilters'] $filters) 'Restore must recover partial filter writes.'
Assert (Test-UsbRegistryState $script:registry['s/EnumeratorClass'] $absent) 'Restore must preserve an originally absent binary.'

# Interrupt a second transaction before changing an already-armed value.
$armed = @($backup.Entries | ForEach-Object {$_.After})
$armed[0] = @{Present=$true;Kind='DWord';Value=5}
Set-UsbTransaction 'mock' $backup $paths $armed
$script:failAt = $script:writes + 1
Assert-Throws { Set-UsbTransaction 'mock' $backup $paths @($backup.Entries | ForEach-Object {$_.Before}) } 'Interrupted disarm must retain the armed previous state.'
$backup = $script:durable
$script:failAt = -1
Set-UsbTransaction 'mock' $backup $paths @($backup.Entries | ForEach-Object {$_.Before})
Assert (Test-UsbRegistryState $script:registry['d/OriginalConfigurationValue'] $original) 'Restore after interrupted disarm must work.'

$script:registry['d/OriginalConfigurationValue'] = @{Present=$true;Kind='DWord';Value=99}
$beforeWrites = $script:writes
Assert-Throws { Set-UsbTransaction 'mock' $backup $paths @($backup.Entries | ForEach-Object {$_.Before}) } 'Third-party changes must prevent overwriting.'
Assert ($script:writes -eq $beforeWrites) 'Conflict detection must happen before any write.'
$backup.Entries[0].Name = 'OtherValue'
Assert-Throws { Assert-UsbBackup $backup $token } 'Backup-controlled registry names must be rejected.'

$fixture = Join-Path ([IO.Path]::GetTempPath()) ('rustcarplay-usb-' + [Guid]::NewGuid().ToString('N') + '.json')
try {
    $report = @(@{product_id=4776;windows_device_token=$token;configuration=@{configuration_index=5;configuration_value=6}})
    ConvertTo-Json -InputObject $report -Depth 5 | Set-Content -LiteralPath $fixture -Encoding utf8NoBOM
    $target = Get-UsbDescriptorTarget $fixture 4776 $token
    Assert ($target.Index -eq 5 -and $target.Value -eq 6) 'Descriptor index and value must remain distinct.'
    Assert-Throws { Get-UsbDescriptorTarget $fixture 4776 'winusb-FFFFFFFFFFFFFFFF' } 'Same-model phone with another instance token must be rejected.'
    Assert-Throws { Get-UsbDescriptorTarget $fixture 4777 $token } 'Descriptor product mismatch must be rejected.'
} finally { if ([IO.File]::Exists($fixture)) { [IO.File]::Delete($fixture) } }
Write-Output 'Windows USB configuration regression checks passed (mock registry only).'
