function Invoke-RegressifyInstall {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    if ($env:OS -ne 'Windows_NT' -and -not $IsWindows) {
        throw 'install.ps1 is for Windows. On macOS or Linux, run: curl -fsSL https://regressify.eshn.dev/install.sh | sh'
    }

    $origin = if ($env:REGRESSIFY_ORIGIN) { $env:REGRESSIFY_ORIGIN } else { 'https://regressify.eshn.dev' }
    if ($origin -cnotmatch '^https://[a-z0-9.-]{1,253}$') {
        throw "Invalid REGRESSIFY_ORIGIN '$origin'. Expected https://host without a path."
    }

    if ($env:PROCESSOR_ARCHITECTURE -eq 'x86' -and -not $env:PROCESSOR_ARCHITEW6432) {
        throw 'Regressify requires 64-bit Windows.'
    }
    Add-Type -AssemblyName System.Net.Http
    function Receive-ReleaseFile([string]$Url, [string]$Destination, [long]$Limit, [bool]$AllowRedirects) {
        $handler = [System.Net.Http.HttpClientHandler]::new()
        $handler.AllowAutoRedirect = $false
        $client = [System.Net.Http.HttpClient]::new($handler)
        $deadline = [System.Threading.CancellationTokenSource]::new([TimeSpan]::FromMinutes(15))
        $client.Timeout = [TimeSpan]::FromSeconds(30)
        try {
            $uri = [uri]$Url
            for ($hop = 0; $hop -lt 5; $hop++) {
                $response = $client.GetAsync($uri, [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead, $deadline.Token).GetAwaiter().GetResult()
                try {
                    if ([int]$response.StatusCode -ge 300 -and [int]$response.StatusCode -lt 400) {
                        if (-not $AllowRedirects -or -not $response.Headers.Location) { throw 'Unexpected download redirect.' }
                        $uri = [uri]::new($uri, $response.Headers.Location)
                        if ($uri.Scheme -cne 'https' -or -not $uri.IsDefaultPort -or $uri.UserInfo -or $uri.Fragment -or
                            $uri.Host -notin @('github.com', 'objects.githubusercontent.com', 'release-assets.githubusercontent.com')) {
                            throw 'Untrusted download redirect.'
                        }
                        continue
                    }
                    if ($response.StatusCode -ne [System.Net.HttpStatusCode]::OK) { throw "Download failed (HTTP $([int]$response.StatusCode))." }
                    if ($response.Content.Headers.ContentLength -gt $Limit) { throw 'Download is larger than declared.' }
                    $inputStream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
                    $outputStream = [System.IO.File]::Create($Destination)
                    try {
                        $buffer = New-Object byte[] 65536
                        $total = 0L
                        while (($count = $inputStream.ReadAsync($buffer, 0, $buffer.Length, $deadline.Token).GetAwaiter().GetResult()) -gt 0) {
                            $total += $count
                            if ($total -gt $Limit) { throw 'Download is larger than declared.' }
                            $outputStream.Write($buffer, 0, $count)
                        }
                    } finally { $inputStream.Dispose(); $outputStream.Dispose() }
                    return
                } finally { $response.Dispose() }
            }
            throw 'Too many download redirects.'
        } finally { $deadline.Dispose(); $client.Dispose(); $handler.Dispose() }
    }

    $tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ('regressify-install-' + [guid]::NewGuid().ToString('N'))
    [System.IO.Directory]::CreateDirectory($tempDir) | Out-Null
    try {
    $manifestPath = Join-Path $tempDir 'latest.txt'
    if ($env:REGRESSIFY_MANIFEST_FILE) {
        if ((Get-Item -LiteralPath $env:REGRESSIFY_MANIFEST_FILE).Length -gt 16384) { throw 'Release manifest exceeds 16 KiB.' }
        Copy-Item -LiteralPath $env:REGRESSIFY_MANIFEST_FILE -Destination $manifestPath
    } else {
        Write-Host "Fetching release manifest from $origin/downloads/latest.txt..."
        Receive-ReleaseFile "$origin/downloads/latest.txt" $manifestPath 16384 $false
    }
    $manifestText = [System.IO.File]::ReadAllText($manifestPath, [System.Text.UTF8Encoding]::new($false, $true))

    $fields = @{}
    foreach ($rawLine in ($manifestText.TrimStart([char]0xFEFF) -split '\r?\n')) {
        $line = $rawLine.Trim()
        if ([string]::IsNullOrEmpty($line)) { continue }
        $separator = $line.IndexOf('=')
        if ($separator -lt 1) { throw 'Invalid line in latest.txt manifest.' }
        $key = $line.Substring(0, $separator).Trim()
        $value = $line.Substring($separator + 1).Trim()
        if ($fields.ContainsKey($key)) { throw "Duplicate field '$key' in latest.txt manifest." }
        $fields[$key] = $value
    }

    $version = $fields['version']
    $installerUrl = $fields['installer']
    $expectedHash = $fields['sha256']
    $expectedSizeText = $fields['size']
    if (-not $version -or -not $installerUrl -or -not $expectedHash -or -not $expectedSizeText) {
        throw 'Release manifest is missing required fields.'
    }
    if ($version -cnotmatch '^[0-9]{1,4}(\.[0-9]{1,4}){3}$') {
        throw "Invalid version '$version' in release manifest."
    }
    $installerPattern = '^https://github\.com/precise-alloy/regressify-rs/releases/download/(v[0-9]{1,4}(?:\.[0-9]{1,4}){2,3})/' + [regex]::Escape("Regressify-Setup-$version-x64.exe") + '$'
    $urlMatch = [regex]::Match($installerUrl, $installerPattern)
    if (-not $urlMatch.Success) {
        throw "Untrusted installer URL '$installerUrl' in release manifest."
    }
    $releaseTag = $urlMatch.Groups[1].Value
    $parts = @($releaseTag.Substring(1).Split('.') | ForEach-Object { [int]$_ })
    if ($parts.Count -eq 3) { $parts += 0 }
    if (($parts -join '.') -cne $version) { throw 'Release tag does not match version.' }
    if ($fields.ContainsKey('tag') -and $fields['tag'] -cne $releaseTag) {
        throw 'Release manifest tag does not match installer URL.'
    }
    if ($expectedHash -notmatch '^[0-9a-fA-F]{64}$') {
        throw 'Invalid installer SHA-256 in release manifest.'
    }
    $expectedSize = 0L
    if (-not [long]::TryParse($expectedSizeText, [ref]$expectedSize) -or $expectedSize -lt 1 -or $expectedSize -gt 1073741824) {
        throw 'Invalid installer size in release manifest.'
    }

        function Get-VerifiedFile([string]$Name, [string]$Url, [long]$Size, [string]$Hash) {
            $target = Join-Path $tempDir $Name
            if ($env:REGRESSIFY_ASSETS_DIR) {
                Copy-Item -LiteralPath (Join-Path $env:REGRESSIFY_ASSETS_DIR $Name) -Destination $target -Force
            } else {
                Write-Host "Downloading $Name..."
                Receive-ReleaseFile $Url $target $Size $true
            }
            $item = Get-Item -LiteralPath $target -ErrorAction Stop
            if ($item.Length -ne $Size) {
                throw "Size mismatch for $Name (expected $Size bytes, got $($item.Length) bytes)."
            }
            $actualHash = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash
            if ($actualHash.ToLowerInvariant() -cne $Hash.ToLowerInvariant()) {
                throw "SHA-256 verification failed for $Name."
            }
            return $target
        }

        $installerFile = "Regressify-Setup-$version-x64.exe"
        $installerPath = Get-VerifiedFile $installerFile $installerUrl $expectedSize $expectedHash

        Write-Host "Running Regressify installer ($version)..."
        $process = Start-Process -FilePath $installerPath -Wait -PassThru
        if ($process.ExitCode -ne 0) {
            throw "Regressify setup exited with code $($process.ExitCode)."
        }
        Write-Host "Regressify $version installed successfully. Open a new terminal to use 'regressify'."
    } finally {
        if (Test-Path -LiteralPath $tempDir) {
            Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

Invoke-RegressifyInstall
