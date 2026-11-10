#Requires -Version 5.1
<#
.SYNOPSIS
    Installs the latest rusted release for Windows x64.

.EXAMPLE
    .\install.ps1
    .\install.ps1 -Version v0.1.0
    .\install.ps1 -InstallDir D:\tools\rusted
#>
[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$InstallDir
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = "Stop"
$Repo = "gabriel-aplok/rusted"
$Target = "x86_64-pc-windows-msvc"

[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

function Write-Info([string]$Message) {
    Write-Host "rusted install: $Message"
}

if (-not $InstallDir) {
    $InstallDir = Join-Path $env:LOCALAPPDATA "Programs\rusted"
}

$Api = "https://api.github.com/repos/$Repo/releases/$Version"
Write-Info "reading release metadata from $Api"
$Release = Invoke-RestMethod -Uri $Api -UserAgent "rusted-installer"
$Tag = $Release.tag_name

$BinaryAsset = $Release.assets | Where-Object { $_.name -eq "rusted-$Target.exe" }
$ChecksumAsset = $Release.assets | Where-Object { $_.name -eq "checksums.sha256" }
if (-not $BinaryAsset -or -not $ChecksumAsset) {
    throw "release $Tag has no binary or checksum asset for $Target"
}

$Temp = New-Item -ItemType Directory -Path (Join-Path ([IO.Path]::GetTempPath()) "rusted-install-$PID") -Force
try {
    $ExePath = Join-Path $Temp "rusted.exe"
    $ChecksumPath = Join-Path $Temp "checksums.sha256"
    Write-Info "downloading rusted $Tag for $Target"
    Invoke-WebRequest -Uri $BinaryAsset.browser_download_url -OutFile $ExePath -UserAgent "rusted-installer"
    Invoke-WebRequest -Uri $ChecksumAsset.browser_download_url -OutFile $ChecksumPath -UserAgent "rusted-installer"

    $Expected = (Get-Content $ChecksumPath |
        Where-Object { $_ -match "\brusted-$([regex]::Escape($Target))\.exe\b" }) -split "\s+" |
        Select-Object -First 1
    if (-not $Expected) {
        throw "checksum file has no entry for rusted-$Target.exe"
    }
    $Actual = (Get-FileHash -Algorithm SHA256 $ExePath).Hash.ToLowerInvariant()
    if ($Actual -ne $Expected.ToLowerInvariant()) {
        throw "checksum mismatch for rusted-$Target.exe"
    }
    Write-Info "checksum verified"

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $Dest = Join-Path $InstallDir "rusted.exe"
    Move-Item -Force -Path $ExePath -Destination $Dest

    $CurrentUser = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not $CurrentUser) { $CurrentUser = "" }
    $Parts = $CurrentUser -split ";" | Where-Object { $_ }
    if ($Parts -notcontains $InstallDir) {
        $NewPath = ($Parts + $InstallDir) -join ";"
        [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
        Write-Info "added $InstallDir to the user Path"
    }
    if (($env:Path -split ";") -notcontains $InstallDir) {
        $env:Path = "$env:Path;$InstallDir"
    }

    & $Dest --version
    Write-Info "$Tag installed in $Dest"
    Write-Info "open a new terminal and run: rusted --version"
}
finally {
    if ($Temp -and (Test-Path $Temp)) {
        Remove-Item -Recurse -Force $Temp -ErrorAction SilentlyContinue
    }
}
