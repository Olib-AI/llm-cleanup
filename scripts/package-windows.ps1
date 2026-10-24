<#
.SYNOPSIS
    Stage the Windows desktop app together with the Microsoft Visual C++ runtime.

.DESCRIPTION
    `aiclean-gui.exe` is built against the *dynamic* MSVC CRT (the Rust default for
    `*-pc-windows-msvc`), so it needs VCRUNTIME140.dll and friends at load time. Those
    DLLs are part of the "Microsoft Visual C++ 2015-2022 Redistributable", which is
    present on many machines but is *not* part of a clean Windows install. Rather than
    asking users to install a redistributable - or worse, letting them hit
    "The code execution cannot proceed because VCRUNTIME140.dll was not found" - we
    deploy the runtime app-local: the redistributable DLLs sit next to the executable,
    and the Windows loader resolves them from the application directory first.

    This script:
      1. locates the Visual Studio VC redistributable folder (via vswhere, so it works
         on any hosted or developer machine, VS 2019 or 2022),
      2. copies the app-local runtime DLL set next to the executable,
      3. falls back to the system copies of the same redistributable files if no
         Visual Studio layout is present (e.g. a machine with only the Build Tools
         workload installed),
      4. writes WINDOWS-RUNTIME.txt so the contents of the folder are self-explaining,
      5. with -Verify, fails the build if a required DLL is missing. The release
         workflow always passes -Verify: a silently runtime-less zip is never shipped.

.PARAMETER StageDir
    Directory to populate. Created if missing. Default: "stage".

.PARAMETER TargetDir
    Cargo target directory holding the release build. Default: "target/release".

.PARAMETER Verify
    Fail if any required runtime DLL is missing after staging.

.EXAMPLE
    pwsh ./scripts/package-windows.ps1 -StageDir stage -Verify
#>
[CmdletBinding()]
param(
    [string] $StageDir = 'stage',
    [string] $TargetDir = 'target/release',
    [switch] $Verify
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The MSVC 2015-2022 redistributable set. vcruntime140*.dll and msvcp140*.dll are the
# ones a Rust/Slint binary imports; the rest ship in the same folder and are copied for
# completeness (a native dependency may pull in concrt140 or vccorlib140 later).
$RuntimeDlls = @(
    'concrt140.dll',
    'msvcp140.dll',
    'msvcp140_1.dll',
    'msvcp140_2.dll',
    'msvcp140_atomic_wait.dll',
    'msvcp140_codecvt_ids.dll',
    'vccorlib140.dll',
    'vcruntime140.dll',
    'vcruntime140_1.dll'
)

# Without these three the binary cannot start at all.
$RequiredDlls = @('vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll')

function Write-Step([string] $Message) {
    Write-Host "==> $Message"
}

function Find-VcRedistDir {
    <#
      Returns the newest `.../VC/Redist/MSVC/<version>/x64/Microsoft.VC*.CRT` folder,
      or $null. Uses vswhere (installed by every Visual Studio / Build Tools setup) so
      a non-default installation path is still found.
    #>
    $candidates = [System.Collections.Generic.List[string]]::new()

    $programFilesX86 = [Environment]::GetEnvironmentVariable('ProgramFiles(x86)')
    $vswhere = if ($programFilesX86) {
        Join-Path $programFilesX86 'Microsoft Visual Studio\Installer\vswhere.exe'
    } else {
        $null
    }
    $installRoots = [System.Collections.Generic.List[string]]::new()

    if ($vswhere -and (Test-Path $vswhere)) {
        $paths = & $vswhere -latest -prerelease -products * `
                             -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
                             -property installationPath 2>$null
        foreach ($p in @($paths)) {
            if ($p -and (Test-Path $p)) { $installRoots.Add($p.Trim()) }
        }
    }
    # Conventional locations as a safety net (older images, custom layouts).
    foreach ($base in @("${env:ProgramFiles}\Microsoft Visual Studio", $programFilesX86)) {
        if (-not $base -or -not (Test-Path $base)) { continue }
        foreach ($year in @('2022', '2019')) {
            $candidate = Join-Path $base $year
            if (Test-Path $candidate) {
                Get-ChildItem $candidate -Directory -ErrorAction SilentlyContinue |
                    ForEach-Object { $installRoots.Add($_.FullName) }
            }
        }
    }

    foreach ($root in $installRoots) {
        $redistRoot = Join-Path $root 'VC\Redist\MSVC'
        if (-not (Test-Path $redistRoot)) { continue }
        # Newest toolset first.
        $versionDirs = Get-ChildItem $redistRoot -Directory -ErrorAction SilentlyContinue |
                       Sort-Object Name -Descending
        foreach ($v in $versionDirs) {
            foreach ($arch in @('x64', 'onecore\x64')) {
                $dir = Join-Path $v.FullName "$arch\Microsoft.VC143.CRT"
                if (Test-Path $dir) { $candidates.Add($dir); continue }
                # Toolset number varies (VC142/VC143/VC144); match the pattern.
                $anyCrt = Get-ChildItem (Join-Path $v.FullName $arch) -Directory -ErrorAction SilentlyContinue |
                          Where-Object { $_.Name -like 'Microsoft.VC*.CRT' } |
                          Sort-Object Name -Descending
                foreach ($d in $anyCrt) { $candidates.Add($d.FullName) }
            }
        }
    }

    if ($candidates.Count -gt 0) { return $candidates[0] }
    return $null
}

# ---------------------------------------------------------------------------
# 1. Stage the executable and the human-facing files.
# ---------------------------------------------------------------------------
New-Item -ItemType Directory -Force -Path $StageDir | Out-Null

$exe = Join-Path $TargetDir 'aiclean-gui.exe'
if (-not (Test-Path $exe)) {
    throw "missing $exe — build the GUI first: cargo build --release -p cleanup-gui"
}
Copy-Item $exe $StageDir -Force
Write-Step "staged aiclean-gui.exe"

foreach ($extra in @('README.md', 'LICENSE')) {
    if (Test-Path $extra) { Copy-Item $extra $StageDir -Force }
}

# ---------------------------------------------------------------------------
# 2. App-local Visual C++ runtime.
# ---------------------------------------------------------------------------
$runtimeSource = Find-VcRedistDir
$source = 'none'
if ($runtimeSource) {
    $source = $runtimeSource
    foreach ($dll in $RuntimeDlls) {
        $from = Join-Path $runtimeSource $dll
        if (Test-Path $from) { Copy-Item $from $StageDir -Force }
    }
    Write-Step "copied the Visual C++ runtime from $runtimeSource"
} else {
    Write-Warning 'No Visual Studio redistributable layout found; falling back to the system copies.'
    $source = "$env:SystemRoot\System32"
    foreach ($dll in $RuntimeDlls) {
        $from = Join-Path $source $dll
        if (Test-Path $from) { Copy-Item $from $StageDir -Force }
    }
    Write-Step "copied the Visual C++ runtime from $source"
}

# Report what we actually got, with versions, so a bad runner image is visible in the log.
$staged = Get-ChildItem $StageDir -Filter '*.dll' -ErrorAction SilentlyContinue |
          Sort-Object Name
if ($staged) {
    $table = $staged | ForEach-Object {
        [pscustomobject]@{
            Dll     = $_.Name
            Version = $_.VersionInfo.FileVersion
            Bytes   = $_.Length
        }
    }
    $table | Format-Table -AutoSize | Out-String | Write-Host
} else {
    Write-Warning 'No DLLs were staged.'
}

# ---------------------------------------------------------------------------
# 3. Explain the folder to whoever unzips it.
# ---------------------------------------------------------------------------
$runtimeNote = @"
llm-cleanup for Windows
=======================

Run:  aiclean-gui.exe

This folder contains the Microsoft Visual C++ 2015-2022 runtime
(vcruntime140.dll, vcruntime140_1.dll, msvcp140.dll, ...), deployed app-local
next to the executable. That is why the app starts on a clean Windows install
with no "VCRUNTIME140.dll is missing" prompt and no administrator rights: the
Windows loader resolves these DLLs from the application folder first.

Do not delete or move the .dll files. If you copy the executable somewhere
else, copy the whole folder.

Source of the runtime DLLs: $source
Packaged: $(Get-Date -Format 'yyyy-MM-dd')

The command-line tool (aiclean.exe) is a single self-contained binary that
needs no runtime DLLs at all.
"@
Set-Content -Path (Join-Path $StageDir 'WINDOWS-RUNTIME.txt') -Value $runtimeNote -Encoding UTF8

# ---------------------------------------------------------------------------
# 4. Verify.
# ---------------------------------------------------------------------------
$missing = @()
foreach ($dll in $RequiredDlls) {
    if (-not (Test-Path (Join-Path $StageDir $dll))) { $missing += $dll }
}

if ($missing.Count -gt 0) {
    $message = "The Visual C++ runtime is incomplete; the app would fail to launch on a clean Windows install. Missing: $($missing -join ', ')"
    if ($Verify) { throw $message }
    Write-Warning $message
} else {
    Write-Step "verified the required Visual C++ runtime DLLs are staged"
}

Write-Step "Windows package ready in $StageDir"
