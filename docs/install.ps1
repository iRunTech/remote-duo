# Installs Remote Duo Host on Windows 10 or 11:
#   irm https://iruntech.github.io/remote-duo/install.ps1 | iex
#
# Downloads the latest installer, checks it against the release's SHA256SUMS,
# and runs it (Windows asks for permission to install). Works in the
# PowerShell 5.1 that ships with Windows.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # the progress bar makes downloads crawl in 5.1
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$base = 'https://github.com/iRunTech/remote-duo/releases/latest/download'
$file = 'RemoteDuoHost-Windows-Setup.exe'
$dir = Join-Path $env:TEMP ('remote-duo-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $dir | Out-Null

try {
    if (-not [Environment]::Is64BitOperatingSystem) {
        throw 'Remote Duo Host needs 64-bit Windows.'
    }
    Write-Host 'Downloading Remote Duo Host for Windows...' -ForegroundColor Cyan
    $setup = Join-Path $dir $file
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$file" -OutFile $setup
    $sums = (Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS").Content
    if ($sums -is [byte[]]) { $sums = [Text.Encoding]::UTF8.GetString($sums) }

    $line = ($sums -split "`n") | Where-Object { $_ -match "\s\*?$([regex]::Escape($file))\s*$" } | Select-Object -First 1
    if (-not $line) { throw "No checksum listed for $file." }
    $expected = ($line -split '\s+')[0].ToLower()
    $actual = (Get-FileHash -Algorithm SHA256 -Path $setup).Hash.ToLower()
    if ($actual -ne $expected) { throw "Checksum mismatch for $file; try again." }

    Write-Host 'Installing (approve the Windows prompt)...' -ForegroundColor Cyan
    # Not -Wait: that also waits for every process the installer starts, and
    # the installer's last step launches the host, which keeps running.
    $p = Start-Process -FilePath $setup -ArgumentList '/SILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -PassThru
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) { throw "The installer exited with code $($p.ExitCode)." }

    Write-Host ''
    Write-Host 'Remote Duo Host is installed and running in the system tray.' -ForegroundColor Green
    Write-Host 'Open the tray menu for the pairing PIN, then add this PC in the Remote Duo iPhone app.'
}
finally {
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}
