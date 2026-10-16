# Installs aibrowse from its latest GitHub release into %USERPROFILE%\.aibrowse\bin.
# usage: irm https://github.com/thejens/aibrowse/releases/latest/download/install.ps1 | iex
$ErrorActionPreference = "Stop"
$repo = "thejens/aibrowse"
$dir = Join-Path $env:USERPROFILE ".aibrowse\bin"
$asset = "aibrowse-windows-x64.zip"
$base = "https://github.com/$repo/releases/latest/download"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("aibrowse-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Invoke-WebRequest "$base/$asset" -OutFile (Join-Path $tmp $asset)
    Invoke-WebRequest "$base/SHA256SUMS" -OutFile (Join-Path $tmp "SHA256SUMS")
    $want = (Get-Content (Join-Path $tmp "SHA256SUMS") | Where-Object { $_ -match " $asset$" }).Split(" ")[0]
    $got = (Get-FileHash (Join-Path $tmp $asset) -Algorithm SHA256).Hash.ToLower()
    if ($want -ne $got) { throw "checksum mismatch for $asset" }
    Expand-Archive (Join-Path $tmp $asset) -DestinationPath $tmp
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Copy-Item (Join-Path $tmp "aibrowse\aibrowse.exe") $dir -Force
    Write-Host "installed $dir\aibrowse.exe"
    Write-Host "next: put JEV_API_KEY=... in $env:USERPROFILE\.aibrowse\env, install Google Chrome if missing, then register it with your MCP client (README)"
} finally {
    Remove-Item -Recurse -Force $tmp
}
