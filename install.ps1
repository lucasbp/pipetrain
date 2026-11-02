#requires -Version 5.1
[CmdletBinding()]
param(
    [string]$Version = $(if ($env:PIPETRAIN_VERSION) { $env:PIPETRAIN_VERSION } else { 'latest' }),
    [string]$InstallDir = $(if ($env:PIPETRAIN_INSTALL_DIR) { $env:PIPETRAIN_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\pipetrain' }),
    [string]$ReleaseBase = $(if ($env:PIPETRAIN_RELEASE_BASE) { $env:PIPETRAIN_RELEASE_BASE } else { 'https://github.com/lucasbp/pipetrain/releases' })
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Stop-Install([string]$Message) {
    [Console]::Error.WriteLine("error: $Message")
    exit 1
}

$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($arch -ne 'AMD64') { Stop-Install "unsupported CPU $arch; only x86_64 Windows has a prebuilt binary" }
$target = 'x86_64-pc-windows-msvc'

if ($Version -eq 'latest') {
    try {
        $request = [System.Net.HttpWebRequest]::Create("$ReleaseBase/latest")
        $request.AllowAutoRedirect = $false
        $response = $request.GetResponse()
        $location = $response.Headers['Location']
        $response.Close()
        $tag = ($location -split '/')[-1]
    } catch [System.Net.WebException] {
        $response = $_.Exception.Response
        if ($null -eq $response) { Stop-Install "could not reach $ReleaseBase/latest" }
        $location = $response.Headers['Location']
        $response.Close()
        $tag = if ($location) { ($location -split '/')[-1] } else { '' }
    }
    if ($tag -notmatch '^v\d') { Stop-Install "could not find the latest release at $ReleaseBase/latest" }
} elseif ($Version.StartsWith('v')) {
    $tag = $Version
} else {
    $tag = "v$Version"
}

$asset = "pipetrain-$tag-$target.zip"
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("pipetrain-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null

try {
    Write-Host "Downloading $asset"
    try {
        Invoke-WebRequest -Uri "$ReleaseBase/download/$tag/$asset" -OutFile (Join-Path $work $asset) -UseBasicParsing
        Invoke-WebRequest -Uri "$ReleaseBase/download/$tag/SHA256SUMS" -OutFile (Join-Path $work 'SHA256SUMS') -UseBasicParsing
    } catch {
        Stop-Install "could not download $asset or SHA256SUMS for $tag from $ReleaseBase"
    }

    $expected = $null
    foreach ($line in Get-Content (Join-Path $work 'SHA256SUMS')) {
        $parts = $line.Trim() -split '\s+', 2
        if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $asset) { $expected = $parts[0].ToLowerInvariant(); break }
    }
    if (-not $expected) { Stop-Install "SHA256SUMS does not list $asset; nothing was installed" }
    $actual = (Get-FileHash -Algorithm SHA256 -Path (Join-Path $work $asset)).Hash.ToLowerInvariant()
    if ($expected -ne $actual) { Stop-Install "SHA-256 mismatch for $asset; nothing was installed" }

    Expand-Archive -Path (Join-Path $work $asset) -DestinationPath (Join-Path $work 'out') -Force
    $exe = Join-Path $work 'out\pipetrain.exe'
    if (-not (Test-Path $exe)) { Stop-Install 'the archive has no pipetrain.exe' }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $installed = Join-Path $InstallDir 'pipetrain.exe'
    $staged = "$installed.new"
    Copy-Item $exe $staged -Force
    if (Test-Path $installed) {
        $old = "$installed.old"
        Remove-Item $old -Force -ErrorAction SilentlyContinue
        Move-Item $installed $old -Force
    }
    Move-Item $staged $installed -Force

    Write-Host "Installed pipetrain $tag to $installed"
    $onPath = ($env:PATH -split ';') -contains $InstallDir
    if (-not $onPath) {
        Write-Host "Add $InstallDir to your PATH to run it: `$env:PATH += ';$InstallDir'"
    }
} finally {
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
}
