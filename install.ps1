# Install grace: irm https://raw.githubusercontent.com/prathameshk421/grace/main/install.ps1 | iex
# Env: GRACE_VERSION (for example v1.1.1), GRACE_INSTALL_DIR (default %LOCALAPPDATA%\grace\bin).
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

& {
    $repo = 'prathameshk421/grace'
    $releases = "https://github.com/$repo/releases"

    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    switch ($arch) {
        'AMD64' { }
        'ARM64' { Write-Host 'ARM64 Windows: the x64 build runs under emulation. Using it.' }
        default { throw "Unsupported CPU: $arch. See $releases for other builds." }
    }
    $target = 'x86_64-pc-windows-msvc'

    $version = $env:GRACE_VERSION
    if (-not $version) {
        try {
            $version = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
        } catch {
            throw "Could not read the latest version. Set GRACE_VERSION or see $releases"
        }
    }
    if (-not $version) { throw "Could not read the latest version. Set GRACE_VERSION or see $releases" }

    $name = "grace-$version-$target"
    $base = "$releases/download/$version"
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("grace-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null

    try {
        Write-Host "Downloading grace $version for $target"
        $zip = Join-Path $tmp "$name.zip"
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$name.zip" -OutFile $zip
        $sumText = (Invoke-WebRequest -UseBasicParsing -Uri "$base/$name.zip.sha256").Content
        if ($sumText -is [byte[]]) { $sumText = [Text.Encoding]::ASCII.GetString($sumText) }
        $expected = ($sumText.Trim() -split '\s+')[0].ToLower()
        $actual = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLower()
        if ($expected -ne $actual) { throw "Checksum mismatch for $name.zip" }
        Write-Host 'Checksum OK'

        $out = Join-Path $tmp 'out'
        Expand-Archive -Path $zip -DestinationPath $out -Force
        $exe = Get-ChildItem -Path $out -Filter 'grace.exe' -Recurse | Select-Object -First 1
        if (-not $exe) { throw 'grace.exe not found in the archive' }

        $dir = $env:GRACE_INSTALL_DIR
        if (-not $dir) { $dir = Join-Path $env:LOCALAPPDATA 'grace\bin' }
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        Copy-Item -Path $exe.FullName -Destination (Join-Path $dir 'grace.exe') -Force
        Write-Host "Installed grace $version to $(Join-Path $dir 'grace.exe')"

        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $parts = @()
        if ($userPath) { $parts = $userPath -split ';' | Where-Object { $_ } }
        if (-not ($parts | Where-Object { $_.TrimEnd('\') -ieq $dir.TrimEnd('\') })) {
            $newPath = (@($parts) + $dir) -join ';'
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
            Write-Host "Added $dir to your user PATH. Open a new terminal to use it."
        }
        if (($env:Path -split ';') -notcontains $dir) { $env:Path = "$env:Path;$dir" }

        Write-Host ''
        Write-Host 'Next step: add this line to your PowerShell profile, then open a new terminal.'
        Write-Host '  notepad $PROFILE'
        Write-Host ''
        Write-Host '  Invoke-Expression (& grace init powershell | Out-String)'
    } finally {
        Remove-Item -Recurse -Force -Path $tmp -ErrorAction SilentlyContinue
    }
}
