# SPDX-License-Identifier: 0BSD
[CmdletBinding()]
param(
    [string]$Destination = (Join-Path $env:LOCALAPPDATA 'Programs\Magpie Rust Portable'),
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = @($userPath -split ';' | Where-Object { $_ -and $_.TrimEnd('\') -ine $Destination.TrimEnd('\') })
if ($Uninstall) {
    'magpie.exe', 'magpie-desktop.exe', 'LICENSE', 'BUILD-METADATA.json', 'INSTALL.md', 'install.ps1' |
        ForEach-Object { Remove-Item -LiteralPath (Join-Path $Destination $_) -Force -ErrorAction SilentlyContinue }
    [Environment]::SetEnvironmentVariable('Path', ($entries -join ';'), 'User')
    Write-Output "Removed portable binaries from $Destination. Open a new terminal."
    return
}
$source = Join-Path $PSScriptRoot 'bin\magpie.exe'
if (-not (Test-Path -LiteralPath $source)) { throw "Extract the complete release ZIP before running install.ps1" }
New-Item -ItemType Directory -Force -Path $Destination | Out-Null
Copy-Item -LiteralPath $source -Destination $Destination -Force
$desktop = Join-Path $PSScriptRoot 'bin\magpie-desktop.exe'
if (Test-Path -LiteralPath $desktop) {
    Copy-Item -LiteralPath $desktop -Destination $Destination -Force
}
'LICENSE', 'BUILD-METADATA.json', 'INSTALL.md', 'install.ps1' | ForEach-Object {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $_) -Destination $Destination -Force
}
[Environment]::SetEnvironmentVariable('Path', (($entries + $Destination) -join ';'), 'User')
$env:Path += ";$Destination"
Write-Output "Installed to $Destination. Open a new terminal and run magpie --help."
