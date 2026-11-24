# Install grace for Windows from GitHub Releases.
# Env: GRACE_VERSION (for example v2.1.0), GRACE_INSTALL_DIR (default %LOCALAPPDATA%\grace\bin).
$ErrorActionPreference = 'Stop'

$repo = 'prathameshk421/grace'
$releases = "https://github.com/$repo/releases"

# Enforce TLS 1.2+ only.
$proto = [System.Net.SecurityProtocolType]::Tls12
if ([Enum]::GetNames([System.Net.SecurityProtocolType]) -contains 'Tls13') {
  $proto = $proto -bor [System.Net.SecurityProtocolType]::Tls13
}
[System.Net.ServicePointManager]::SecurityProtocol = $proto

function Assert-GitHubHttpsUri {
  param([string]$Url)
  $uri = [Uri]$Url
  if ($uri.Scheme -ne 'https' -or $uri.Host -ne 'github.com') {
    throw "Refusing unexpected download URI: $Url"
  }
}

$arch = ''
try {
  $arch = [string][System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
} catch {
  $arch = $env:PROCESSOR_ARCHITECTURE
}
if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }

$targets = switch ($arch.ToUpperInvariant()) {
  'ARM64' { @('aarch64-pc-windows-msvc', 'x86_64-pc-windows-msvc') }
  default { @('x86_64-pc-windows-msvc') }
}

$version = $env:GRACE_VERSION
if (-not $version) {
  try {
    $req = [System.Net.HttpWebRequest]::Create("$releases/latest")
    $req.Method = 'HEAD'
    $req.AllowAutoRedirect = $false
    $req.Timeout = 15000
    $resp = $req.GetResponse()
    $loc = $resp.Headers['Location']
    $resp.Close()
    if ($loc -match '/releases/tag/([^/?#]+)$') {
      $version = $Matches[1]
    }
  } catch [System.Net.WebException] {
    $r = $_.Exception.Response
    if ($r) {
      $loc = $r.Headers['Location']
      $r.Close()
      if ($loc -match '/releases/tag/([^/?#]+)$') {
        $version = $Matches[1]
      }
    }
  }
  if (-not $version) {
    $rel = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest"
    $version = $rel.tag_name
  }
  if (-not $version) {
    throw "Could not read the latest version. Set GRACE_VERSION or see $releases"
  }
}

if ($version -notmatch '^v\d+\.\d+\.\d+([.-][A-Za-z0-9.+-]+)?$') {
  throw "Invalid GRACE_VERSION format: $version"
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("grace-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
try {
  $chosenTarget = $null
  $name = $null
  $zipPath = $null
  $shaPath = $null

  foreach ($t in $targets) {
    $candidateName = "grace-$version-$t"
    $zipUrl = "$releases/download/$version/$candidateName.zip"
    $shaUrl = "$zipUrl.sha256"
    Assert-GitHubHttpsUri $zipUrl
    Assert-GitHubHttpsUri $shaUrl
    $zp = Join-Path $tmp "$candidateName.zip"
    $sp = Join-Path $tmp "$candidateName.zip.sha256"
    try {
      Invoke-WebRequest -Uri $zipUrl -OutFile $zp -UseBasicParsing
      Invoke-WebRequest -Uri $shaUrl -OutFile $sp -UseBasicParsing
      $chosenTarget = $t
      $name = $candidateName
      $zipPath = $zp
      $shaPath = $sp
      break
    } catch {
      if ($t -eq $targets[-1]) { throw }
    }
  }

  $expected = ((Get-Content -Raw -LiteralPath $shaPath).Trim() -split '\s+')[0].ToLowerInvariant()
  if ($expected -notmatch '^[0-9a-f]{64}$') {
    throw "Malformed SHA-256 checksum file ($shaPath)"
  }
  $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $zipPath).Hash.ToLowerInvariant()
  if ($actual -ne $expected) {
    throw "SHA-256 mismatch for $name.zip (expected $expected, got $actual)"
  }

  Add-Type -AssemblyName System.IO.Compression.FileSystem
  $extractDir = Join-Path $tmp 'extract'
  New-Item -ItemType Directory -Force -Path $extractDir | Out-Null
  $canonRoot = [System.IO.Path]::GetFullPath($extractDir + [System.IO.Path]::DirectorySeparatorChar)
  $stagedExe = $null

  $zip = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
  try {
    foreach ($entry in $zip.Entries) {
      if ($entry.FullName.EndsWith('/')) { continue }
      $destPath = [System.IO.Path]::GetFullPath((Join-Path $extractDir $entry.FullName))
      if (-not $destPath.StartsWith($canonRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Blocked unsafe archive entry path: $($entry.FullName)"
      }
      if ($entry.Name -ceq 'grace.exe') {
        $parent = Split-Path -Parent $destPath
        if (-not (Test-Path -LiteralPath $parent)) {
          New-Item -ItemType Directory -Force -Path $parent | Out-Null
        }
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $destPath, $true)
        $stagedExe = $destPath
        break
      }
    }
  } finally {
    $zip.Dispose()
  }

  if (-not $stagedExe -or -not (Test-Path -LiteralPath $stagedExe)) {
    throw "Archive did not contain grace.exe"
  }

  $dir = $env:GRACE_INSTALL_DIR
  if (-not $dir) { $dir = Join-Path $env:LOCALAPPDATA 'grace\bin' }
  New-Item -ItemType Directory -Force -Path $dir | Out-Null

  $dst = Join-Path $dir 'grace.exe'
  $old = Join-Path $dir 'grace.exe.old'
  if (Test-Path -LiteralPath $old) {
    Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
  }
  if (Test-Path -LiteralPath $dst) {
    Move-Item -LiteralPath $dst -Destination $old -Force
  }
  Move-Item -LiteralPath $stagedExe -Destination $dst -Force
  Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue

  # Update current user PATH while preserving REG_EXPAND_SZ if already set.
  $envKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
  if ($envKey) {
    try {
      $rawPath = $envKey.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
      $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
      try { $kind = $envKey.GetValueKind('Path') } catch { }
      $parts = @($rawPath -split ';' | Where-Object { $_ -ne '' })
      if (-not ($parts | Where-Object { $_.TrimEnd('\') -ieq $dir.TrimEnd('\') })) {
        $newPath = if ($rawPath) { "$rawPath;$dir" } else { $dir }
        $envKey.SetValue('Path', $newPath, $kind)
      }
    } finally {
      $envKey.Close()
    }
  }
  if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $dir.TrimEnd('\') })) {
    $env:Path = "$env:Path;$dir"
  }

  Write-Host ""
  Write-Host "  * grace $version installed to $dst ($chosenTarget)" -ForegroundColor Green
  Write-Host ""
  Write-Host "  1. Enable shell integration in PowerShell (`$PROFILE):"
  Write-Host "       New-Item -ItemType File -Force -Path `$PROFILE"
  Write-Host "       Add-Content `$PROFILE 'Invoke-Expression (& grace init powershell | Out-String)'"
  Write-Host ""
  Write-Host "  2. Connect CLI agents and verify health:"
  Write-Host "       grace setup --all"
  Write-Host "       grace doctor"
  Write-Host ""
} finally {
  if (Test-Path -LiteralPath $tmp) {
    Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
  }
}
