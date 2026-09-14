<#
.SYNOPSIS
Install adbx from a GitHub Release.
#>
[CmdletBinding()]
param(
    [Parameter()]
    [string]$Version = 'latest',

    [Parameter()]
    [string]$InstallDir,

    [Parameter()]
    [switch]$Help
)

$ErrorActionPreference = 'Stop'

$Repository = 'location-txl/adbx'
$ReleaseBaseUrl = "https://github.com/$Repository/releases"

function Show-Usage {
    @'
Install adbx from a GitHub Release.

Usage:
  .\install.ps1 [-Version <version>] [-InstallDir <directory>]

Parameters:
  -Version <version>       Install a specific version, for example v0.1.0; default: latest stable release
  -InstallDir <directory> Installation directory; default: %LOCALAPPDATA%\adbx\bin
  -Help                   Show this help
'@
}

function Normalize-ReleaseTag {
    param(
        [Parameter(Mandatory = $true)]
        [string]$InputTag
    )

    if ([string]::IsNullOrWhiteSpace($InputTag)) {
        throw 'The version cannot be empty'
    }

    if ($InputTag -eq 'latest') {
        return 'latest'
    }

    $tag = if ($InputTag.StartsWith('v')) { $InputTag } else { "v$InputTag" }

    # The version is used in a URL and file name, so accept only the SemVer form used by the release workflow.
    if ($tag -notmatch '^v\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$') {
        throw "Version must look like v0.1.0: $InputTag"
    }

    return $tag
}

function Resolve-LatestReleaseTag {
    try {
        $response = Invoke-WebRequest -Uri "$ReleaseBaseUrl/latest" -MaximumRedirection 10 -UseBasicParsing
    }
    catch {
        throw 'Unable to fetch the latest GitHub Release'
    }

    $finalUri = $response.BaseResponse.ResponseUri
    # PowerShell 7 uses HttpResponseMessage; Windows PowerShell 5.1 exposes ResponseUri directly.
    if ($null -eq $finalUri) {
        $finalUri = $response.BaseResponse.RequestMessage.RequestUri
    }
    if ($null -eq $finalUri) {
        throw 'Unable to resolve the version tag from the GitHub Release response'
    }

    $segments = $finalUri.AbsolutePath.Trim('/').Split('/')
    if ($segments.Count -lt 5 -or $segments[$segments.Count - 2] -ne 'tag') {
        throw 'No stable GitHub Release is available'
    }

    return Normalize-ReleaseTag $segments[$segments.Count - 1]
}

function Get-NormalizedPath {
    param(
        [AllowNull()]
        [string]$PathValue
    )

    if ([string]::IsNullOrWhiteSpace($PathValue)) {
        return ''
    }

    try {
        return ([IO.Path]::GetFullPath($PathValue)).TrimEnd('\')
    }
    catch {
        return $PathValue.TrimEnd('\')
    }
}

function Test-PathEntry {
    param(
        [AllowNull()]
        [string]$PathValue,

        [Parameter(Mandatory = $true)]
        [string]$Candidate
    )

    $normalizedCandidate = Get-NormalizedPath $Candidate
    foreach ($entry in @($PathValue -split ';')) {
        if ((Get-NormalizedPath $entry) -ieq $normalizedCandidate) {
            return $true
        }
    }

    return $false
}

function Add-UserPathEntry {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Directory
    )

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not (Test-PathEntry $userPath $Directory)) {
        $newUserPath = if ([string]::IsNullOrWhiteSpace($userPath)) {
            $Directory
        }
        else {
            "$userPath;$Directory"
        }
        [Environment]::SetEnvironmentVariable('Path', $newUserPath, 'User')
    }

    # Persisting PATH does not update the parent PowerShell process, so update this process too.
    if (-not (Test-PathEntry $env:Path $Directory)) {
        $env:Path = "$Directory;$env:Path"
    }
}

if ($Help) {
    Show-Usage
    exit 0
}

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'This installer only supports Windows; use install.sh on macOS or Linux'
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
        throw 'Unable to determine the Windows user installation directory: LOCALAPPDATA is not set'
    }
    $InstallDir = Join-Path $env:LOCALAPPDATA 'adbx\bin'
}
$InstallDir = [IO.Path]::GetFullPath($InstallDir)

# Detect the OS architecture instead of the current PowerShell process:
# Windows on ARM runs x64/x86 processes under emulation, where
# PROCESSOR_ARCHITECTURE reports AMD64/x86. .NET's OSArchitecture reflects
# the real OS; fall back to the WOW64 variable for old .NET Framework builds.
$architecture = $null
try {
    $architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
}
catch {
    $architecture = $env:PROCESSOR_ARCHITEW6432
    if ([string]::IsNullOrWhiteSpace($architecture)) {
        $architecture = $env:PROCESSOR_ARCHITECTURE
    }
}
if ([string]::IsNullOrWhiteSpace($architecture)) {
    throw 'Unable to detect the CPU architecture'
}

switch ($architecture.ToUpperInvariant()) {
    'ARM64' {
        $target = 'aarch64-pc-windows-msvc'
    }
    'AMD64', 'X64' {
        $target = 'x86_64-pc-windows-msvc'
    }
    default {
        throw "Unsupported Windows architecture: $architecture; the current Release provides Windows x64 and ARM64"
    }
}

try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
}
catch {
    # PowerShell 7 uses its own HTTP stack and does not need ServicePointManager.
}

$tag = Normalize-ReleaseTag $Version
if ($tag -eq 'latest') {
    $tag = Resolve-LatestReleaseTag
}

$assetName = "adbx-$tag-$target.zip"
$downloadUrl = "$ReleaseBaseUrl/download/$tag/$assetName"
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("adbx-install-" + [Guid]::NewGuid().ToString('N'))
$stagedPath = $null

try {
    New-Item -ItemType Directory -Path $tempRoot -Force | Out-Null
    $archivePath = Join-Path $tempRoot $assetName
    $extractDir = Join-Path $tempRoot 'extracted'

    Write-Host "Downloading $assetName..."
    Invoke-WebRequest -Uri $downloadUrl -OutFile $archivePath -UseBasicParsing

    # Extract and validate the binary before touching the final installation directory.
    New-Item -ItemType Directory -Path $extractDir -Force | Out-Null
    Expand-Archive -LiteralPath $archivePath -DestinationPath $extractDir -Force
    $binaryPath = Join-Path $extractDir 'adbx.exe'
    if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
        throw 'The Release archive does not contain the adbx.exe binary'
    }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $destination = Join-Path $InstallDir 'adbx.exe'
    $stagedPath = Join-Path $InstallDir ('.adbx.exe.' + [Guid]::NewGuid().ToString('N') + '.tmp')
    Copy-Item -LiteralPath $binaryPath -Destination $stagedPath -Force

    # Write a temporary file first, then replace the installed binary after validation succeeds.
    Move-Item -LiteralPath $stagedPath -Destination $destination -Force
    $stagedPath = $null
}
finally {
    if ($null -ne $stagedPath -and (Test-Path -LiteralPath $stagedPath)) {
        Remove-Item -LiteralPath $stagedPath -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}

try {
    Add-UserPathEntry $InstallDir
    Write-Host "[OK] Added $InstallDir to the current user PATH"
}
catch {
    Write-Warning "The binary was installed, but updating the user PATH failed: $($_.Exception.Message)"
    Write-Host "Add $InstallDir to the current user PATH manually"
}

Write-Host "[OK] adbx installed to $destination"
try {
    & $destination --version
}
catch {
    Write-Warning "Unable to run the newly installed adbx: $($_.Exception.Message)"
}
Write-Host 'Open a new terminal to use adbx directly.'
