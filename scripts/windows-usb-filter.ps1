# SPDX-License-Identifier: GPL-3.0-only
[CmdletBinding()]
param(
    [ValidateSet('Verify', 'Install', 'Restore')][string]$Action = 'Verify',
    [string]$DeviceToken,
    [string]$StateDirectory,
    [string]$PackageDirectory,
    [string]$SignTool,
    [switch]$UseExistingFilter
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-usb-paths.ps1')
$StateDirectory = Resolve-RustCarPlayUsbStateDirectory $StateDirectory
if (-not $PackageDirectory) { $PackageDirectory = Join-Path $StateDirectory 'packages/libusb-win32-verified' }
$filterAction = $Action
$filterToken = $DeviceToken
$filterStateDirectory = [IO.Path]::GetFullPath($StateDirectory)
. (Join-Path $PSScriptRoot 'windows-usb-config.ps1')
$Action = $filterAction
$DeviceToken = $filterToken
$StateDirectory = $filterStateDirectory

function Invoke-UsbDriverVerify($Package, [string]$Member, [string]$Catalog) {
    if ($Package.NativeVerifier) {
        $arguments = @('--embedded', $Member)
        if ($Catalog) { $arguments = @('--catalog', $Catalog, $Member) }
        $output = & $Package.NativeVerifier @arguments 2>&1
    } elseif ($Catalog) { $output = & $Package.SignTool verify /kp /c $Catalog $Member 2>&1 }
    else { $output = & $Package.SignTool verify /kp $Member 2>&1 }
    if ($LASTEXITCODE -ne 0) { throw 'Kernel signing policy or catalog membership verification failed; nothing was installed.' }
}

function Get-UsbFilterPackage {
    if (-not (Test-RustCarPlayWindows) -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
        throw 'The reviewed filter installation path currently supports Windows x64 only.'
    }
    $directory = [IO.Path]::GetFullPath($PackageDirectory)
    $zip = Join-Path (Split-Path -Parent $directory) 'libusb-win32-bin-1.4.0.2.zip'
    $bundledZip = Join-Path (Split-Path -Parent $PSScriptRoot) 'runtime/usb-driver/libusb-win32-bin-1.4.0.2.zip'
    $hash = '00004C92CDB99BE36E17FB2377165EB97E63B48BA895BFC04A642EA9C3E26D94'
    if ([IO.File]::Exists($bundledZip)) {
        # The portable archive is still untrusted until the same pinned hash,
        # signatures and catalog membership checks below have all succeeded.
        $zip = $bundledZip
    } else {
        [IO.Directory]::CreateDirectory((Split-Path -Parent $zip)) | Out-Null
        if (-not [IO.File]::Exists($zip)) {
            Invoke-WebRequest -Uri 'https://github.com/mcuee/libusb-win32/releases/download/release_1.4.0.2/libusb-win32-bin-1.4.0.2.zip' -OutFile $zip
        }
    }
    if ((Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash -cne $hash) {
        throw 'The pinned upstream archive SHA-256 does not match; nothing was installed.'
    }
    $root = Join-Path $directory 'libusb-win32-bin-1.4.0.2'
    # Re-extract the verified archive. Do not trust files from a previous run.
    Expand-Archive -LiteralPath $zip -DestinationPath $directory -Force
    $sys = Join-Path $root 'bin/amd64/libusb0.sys'
    $cat = Join-Path $root 'bin/libusb0.cat'
    $exe = Join-Path $root 'bin/amd64/install-filter.exe'
    foreach ($file in @($sys, $cat)) {
        $signature = Get-AuthenticodeSignature -LiteralPath $file
        if ($signature.Status -ne 'Valid' -or ($file -eq $cat -and $signature.SignerCertificate.Subject -notmatch 'Microsoft Windows Hardware Compatibility Publisher')) {
            throw 'The driver signature or Microsoft WHCP catalog signature is not valid.'
        }
    }
    $tool = $SignTool
    if (-not $tool) {
        $candidates = @(Get-ChildItem -Path 'C:/Program Files (x86)/Windows Kits/10/bin/*/x64/signtool.exe' -ErrorAction SilentlyContinue |
            Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName)
        if ($candidates.Count) { $tool = $candidates[0] }
    }
    $packageRoot = Split-Path -Parent $PSScriptRoot
    $verifier = Join-Path $packageRoot 'tools/usb_driver_verify.exe'
    if (-not [IO.File]::Exists($verifier)) { $verifier = Join-Path $packageRoot 'target/debug/examples/usb_driver_verify.exe' }
    if (-not [IO.File]::Exists($verifier)) { $verifier = $null }
    if (-not $verifier -and (-not $tool -or -not (Test-Path -LiteralPath $tool))) {
        throw 'The USB driver verification helper is missing. Reinstall the complete application; source builds may use Windows SDK signtool.'
    }
    $package = @{ Root=$root; Sys=$sys; Cat=$cat; Exe=$exe; SignTool=$tool; NativeVerifier=$verifier; SysHash=(Get-FileHash -LiteralPath $sys -Algorithm SHA256).Hash }
    Invoke-UsbDriverVerify $package $sys $cat
    $package
}

function Assert-UsbFilterBackup($Record, [string]$Token) {
    Assert-UsbInstance $Record.Instance
    if ($Record.Version -ne 1 -or (Get-UsbToken $Record.Instance) -cne $Token -or
        $Record.SysHash -cnotmatch '^[0-9A-F]{64}$') { throw 'Invalid USB filter restore record.' }
    foreach ($state in @($Record.Before, $Record.After)) {
        if ($state.Present -and $state.Kind -cne 'MultiString') { throw 'Invalid saved filter type.' }
    }
    if ($Record.Before.Present -and $Record.Before.Value -icontains 'libusb0') {
        throw 'The restore record describes a preexisting filter; refusing to remove it.'
    }
    if ($Record.SurpriseBefore.Present -and $Record.SurpriseBefore.Kind -cne 'DWord') {
        throw 'Unexpected original SurpriseRemovalOK type.'
    }
    $expected = @(); if ($Record.Before.Present) { $expected += @($Record.Before.Value) }; $expected += 'libusb0'
    if (-not (Test-UsbRegistryState $Record.After @{Present=$true;Kind='MultiString';Value=$expected})) {
        throw 'Unexpected intended filter state in the restore record.'
    }
}

function Get-VerifiedExistingUsbFilter($Package) {
    $destination = Join-Path $env:SystemRoot 'System32/drivers/libusb0.sys'
    if (-not [IO.File]::Exists($destination)) { throw 'The existing libusb0 driver file is missing.' }
    $hash = (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash
    $version = (Get-Item -LiteralPath $destination).VersionInfo.FileVersion
    $knownLegacy = $version -eq '1.2.6.0' -and $hash -ceq '8058F2AFE6EF96A7D2DED432997FD8655970C9EA75A938EE4557D6A2CB4CC989'
    $knownCurrent = $version -eq '1.4.0.2' -and $hash -ceq $Package.SysHash
    if (-not $knownLegacy -and -not $knownCurrent) {
        throw 'The existing filter version/hash has not been reviewed for reuse. No shared driver was changed.'
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $destination
    if ($signature.Status -ne 'Valid') { throw 'The existing filter Authenticode signature is not valid.' }
    # No /a: verify its embedded kernel signature directly, without selecting
    # an unrelated older self-signed device catalog already on the machine.
    if ($knownLegacy) {
        # The explicitly supported old cross-signed 1.2.6 driver needs the
        # SDK's legacy /kp path. Never accept ordinary Authenticode as a fallback.
        if (-not $Package.SignTool -or -not [IO.File]::Exists($Package.SignTool)) {
            throw 'Reusing the existing legacy 1.2.6 USB filter requires SDK signtool kernel-policy verification; the shared driver was not changed.'
        }
        $output = & $Package.SignTool verify /kp $destination 2>&1
        if ($LASTEXITCODE -ne 0) { throw 'The existing filter did not pass kernel signing policy verification.' }
    } else { Invoke-UsbDriverVerify $Package $destination $Package.Cat }
    $key = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey('SYSTEM\CurrentControlSet\Services\libusb0')
    if ($null -eq $key) { throw 'The existing libusb0 service is missing.' }
    try {
        $image = [string]$key.GetValue('ImagePath')
        $allowedImages = @('system32\drivers\libusb0.sys', '\SystemRoot\System32\drivers\libusb0.sys', $destination)
        if ($key.GetValue('Type') -ne 1 -or $key.GetValue('Start') -ne 3 -or $allowedImages -inotcontains $image) {
            throw 'The existing kernel service path/type/start mode is not the reviewed configuration.'
        }
    } finally { $key.Dispose() }
    $state = (Get-Service -Name libusb0).Status.ToString()
    if ($state -notin @('Stopped','Running')) { throw 'The existing libusb0 service is transitioning; retry when stable.' }
    @{ Hash=$hash; Version=$version; ImagePath=$image; Legacy=$knownLegacy; ServiceState=$state }
}

function Test-UsbFilterFreshRetry($Record, [bool]$FileExists, [bool]$ServiceExists) {
    # Only a fully restored, project-owned failed installation with neither
    # global component present may be treated as a fresh install. One missing
    # component or an external/shared receipt still requires explicit review.
    $Record -and $Record.ContainsKey('Detached') -and $Record.Detached -eq $true -and
        $Record.OwnFile -eq $true -and $Record.OwnService -eq $true -and
        -not $FileExists -and -not $ServiceExists
}

function Invoke-UsbFilter {
    if ($Action -eq 'Verify') {
        $package = Get-UsbFilterPackage
        $result = @{ archive_sha256_verified=$true; microsoft_signature_verified=$true; kernel_catalog_membership_verified=$true; installed=$false; runtime_directory=(Join-Path $package.Root 'bin/amd64'); reuse_existing_filter=[bool]$UseExistingFilter }
        $saved = $null
        if ($DeviceToken) {
            if ($DeviceToken -cnotmatch '^winusb-[0-9A-F]{16}$') { throw 'Invalid device_token.' }
            $savedPath = Join-Path $StateDirectory ($DeviceToken + '.filter.json')
            if ([IO.File]::Exists($savedPath)) {
                if ((Get-Item -LiteralPath $savedPath).Length -gt 1048576) { throw 'Filter restore record is too large.' }
                $saved = Get-Content -LiteralPath $savedPath -Raw -Encoding utf8 | ConvertFrom-RustCarPlayJson
                Assert-UsbFilterBackup $saved $DeviceToken
                $result.reuse_existing_filter = $UseExistingFilter -or -not $saved.OwnFile -or -not $saved.OwnService
            }
        }
        $freshRetry = $false
        if ($saved -and -not $UseExistingFilter) {
            $driverExists = [IO.File]::Exists((Join-Path $env:SystemRoot 'System32/drivers/libusb0.sys'))
            $service = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey('SYSTEM\CurrentControlSet\Services\libusb0')
            try { $freshRetry = Test-UsbFilterFreshRetry $saved $driverExists ($null -ne $service) }
            finally { if ($null -ne $service) { $service.Dispose() } }
        }
        if (($UseExistingFilter -or $saved) -and -not $freshRetry) {
            $existing = Get-VerifiedExistingUsbFilter $package
            if ($saved -and $existing.Hash -cne $saved.SysHash) {
                throw 'The installed USB filter differs from the saved restore record; no shared driver was changed.'
            }
            $result.existing_driver_verified = $true
            $result.existing_driver_version = $existing.Version
            $result.existing_driver_control_only = $existing.Legacy
        }
        $result | ConvertTo-Json -Compress
        return
    }
    $principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Install/Restore requires an administrator PowerShell terminal. Verify is available without elevation.'
    }
    $parents = @(Get-UsbParents)
    if (-not $DeviceToken) {
        if ($parents.Count -ne 1) { throw 'Select one device_token from windows-usb-config.ps1 -Action Inspect.' }
        $DeviceToken = Get-UsbToken $parents[0].InstanceId
    }
    if ($DeviceToken -cnotmatch '^winusb-[0-9A-F]{16}$') { throw 'Invalid device_token.' }
    $path = Join-Path $StateDirectory ($DeviceToken + '.filter.json')
    $mutex = [Threading.Mutex]::new($false, ('Global\RustCarPlayUsb-' + $DeviceToken))
    $locked = $false
    try {
        try { $locked = $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $locked = $true }
        if (-not $locked) { throw 'Another USB setup transaction is running for this phone.' }
        if ($Action -eq 'Install') {
            $previous = $null
            if ([IO.File]::Exists($path)) {
                if ((Get-Item -LiteralPath $path).Length -gt 1048576) { throw 'Filter restore record is too large.' }
                $previous = Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-RustCarPlayJson
                Assert-UsbFilterBackup $previous $DeviceToken
                if (-not $previous.Contains('Detached') -or -not $previous.Detached) {
                    throw 'An active or partial installation record already exists. Restore it before another install.'
                }
            }
            $selected = @($parents | Where-Object { (Get-UsbToken $_.InstanceId) -ceq $DeviceToken })
            if ($selected.Count -ne 1) { throw 'The selected USB parent is not present.' }
            $instance = $selected[0].InstanceId
            $paths = Get-UsbPaths $instance
            $before = Get-UsbRegistryState $paths.Parent 'UpperFilters'
            if ($before.Present -and ($before.Kind -ne 'MultiString' -or $before.Value -icontains 'libusb0')) {
                throw 'Unexpected or preexisting libusb0 upper filter; nothing was changed.'
            }
            if ($previous -and -not (Test-UsbRegistryState $before $previous.Before)) {
                throw 'The upper filters changed after restore; refusing to reuse the previous installation.'
            }
            $destination = Join-Path $env:SystemRoot 'System32/drivers/libusb0.sys'
            $package = Get-UsbFilterPackage
            $external = $null
            if ($UseExistingFilter) { $external = Get-VerifiedExistingUsbFilter $package }
            $existingService = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey('SYSTEM\CurrentControlSet\Services\libusb0')
            $reuseService = $false
            if ($null -ne $existingService) {
                try {
                    $imagePath = [string]$existingService.GetValue('ImagePath')
                    $allowedImages = @('system32\drivers\libusb0.sys', '\SystemRoot\System32\drivers\libusb0.sys', $destination)
                    if ((-not $external -and (-not $previous -or -not $previous.OwnService)) -or $existingService.GetValue('Type') -ne 1 -or
                        $existingService.GetValue('Start') -ne 3 -or $allowedImages -inotcontains $imagePath) {
                        throw 'A preexisting or changed libusb0 service is present; do not replace a shared driver automatically.'
                    }
                    $reuseService = $true
                } finally { $existingService.Dispose() }
            }
            $reuseFile = [IO.File]::Exists($destination)
            if ($reuseFile -and -not $external -and (-not $previous -or -not $previous.OwnFile -or
                (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash -cne $previous.SysHash)) {
                throw 'A preexisting or changed libusb0.sys is present; do not overwrite a shared driver automatically.'
            }
            $values = @(); if ($before.Present) { $values += @($before.Value) }; $values += 'libusb0'
            $after = @{Present=$true;Kind='MultiString';Value=$values}
            $catalogName = 'rustcarplay-libusb0-' + [Guid]::NewGuid().ToString('N') + '.cat'
            $surpriseBefore = Get-UsbRegistryState $paths.Parameters 'SurpriseRemovalOK'
            $record = @{ Version=1; Instance=$instance; Before=$before; After=$after; SurpriseBefore=$surpriseBefore; SysHash=$package.SysHash; CatalogName=$catalogName; OwnService=$true; OwnFile=$true; Detached=$false }
            if ($external) {
                $record.SysHash = $external.Hash
                $record.CatalogName = $null
                $record.OwnService = $false
                $record.OwnFile = $false
                $record.ExistingDriverVersion = $external.Version
                $record.ExistingServiceImagePath = $external.ImagePath
            }
            Assert-UsbFilterBackup $record $DeviceToken
            [IO.Directory]::CreateDirectory($StateDirectory) | Out-Null
            Save-UsbBackup $path $record ($null -eq $previous)
            # The WHCP signature lives in the catalog. Register it through the
            # supported catalog database tool, never copy into CatRoot by hand.
            # A random basename plus /u cannot replace an existing catalog.
            if (-not $external) {
                $localCatalog = Join-Path $StateDirectory $catalogName
                [IO.File]::Copy($package.Cat, $localCatalog, $false)
                if ($package.NativeVerifier) {
                    $output = & $package.NativeVerifier --add-catalog $localCatalog 2>&1
                    if ($LASTEXITCODE -ne 0) { throw 'Registering the verified Microsoft driver catalog failed; no filter was attached.' }
                    $registration = ($output -join "`n") | ConvertFrom-Json
                    $record.CatalogName = $registration.catalog_name
                    Save-UsbBackup $path $record $false
                } else {
                    $output = & $package.SignTool catdb /u $localCatalog 2>&1
                    if ($LASTEXITCODE -ne 0) { throw 'Registering the verified Microsoft driver catalog failed; no filter was attached.' }
                }
            }
            if (-not $reuseFile) { [IO.File]::Copy($package.Sys, $destination, $false) }
            # Upstream --device-id matches the full instance. --device matches
            # a hardware ID and MUST NOT be substituted here. The installer
            # restarts the selected parent as part of attaching the filter.
            if ($reuseService) {
                # Do not rerun an installer that stops other libusb devices.
                # Reuse our hash-checked service and only reattach this phone.
                if ($external) {
                    $checked = Get-VerifiedExistingUsbFilter $package
                    if ($checked.Hash -cne $external.Hash -or $checked.ImagePath -cne $external.ImagePath) {
                        throw 'The existing shared driver changed during preparation; no attachment was made.'
                    }
                }
                Set-UsbRegistryState $paths.Parent 'UpperFilters' $after
                $output = & "$env:SystemRoot/System32/pnputil.exe" /restart-device $instance 2>&1
            } else {
                $output = & $package.Exe install ("--device-id=" + $instance) 2>&1
            }
            if ($LASTEXITCODE -ne 0) {
                $reason = ($output -join ' ').Replace($instance, '[selected iPhone]')
                if ($reason.Length -gt 700) { $reason = $reason.Substring(0,700) }
                throw ('The scoped filter attachment/restart failed (exit ' + $LASTEXITCODE + '): ' + $reason + '. The saved Restore action can remove any partial attachment.')
            }
            $actual = Get-UsbRegistryState $paths.Parent 'UpperFilters'
            if (-not (Test-UsbRegistryState $actual $after)) {
                throw 'The installer did not produce the exact expected filter list. Review the saved restore record.'
            }
            @{ action='Install'; device_token=$DeviceToken; device_restarted=$true; installed=$true; restore_record_saved=$true; reused_existing_driver=($null -ne $external); global_driver_modified=($null -eq $external -and -not $reuseService) } | ConvertTo-Json -Compress
        } else {
            if (-not [IO.File]::Exists($path) -or (Get-Item -LiteralPath $path).Length -gt 1048576) { throw 'A bounded filter restore record is required.' }
            $record = Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-RustCarPlayJson
            Assert-UsbFilterBackup $record $DeviceToken
            $paths = Get-UsbPaths $record.Instance
            $actual = Get-UsbRegistryState $paths.Parent 'UpperFilters'
            if (-not (Test-UsbRegistryState $actual $record.Before) -and -not (Test-UsbRegistryState $actual $record.After)) {
                throw 'Another program changed this phone upper filters. Refusing to overwrite its changes.'
            }
            $surpriseActual = Get-UsbRegistryState $paths.Parameters 'SurpriseRemovalOK'
            if ($surpriseActual.Present -and -not (Test-UsbRegistryState $surpriseActual $record.SurpriseBefore)) {
                throw 'Another program changed SurpriseRemovalOK; refusing to overwrite its changes.'
            }
            Set-UsbRegistryState $paths.Parent 'UpperFilters' $record.Before
            Set-UsbRegistryState $paths.Parameters 'SurpriseRemovalOK' $record.SurpriseBefore
            # Do not call upstream uninstall: it also touches legacy/global
            # services. Restore this exact instance and leave the inert shared
            # service/binary in place until no device refers to it.
            $present = @($parents | Where-Object { $_.InstanceId -ieq $record.Instance })
            if ($present.Count -eq 1) {
                $output = & "$env:SystemRoot/System32/pnputil.exe" /restart-device $record.Instance 2>&1
                if ($LASTEXITCODE -ne 0) { throw 'Filter registry state was restored; unplug/replug the selected phone to unload its old stack.' }
            }
            @{ action='Restore'; device_token=$DeviceToken; filters_restored=$true; shared_service_retained=$true; driver_file_retained=$true; device_restarted=($present.Count -eq 1) } | ConvertTo-Json -Compress
            $record.Detached = $true
            Save-UsbBackup $path $record $false
        }
    } finally {
        if ($locked) { $mutex.ReleaseMutex() }
        $mutex.Dispose()
    }
}

if ($MyInvocation.InvocationName -ne '.') { Invoke-UsbFilter }
