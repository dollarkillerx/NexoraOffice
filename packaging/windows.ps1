# Builds a .zip for the Windows machine this runs on.
#   pwsh packaging/windows.ps1
# There is no installer yet and the executable is not signed.
$ErrorActionPreference = 'Stop'

# Keep these in step with packaging/common.sh.
$AppBinary = 'g1-ime'
$AppPackage = 'g1-ime'

$Root = Split-Path -Parent $PSScriptRoot
$Dist = Join-Path $Root 'target/dist-packages'
$Manifest = Join-Path $Root 'Cargo.toml'

$metadata = cargo metadata --format-version 1 --no-deps --locked --manifest-path $Manifest | ConvertFrom-Json
$version = ($metadata.packages | Where-Object { $_.name -eq $AppPackage }).version
$arch = if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq 'Arm64') { 'arm64' } else { 'x64' }

cargo build --locked --profile dist -p $AppPackage --manifest-path $Manifest
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

$stage = Join-Path $Dist 'aster-office'
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item (Join-Path $Root "target/dist/$AppBinary.exe") (Join-Path $stage 'aster-office.exe')
Copy-Item (Join-Path $Root 'assets/brand/logo/aster.ico') $stage
python (Join-Path $Root 'packaging/licenses.py') --report (Join-Path $stage 'THIRD-PARTY-PACKAGES.txt') | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'licence report failed' }

$zip = Join-Path $Dist "aster-office-$version-windows-$arch.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip

Write-Host "Windows packages, version $version ($arch):"
foreach ($file in @((Join-Path $stage 'aster-office.exe'), $zip)) {
    '  {0,-44} {1,8} KB' -f (Split-Path -Leaf $file), [math]::Floor((Get-Item $file).Length / 1KB) | Write-Host
}
