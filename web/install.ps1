# Cloudiy provider node installer (Windows). No Rust, no compiler.
#   irm https://cloudiy.cloud/install.ps1 | iex
#
# Downloads the prebuilt cloudiy.exe from the latest GitHub Release into
# %LOCALAPPDATA%\Cloudiy and adds it to your user PATH.
$ErrorActionPreference = 'Stop'

# Public distribution repo (binaries only; source lives in the private repo).
$Repo = 'w3-surfer/cloudiy-dist'
$Bin  = 'cloudiy'
$Dest = if ($env:CLOUDIY_INSTALL_DIR) { $env:CLOUDIY_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Cloudiy' }

$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
if ($arch -ne 'x86_64') {
  Write-Error "cloudiy: only x86_64 Windows is prebuilt for now (detected $arch)."
  exit 1
}
$target = "$arch-pc-windows-msvc"
$url = "https://github.com/$Repo/releases/latest/download/$Bin-$target.zip"

New-Item -ItemType Directory -Force -Path $Dest | Out-Null
$zip = Join-Path $env:TEMP "cloudiy-$target.zip"

Write-Host "cloudiy: downloading $target..."
try {
  Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
} catch {
  Write-Error "cloudiy: download failed ($url). No release yet? See https://github.com/$Repo/releases"
  exit 1
}

Expand-Archive -Path $zip -DestinationPath $Dest -Force
Remove-Item $zip -ErrorAction SilentlyContinue

# Add to user PATH if missing.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$Dest*") {
  [Environment]::SetEnvironmentVariable('Path', "$userPath;$Dest", 'User')
  Write-Host "cloudiy: added $Dest to your PATH (restart the terminal)."
}

Write-Host "cloudiy: installed to $Dest\$Bin.exe"
Write-Host "cloudiy: get started with  $Bin --help"
