# Install ClipSync on Windows from GitHub Releases.
# Usage:
#   irm https://raw.githubusercontent.com/Jeon1691/clipsync/master/scripts/install-windows.ps1 | iex
#   ./scripts/install-windows.ps1 -Winget
#   ./scripts/install-windows.ps1 -Version 0.1.12
[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$InstallDir = $(Join-Path $env:LOCALAPPDATA "ClipSync"),
    [switch]$Winget
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$Repo = "Jeon1691/clipsync"
$AssetName = "clipsync-x86_64-pc-windows-msvc.zip"

function Get-Release {
    $tag = $Version.Trim()
    if ($tag -eq "latest") {
        $uri = "https://api.github.com/repos/$Repo/releases/latest"
    } else {
        $tag = $tag.TrimStart("v")
        $uri = "https://api.github.com/repos/$Repo/releases/tags/v$tag"
    }
    Invoke-RestMethod -Uri $uri -Headers @{ "User-Agent" = "clipsync-install"; "Accept" = "application/vnd.github+json" }
}

function Get-WindowsAsset($Release) {
    $asset = $Release.assets | Where-Object { $_.name -eq $AssetName } | Select-Object -First 1
    if (-not $asset) {
        throw "Release $($Release.tag_name) has no $AssetName asset."
    }
    $asset
}

$release = Get-Release
$asset = Get-WindowsAsset $release
$tag = $release.tag_name
Write-Host "ClipSync $tag"

if ($Winget) {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        throw "winget is not installed. Re-run without -Winget."
    }
    $work = Join-Path $env:TEMP ("clipsync-winget-" + [guid]::NewGuid().ToString("n"))
    New-Item -ItemType Directory -Path $work | Out-Null
    $zip = Join-Path $work $AssetName
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zip -Headers @{ "User-Agent" = "clipsync-install" }
    $sha = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    $ver = $tag.TrimStart("v")
    @"
PackageIdentifier: Jeon1691.ClipSync
PackageVersion: $ver
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.6.0
"@ | Set-Content -Path (Join-Path $work "Jeon1691.ClipSync.yaml") -Encoding utf8
    @"
PackageIdentifier: Jeon1691.ClipSync
PackageVersion: $ver
PackageLocale: en-US
Publisher: Jeon1691
PackageName: ClipSync
License: MIT
ShortDescription: End-to-end encrypted clipboard sync
ManifestType: defaultLocale
ManifestVersion: 1.6.0
"@ | Set-Content -Path (Join-Path $work "Jeon1691.ClipSync.locale.en-US.yaml") -Encoding utf8
    @"
PackageIdentifier: Jeon1691.ClipSync
PackageVersion: $ver
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
- RelativeFilePath: clipsync.exe
  PortableCommandAlias: clipsync
Installers:
- Architecture: x64
  InstallerUrl: $($asset.browser_download_url)
  InstallerSha256: $sha
ManifestType: installer
ManifestVersion: 1.6.0
"@ | Set-Content -Path (Join-Path $work "Jeon1691.ClipSync.installer.yaml") -Encoding utf8
    winget install --manifest $work --accept-package-agreements --accept-source-agreements
    Write-Host "Installed with winget. Open a new terminal and run: clipsync --version"
    return
}

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
$zip = Join-Path $env:TEMP $AssetName
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zip -Headers @{ "User-Agent" = "clipsync-install" }
$stage = Join-Path $env:TEMP ("clipsync-expand-" + $tag)
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
Expand-Archive -Path $zip -DestinationPath $stage -Force
Get-ChildItem -Path $stage -Filter *.exe -Recurse | ForEach-Object {
    Copy-Item -Path $_.FullName -Destination (Join-Path $InstallDir $_.Name) -Force
    Write-Host "Installed $($_.Name) -> $InstallDir"
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not $userPath) { $userPath = "" }
$parts = $userPath -split ";" | Where-Object { $_ -and ($_ -ne $InstallDir) }
$updated = (@($InstallDir) + $parts) -join ";"
[Environment]::SetEnvironmentVariable("Path", $updated, "User")
if (($env:Path -split ";") -notcontains $InstallDir) {
    $env:Path = "$InstallDir;$env:Path"
}
Write-Host "ClipSync $tag is on PATH for new terminals ($InstallDir)."
Write-Host "Try: clipsync --version"
