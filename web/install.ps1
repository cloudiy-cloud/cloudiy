# Cloudiy provider node installer (Windows). No Rust, no compiler.
#   irm https://cloudiy.cloud/install.ps1 | iex
#
# Rather read it before running? It is short and does nothing surprising:
#   irm https://cloudiy.cloud/install.ps1   # inspect, then pipe to iex
#
# Downloads the prebuilt cloudiy.exe from the latest GitHub Release into
# %LOCALAPPDATA%\Cloudiy and adds it to your user PATH.
#
# The whole installer lives in Install-Cloudiy, invoked only on the LAST line,
# so a truncated download (dropped connection mid-pipe) can't run a half command.
$ErrorActionPreference = 'Stop'

function Install-Cloudiy {
  # Public distribution repo (binaries only; source lives in the private repo).
  $Repo = 'w3-surfer/cloudiy-dist'
  $Bin  = 'cloudiy'
  $Dest = if ($env:CLOUDIY_INSTALL_DIR) { $env:CLOUDIY_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Cloudiy' }

  $arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
  if ($arch -ne 'x86_64') {
    Write-Error "cloudiy: only x86_64 Windows is prebuilt for now (detected $arch)."
    return
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
    return
  }

  # Verify the SHA-256 the release publishes next to the binary (fail closed).
  $shaFile = "$zip.sha256"
  try {
    Invoke-WebRequest -Uri "$url.sha256" -OutFile $shaFile -UseBasicParsing
  } catch {
    Remove-Item $zip -ErrorAction SilentlyContinue
    Write-Error "cloudiy: could not fetch checksum ($url.sha256) - refusing to install unverified."
    return
  }
  $expected = ((Get-Content $shaFile -Raw).Trim() -split '\s+')[0].ToLower()
  $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
  Remove-Item $shaFile -ErrorAction SilentlyContinue
  if (-not $expected -or $expected -ne $actual) {
    Remove-Item $zip -ErrorAction SilentlyContinue
    Write-Error "cloudiy: checksum mismatch - refusing to install. expected=$expected actual=$actual"
    return
  }
  Write-Host "cloudiy: checksum verified (sha256)"

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
}

Install-Cloudiy
