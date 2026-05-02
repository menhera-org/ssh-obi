param(
    [string]$Want = "0.1",
    [switch]$Install
)

$ErrorActionPreference = "Stop"

function Fail($Message) {
    Write-Output "OBI-ERROR $Message"
    exit 1
}

$target = "x86_64-pc-windows-gnu"
$baseUrl = "https://obi.menhera.org"

$arch = $env:PROCESSOR_ARCHITEW6432
if ([string]::IsNullOrEmpty($arch)) {
    $arch = $env:PROCESSOR_ARCHITECTURE
}

if ($arch -ne "AMD64") {
    Fail "unsupported Windows architecture $arch; only x86_64 is published"
}

if ([string]::IsNullOrEmpty($env:USERPROFILE)) {
    Fail "USERPROFILE is not set"
}

if ([string]::IsNullOrEmpty($env:TEMP)) {
    Fail "TEMP is not set"
}

$installRoot = Join-Path $env:USERPROFILE ".ssh-obi"
$binDir = Join-Path $installRoot "bin"
$client = Join-Path $binDir "ssh-obi.exe"
$archiveUrl = "$baseUrl/release-$target.tar.gz"
$tmp = Join-Path $env:TEMP ("ssh-obi-install-{0}-{1}" -f $PID, [Guid]::NewGuid().ToString("N"))
$archive = Join-Path $tmp "release.tar.gz"

try {
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null

    if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) {
        Fail "tar.exe is required to unpack release archives"
    }

    Invoke-WebRequest -UseBasicParsing -Uri $archiveUrl -OutFile $archive

    & tar.exe -xzf $archive -C $tmp
    if ($LASTEXITCODE -ne 0) {
        Fail "failed to unpack release archive"
    }

    $extractedClient = Join-Path $tmp "ssh-obi.exe"
    if (-not (Test-Path -LiteralPath $extractedClient -PathType Leaf)) {
        Fail "release archive does not contain ssh-obi.exe"
    }

    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    Copy-Item -LiteralPath $extractedClient -Destination $client -Force

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ([string]::IsNullOrEmpty($userPath)) {
        [Environment]::SetEnvironmentVariable("Path", $binDir, "User")
    } elseif (($userPath -split ";") -notcontains $binDir) {
        [Environment]::SetEnvironmentVariable("Path", ($userPath.TrimEnd(";") + ";" + $binDir), "User")
    }

    Write-Output "OBI-INSTALL-COMPLETE"
    Write-Output "OBI-PATH $binDir"
    Write-Output "OBI-NOTE restart your terminal if ssh-obi.exe is not found on PATH"
    exit 0
} catch {
    Fail $_.Exception.Message
} finally {
    if (Test-Path -LiteralPath $tmp) {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}
