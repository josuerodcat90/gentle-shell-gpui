# Verify the executable before packaging: the installer and updater both
# distribute a single EXE, without a separate Visual C++ Redistributable.
param(
    [Parameter(Mandatory = $true)]
    [string]$Exe
)

$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path -LiteralPath $Exe).Path
$tool = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
if ($tool) {
    $dumpbin = $tool.Source
} else {
    $vswhere = Join-Path ([Environment]::GetEnvironmentVariable('ProgramFiles(x86)')) 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) {
        throw 'Cannot find dumpbin.exe or vswhere.exe; install the MSVC C++ build tools.'
    }
    $install = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($LASTEXITCODE -ne 0 -or -not $install) {
        throw 'Cannot locate the MSVC C++ build tools.'
    }
    $tool = Get-ChildItem -Path (Join-Path $install 'VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe') |
        Sort-Object { [version]$_.Directory.Parent.Parent.Parent.Name } -Descending |
        Select-Object -First 1
    if (-not $tool) {
        throw 'Cannot locate dumpbin.exe in the MSVC installation.'
    }
    $dumpbin = $tool.FullName
}

$imports = & $dumpbin /DEPENDENTS $exePath
if ($LASTEXITCODE -ne 0) {
    throw "dumpbin failed with exit code $LASTEXITCODE"
}
$imports | Write-Output
# Windows 10/11 include the UCRT; the separately redistributed VC runtime
# DLLs (including suffixed variants) must not be imported.
$pattern = '^\s*(?:vcruntime|msvcp|msvcr|concrt|vccorlib|vcamp|vcomp)\d+[\w]*\.dll\s*$'
$forbidden = @($imports | Where-Object { $_ -match $pattern })
if ($forbidden.Count -gt 0) {
    throw "Unexpected Visual C++ runtime dependencies: $($forbidden.Trim() -join ', ')"
}
Write-Output 'No separately redistributed Visual C++ runtime DLL imports found.'
