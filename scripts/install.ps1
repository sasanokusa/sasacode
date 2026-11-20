# sasacode installer for Windows (PowerShell 5.1 or later)
#   irm https://sasanokusa.com/sasacode/install.ps1 | iex
#
# The same steps as install.sh: binaries come from GitHub Releases and are checked against the
# release's SHA256SUMS (and, when gh is installed and signed in, the release's signed build provenance)
# before they are put in place. The bash tool needs Git for Windows (Git Bash).
#
# Environment:
#   SASACODE_VERSION       version to install, e.g. v0.11.0 (default: the latest release)
#   SASACODE_INSTALL_DIR   where to put sasacode.exe (default: %LOCALAPPDATA%\Programs\sasacode)
#   SASACODE_DOWNLOAD_BASE releases URL, https only (default: https://github.com/sasanokusa/sasacode/releases)
#   SASACODE_NO_MODIFY_PATH set to leave the user's PATH alone
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue' # Invoke-WebRequest is many times slower with the progress bar
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

# Everything that runs is in Install-Sasacode, called on the last line: a script that arrives only in
# part (a dropped connection under `irm | iex`) defines functions and runs nothing.

$RepoSlug = 'sasanokusa/sasacode'
$DefaultBase = "https://github.com/$RepoSlug/releases"

function Fail($message) {
  Write-Host "sasacode install: $message" -ForegroundColor Red
  exit 1
}

# The checksum comes from the release itself; the signed build provenance says who built the file. It
# is checked when gh can check it (installed and signed in), for the official releases. A failed check
# stops the install; one that cannot be made says so, with how to make it by hand.
function Test-Attestation($file, $base) {
  # A native command's stderr is an error under 'Stop' in Windows PowerShell 5.1: gh's exit code is what counts here.
  $ErrorActionPreference = 'Continue'
  $hint = "gh attestation verify <the downloaded file> --repo $RepoSlug"
  if ($base -ne $DefaultBase) { Write-Host "Build provenance was not checked (not the official releases). By hand: $hint"; return }
  if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { Write-Host "Build provenance was not checked (gh is not installed). By hand: $hint"; return }
  & gh auth status *> $null
  if ($LASTEXITCODE -ne 0) { Write-Host "Build provenance was not checked (gh is not signed in). By hand: $hint"; return }
  $checked = & gh attestation verify $file --repo $RepoSlug 2>&1 | Out-String
  if ($LASTEXITCODE -ne 0) {
    Write-Host $checked
    Fail "build provenance check failed for $(Split-Path $file -Leaf); nothing was installed"
  }
  Write-Host 'Build provenance verified.'
}

function Install-Sasacode {
  $base = if ($env:SASACODE_DOWNLOAD_BASE) { $env:SASACODE_DOWNLOAD_BASE.TrimEnd('/') } else { $DefaultBase }
  if (-not $base.StartsWith('https://')) { Fail 'SASACODE_DOWNLOAD_BASE must be an https:// URL' }
  $installDir = if ($env:SASACODE_INSTALL_DIR) { $env:SASACODE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\sasacode' }
  $version = $env:SASACODE_VERSION

  # -- platform --------------------------------------------------------
  # The OS's architecture, not this PowerShell's (an x86 PowerShell on x64 Windows still gets x64).
  $machine = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
  switch ($machine) {
    'AMD64' { $arch = 'x64' }
    'ARM64' { $arch = 'arm64' }
    default { Fail "unsupported CPU: $machine" }
  }
  $target = "windows-$arch"
  if ($arch -eq 'x64') {
    # CPUs without AVX2 get the baseline build (PF_AVX2_INSTRUCTIONS_AVAILABLE = 40).
    $kernel32 = Add-Type -MemberDefinition '[DllImport("kernel32.dll")] public static extern bool IsProcessorFeaturePresent(int f);' -Name K -Namespace SasacodeCpu -PassThru
    if (-not $kernel32::IsProcessorFeaturePresent(40)) { $target = 'windows-x64-baseline' }
  }

  # -- download --------------------------------------------------------
  # Pin the release once, from the redirect of .../latest/download/ (see install.sh).
  if (-not $version) {
    try {
      $request = [Net.WebRequest]::Create("$base/latest/download/SHA256SUMS")
      $request.Method = 'HEAD'
      $request.AllowAutoRedirect = $false
      $response = $request.GetResponse()
      if ($response.Headers['Location'] -match '/download/([^/]+)/SHA256SUMS') { $version = $Matches[1] }
      $response.Close()
    } catch {}
  }
  $url = if ($version) { "$base/download/$version" } else { "$base/latest/download" }

  $archive = "sasacode-$target.zip"
  $binary = "sasacode-$target.exe"
  $tmp = Join-Path ([IO.Path]::GetTempPath()) ("sasacode-install-" + [Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $tmp | Out-Null
  try {
    $label = if ($version) { $version } else { 'latest' }
    Write-Host "Downloading sasacode $label ($target)..."
    try { Invoke-WebRequest -UseBasicParsing -Uri "$url/$archive" -OutFile (Join-Path $tmp $archive) } catch { Fail "download failed: $url/$archive" }
    try { Invoke-WebRequest -UseBasicParsing -Uri "$url/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS') } catch { Fail "download failed: $url/SHA256SUMS" }

    $line = Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { $_ -match " $([Regex]::Escape($archive))$" } | Select-Object -First 1
    if (-not $line) { Fail "$archive is not listed in SHA256SUMS" }
    $expected = ($line -split '\s+')[0].ToLower()
    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $archive)).Hash.ToLower()
    if ($expected -ne $actual) { Fail "checksum mismatch for $archive" }
    Test-Attestation (Join-Path $tmp $archive) $base

    Expand-Archive -Path (Join-Path $tmp $archive) -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    $exe = Join-Path $installDir 'sasacode.exe'
    # A running sasacode.exe cannot be overwritten, but it can be moved aside.
    if (Test-Path $exe) {
      Remove-Item "$exe.old" -Force -ErrorAction SilentlyContinue
      Move-Item $exe "$exe.old" -Force
    }
    Move-Item (Join-Path $tmp $binary) $exe -Force
  } finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
  }

  $installed = & $exe --version 2>$null
  if (-not $installed) { Fail 'installed binary does not run on this system' }
  Write-Host "Installed sasacode $installed to $exe"

  # On the user's PATH, for new terminals (and for this one).
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  if (-not $env:SASACODE_NO_MODIFY_PATH -and -not (($userPath -split ';') -contains $installDir)) {
    $newPath = if ($userPath) { "$installDir;$userPath" } else { $installDir }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    $env:Path = "$installDir;$env:Path"
    Write-Host "Added $installDir to your PATH (open a new terminal for other windows to see it)."
  }

  if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    Write-Host ''
    Write-Host 'Git for Windows was not found: the bash tool runs commands with its Git Bash.' -ForegroundColor Yellow
    Write-Host '  winget install --id Git.Git -e   (or https://git-scm.com/download/win)'
  }

  Write-Host ''
  Write-Host 'Next: fill in an API key in ~\.sasacode\.env (blank entries are ignored), then run: sasacode'
}

Install-Sasacode
