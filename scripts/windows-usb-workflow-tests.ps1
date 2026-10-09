# SPDX-License-Identifier: GPL-3.0-only
# State-machine fixtures only. Never enumerates or changes a real USB device.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-usb-config.ps1')
. (Join-Path $PSScriptRoot 'windows-usb-filter.ps1')
. (Join-Path $PSScriptRoot 'start-windows-usb.ps1')
function Assert([bool]$Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Assert-Throws([scriptblock]$Action, [string]$Message) {
    $thrown = $false; try { & $Action } catch { $thrown = $true }; Assert $thrown $Message
}
$fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('rustcarplay-usb-workflow-' + [Guid]::NewGuid().ToString('N'))
$testToken = 'winusb-0123456789ABCDEF'
$script:phone = @{windows_device_token=$testToken;product_id=4776;configuration=$null;active_configuration=3}
$script:events = [Collections.Generic.List[string]]::new()
$script:rejectRestart = $true
$script:resetToInitial = $true
function Read-SelectedPhone { $script:phone }
function Restart-SelectedParent([string]$Token) {
    $script:events.Add('Restart')
    if ($script:rejectRestart) { throw 'The selected device could not restart (exit 3010).' }
    if ($script:resetToInitial) { $script:phone.configuration = $null }
}
function Switch-CarPlay {
    $script:events.Add('Mode')
    $script:phone.configuration = @{configuration_index=5;configuration_value=6}
}
$configScript = { param($Action, $DeviceToken, $StateDirectory) $script:events.Add($Action) }
try {
    $restored = @{Detached=$true;OwnFile=$true;OwnService=$true}
    Assert (Test-UsbFilterFreshRetry $restored $false $false) 'A restored failed install before creating global components must allow a fresh install.'
    Assert (-not (Test-UsbFilterFreshRetry $restored $true $false)) 'A lone driver file must not bypass existing-component verification.'
    Assert (-not (Test-UsbFilterFreshRetry $restored $false $true)) 'A lone service must not bypass existing-component verification.'
    Assert (-not (Test-UsbFilterFreshRetry @{Detached=$true;OwnFile=$false;OwnService=$false} $false $false)) 'External driver receipts must never be treated as fresh installations.'
    Assert (-not (Test-UsbFilterFreshRetry @{Detached=$false;OwnFile=$true;OwnService=$true} $false $false)) 'Unrestored partial installations must require Restore.'
    [IO.Directory]::CreateDirectory($fixtureRoot) | Out-Null
    $resumePath = Join-Path $fixtureRoot ($testToken + '.resume.json')
    Save-PreparationResume $fixtureRoot $testToken @{resume_after_replug=$true;stage='descriptor_reset'}
    $saved = Get-Content -LiteralPath $resumePath -Raw -Encoding utf8 | ConvertFrom-RustCarPlayJson
    Assert ($saved.resume_after_replug -and $saved.stage -eq 'descriptor_reset') 'Replug progress must survive the GUI exiting.'
    Save-PreparationResume $fixtureRoot $testToken @{success=$true}
    Assert (-not (Test-Path -LiteralPath $resumePath)) 'Successful preparation clears the checkpoint.'
    Save-PreparationResume $fixtureRoot $testToken @{resume_after_replug=$true;stage='initial_restart'}
    Save-PreparationResume $fixtureRoot $testToken @{resume_after_replug=$false;cleanup_error='failed'}
    Assert (-not (Test-Path -LiteralPath $resumePath)) 'Cleanup failure must invalidate the checkpoint.'
    $ResultFile = Join-Path $fixtureRoot 'result.json'
    Set-PreparationStage @{Stage='preflight'} 'descriptor_probe'
    Assert ((Get-Content -LiteralPath ($ResultFile + '.progress') -Raw -Encoding utf8) -eq 'descriptor_probe') 'The GUI must see actual helper progress.'
    $ResultFile = $null
    $progress = @{Stage='preflight'}
    $report = Join-Path $fixtureRoot ($testToken + '.descriptors.json')
    # First restart fails before discovery. A retry must retain the explicit
    # cable-replug intent even though there is no cached descriptor yet.
    Assert-Throws { Get-PreparationDescriptorReport $testToken $fixtureRoot $false $progress } 'The initial restart failure must surface.'
    Assert ($progress.Stage -eq 'initial_restart') 'The result must identify the failed preparation stage.'
    Assert (-not (Test-Path -LiteralPath $report)) 'No descriptor exists before the first mode request.'
    Assert (Test-PreparationCanResume $progress.Stage $true $true) 'Initial restart failure must allow a physical-replug resume without a report.'
    Assert (Test-PreparationCanResume 'filter_install' $true $true) 'A filter-only restore record must permit resume after its installation restart failed.'
    Assert (-not (Test-PreparationCanResume 'filter_install' $true $false)) 'No restore record means there is no verified preparation to resume.'
    Assert (-not (Test-PreparationCanResume 'preflight' $true $true)) 'Unrelated preflight failures cannot authorize skipping a restart.'

    $script:events.Clear()
    Assert-Throws { Get-PreparationDescriptorReport $testToken $fixtureRoot $true $progress } 'A second failed restart after discovery must request another physical replug.'
    Assert (($script:events -join ',') -eq 'Mode,Disarm,Restart') 'First replug must advance to discovery instead of repeating the first failed restart.'
    Assert ($progress.Stage -eq 'descriptor_reset') 'The second restart must identify the descriptor-reset stage.'
    Assert (Test-PreparationCanResume $progress.Stage $true $true) 'Second restart failure must remain resumable.'
    Assert (Test-Path -LiteralPath $report) 'Discovery must retain the descriptor for the next replug.'
    Assert ((Get-UsbDescriptorTarget $report 4776 $testToken).Index -eq 5) 'The saved report must identify the exact phone and descriptor index.'

    $script:phone.configuration = $null # the user's second physical replug
    $script:events.Clear()
    $selectedReport = Get-PreparationDescriptorReport $testToken $fixtureRoot $true $progress
    Assert ($selectedReport -eq $report -and $script:events.Count -eq 0) 'Second replug must use the fresh report directly before the existing strict Arm validation.'
    Assert-Throws { Get-UsbDescriptorTarget $report 4776 'winusb-1111111111111111' } 'Even a fresh report from another phone must fail Arm validation.'

    # Expired reports cannot be used blindly; a deliberate resume starts new
    # discovery, still skipping only the restart the physical replug replaced.
    [IO.File]::SetLastWriteTimeUtc($report, [DateTime]::UtcNow.AddMinutes(-31))
    $script:rejectRestart = $false
    $null = Get-PreparationDescriptorReport $testToken $fixtureRoot $true $progress
    Assert (($script:events -join ',') -eq 'Mode,Disarm,Restart') 'Expired descriptors require fresh mode discovery.'
    Assert ((Get-Item -LiteralPath $report).LastWriteTimeUtc -gt [DateTime]::UtcNow.AddMinutes(-1)) 'Expired reports must be replaced with the new same-phone observation.'

    [IO.File]::SetLastWriteTimeUtc($report, [DateTime]::UtcNow.AddMinutes(-31))
    $script:resetToInitial = $false
    Assert-Throws { Get-PreparationDescriptorReport $testToken $fixtureRoot $true $progress } 'A successful PnP restart without initial USB mode must not proceed to Arm.'
    Assert ($progress.Stage -eq 'descriptor_reset') 'An ineffective PnP restart must request the correct second replug stage.'

    $script:phone.configuration = @{configuration_index=5;configuration_value=6}
    $script:events.Clear()
    Assert-Throws { Get-PreparationDescriptorReport $testToken $fixtureRoot $true $progress } 'A resume without actually returning to initial mode must be refused.'
    $script:phone.configuration = $null
    $script:phone.windows_device_token = 'winusb-1111111111111111'
    Assert-Throws { Get-PreparationDescriptorReport $testToken $fixtureRoot $true $progress } 'Replacing the physical phone between attempts must be refused.'
    Assert ($script:events.Count -eq 0) 'Invalid resume requests must perform no mode or restart operations.'
} finally {
    $resolved = [IO.Path]::GetFullPath($fixtureRoot)
    $temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $resolved.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($resolved) -notlike 'rustcarplay-usb-workflow-*') { throw 'Unexpected fixture cleanup directory.' }
    if (Test-Path -LiteralPath $resolved) { Remove-Item -LiteralPath $resolved -Recurse -Force }
}
Write-Output 'Windows USB preparation/replug progression checks passed (mock devices only).'
