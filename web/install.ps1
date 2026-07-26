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

$InstallerBuild = '1.1'

function Install-Cloudiy {
  # Public distribution repo (binaries only; source lives in the private repo).
  $Repo = 'w3-surfer/cloudiy-dist'
  $Bin  = 'cloudiy'
  $Dest = if ($env:CLOUDIY_INSTALL_DIR) { $env:CLOUDIY_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Cloudiy' }

  # ---- presentation: brand colour only when the console can render it ------
  # ANSI truecolor on PS7+ or Windows Terminal, unless NO_COLOR is set or output
  # is redirected. Colour never carries meaning alone: OK/ERROR are words too.
  $ESC = [char]27
  $useColor = $false
  try {
    $useColor = (-not $env:NO_COLOR) -and (-not [Console]::IsOutputRedirected) -and `
                (($PSVersionTable.PSVersion.Major -ge 7) -or $env:WT_SESSION)
  } catch { $useColor = $false }
  if ($useColor) {
    $B = "$ESC[38;2;204;255;51m"; $D = "$ESC[2m"; $BOLD = "$ESC[1m"; $RED = "$ESC[1;31m"; $RS = "$ESC[0m"
  } else {
    $B = ''; $D = ''; $BOLD = ''; $RED = ''; $RS = ''
  }
  $width = 80
  try { $width = [Math]::Max(40, [Math]::Min(72, $Host.UI.RawUI.WindowSize.Width)) } catch {}
  $bar = ('-' * $width)

  function Line($s) { Write-Host $s }
  function Rule()   { Write-Host "$D$bar$RS" }
  function Step($s) { Write-Host "  $B>$RS $s" }
  function Ok($s)   { Write-Host "  $B+ OK$RS $s" }
  function Fail($s) { Write-Host "  ${RED}X ERROR$RS $s" }

  # ---- header --------------------------------------------------------------
  Line ''
  Rule
  $art = @'
      ___  _                 _  _
     / __|| | ___  _  _   __| |(_) _  _
    | (__ | |/ _ \| || | / _` || || || |
     \___||_|\___/ \_,_| \__,_||_| \_, |
                                   |__/
'@
  Write-Host "$B$art$RS"
  Write-Host "  ${D}the open compute network -- provider node installer$RS"
  Rule
  Line '  Signal acquired. I will guide the installation.'
  Write-Host "  ${D}Abort any time with Ctrl-C. Nothing is changed until you confirm.$RS"
  Line ''

  # ---- detect the target ---------------------------------------------------
  $arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
  if ($arch -ne 'x86_64') {
    Fail "only x86_64 Windows is prebuilt for now (detected $arch)."
    return
  }
  $target = "$arch-pc-windows-msvc"
  $url = "https://github.com/$Repo/releases/latest/download/$Bin-$target.zip"

  # Best-effort: name the exact release that will be installed (never fatal).
  $version = 'latest'
  try {
    $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -UseBasicParsing -TimeoutSec 6
    if ($rel.tag_name) { $version = $rel.tag_name }
  } catch {}

  Step "Host        Windows / $arch"
  Step "Target      $target"
  Step "Version     $B$version$RS"
  Step "Install to  $Dest\$Bin.exe"
  Step "Installer   build $InstallerBuild"
  Line ''
  Write-Host "  Next: download the verified ${B}cloudiy$RS binary and place it above."

  # Confirm only when a real interactive console is attached (skips in CI / iex
  # pipelines with redirected input, so automation never blocks).
  try {
    if ([Environment]::UserInteractive -and (-not [Console]::IsInputRedirected)) {
      Write-Host "  ${B}Press Enter to install, or Ctrl-C to abort:$RS " -NoNewline
      [void](Read-Host)
    } else {
      Write-Host "  ${D}(non-interactive -- installing without a prompt)$RS"
    }
  } catch {}
  Line ''

  New-Item -ItemType Directory -Force -Path $Dest | Out-Null
  $zip = Join-Path $env:TEMP "cloudiy-$target.zip"

  Step "Downloading  $target ..."
  try {
    Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
  } catch {
    Fail "download failed ($url)."
    Write-Host "          No release yet for this platform? See https://github.com/$Repo/releases"
    return
  }

  # Verify the SHA-256 the release publishes next to the binary (fail closed).
  $shaFile = "$zip.sha256"
  try {
    Invoke-WebRequest -Uri "$url.sha256" -OutFile $shaFile -UseBasicParsing
  } catch {
    Remove-Item $zip -ErrorAction SilentlyContinue
    Fail "could not fetch checksum ($url.sha256) - refusing to install unverified."
    return
  }
  $expected = ((Get-Content $shaFile -Raw).Trim() -split '\s+')[0].ToLower()
  $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
  Remove-Item $shaFile -ErrorAction SilentlyContinue
  if (-not $expected -or $expected -ne $actual) {
    Remove-Item $zip -ErrorAction SilentlyContinue
    Fail "checksum mismatch - refusing to install."
    Write-Host "            expected $expected"
    Write-Host "            actual   $actual"
    return
  }
  Ok "Checksum verified (sha256)"

  Expand-Archive -Path $zip -DestinationPath $Dest -Force
  Remove-Item $zip -ErrorAction SilentlyContinue
  Ok "Installed to $Dest\$Bin.exe"

  # Add to user PATH if missing.
  $onPath = $true
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  if ($userPath -notlike "*$Dest*") {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$Dest", 'User')
    $onPath = $false
  }

  # ---- what happened + the real next step ----------------------------------
  Line ''
  Rule
  Write-Host "  $BOLD${B}Installed.$RS  cloudiy $version -> $Dest\$Bin.exe"
  Rule
  if (-not $onPath) {
    Write-Host "  ${RED}!$RS Added $Dest to your PATH -- restart the terminal to pick it up."
    Line ''
  }
  Write-Host '  Next step -- start earning by offering this machine:'
  Write-Host "      $B$Bin share$RS"
  Write-Host "  ${D}``$Bin share`` walks you through the receiving setup (wallet, price, limits).$RS"
  Write-Host "  See everything it can do:  $Bin --help"
  Line ''
}

Install-Cloudiy
