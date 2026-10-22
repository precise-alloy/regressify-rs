param(
    [Parameter(Mandatory)][string]$Version,
    [string]$ReleaseTag,
    [string]$Target = 'x86_64-pc-windows-msvc',
    [string]$BinaryDirectory,
    [string]$OutputDirectory,
    [string]$Compiler,
    [switch]$SmokeTest,
    [switch]$RequireStaticRuntime
)

$ErrorActionPreference = 'Stop'
if ($Version -cnotmatch '^[0-9]{1,4}(\.[0-9]{1,4}){3}$') {
    throw 'Version must be canonical N.N.N.N, with each component 0-9999.'
}
if (([version]$Version).ToString() -cne $Version) { throw 'Version must not contain leading zeros.' }
$releaseTagValue = if ($ReleaseTag) { $ReleaseTag } else { "v$Version" }
if ($releaseTagValue -cnotmatch '^v[0-9]{1,4}(\.[0-9]{1,4}){2,3}$') { throw 'ReleaseTag must be vN.N.N or vN.N.N.N.' }
$tagParts = @($releaseTagValue.Substring(1).Split('.') | ForEach-Object { [int]$_ })
if ($tagParts.Count -eq 3) { $tagParts += 0 }
if (($tagParts -join '.') -cne $Version) { throw 'ReleaseTag must match the installer version.' }
$root = Split-Path $PSScriptRoot -Parent
if (-not $BinaryDirectory) { $BinaryDirectory = Join-Path $root "target/$Target/release" }
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $root 'target/installer' }
$binary = Get-Item -LiteralPath (Join-Path $BinaryDirectory 'regressify-desktop.exe')
$cliBinary = Get-Item -LiteralPath (Join-Path $BinaryDirectory 'regressify.exe')
if ($binary.PSIsContainer -or $cliBinary.PSIsContainer) { throw 'Both installer payloads must be executable files.' }
if ([version]$binary.VersionInfo.FileVersion -ne [version]$Version) {
    throw "Desktop executable version $($binary.VersionInfo.FileVersion) does not match installer version $Version."
}
if ($RequireStaticRuntime) {
    $locator = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $dumpbinPaths = @(& $locator -latest -products '*' -find 'VC/Tools/MSVC/**/bin/Hostx64/x64/dumpbin.exe')
    if ($LASTEXITCODE -ne 0) { throw "dumpbin discovery failed ($LASTEXITCODE)." }
    $dumpbin = $dumpbinPaths | Select-Object -First 1
    if (-not $dumpbin) { throw 'dumpbin.exe is required to verify the installer runtime dependencies.' }
    foreach ($payload in @($binary, $cliBinary)) {
        $dependencies = & $dumpbin /DEPENDENTS $payload.FullName
        if ($LASTEXITCODE -ne 0) { throw "Cannot inspect $($payload.Name) dependencies." }
        if ($dependencies -match '(?i)(vcruntime|msvcp)[0-9].*\.dll') {
            throw "$($payload.Name) must use the static MSVC runtime."
        }
    }
}
if (-not $Compiler) {
    $command = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    $Compiler = if ($command) { $command.Source } else { "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
}
if (-not (Test-Path -LiteralPath $Compiler)) { throw 'Install Inno Setup 6 or pass -Compiler with the ISCC.exe path.' }
$source = $binary.Directory.FullName
$output = [System.IO.Path]::GetFullPath($OutputDirectory)
$licenseInputs = @((Join-Path $root 'LICENSE'), (Join-Path $root 'LICENSING.md'))
foreach ($spec in @(
    @{ Archive = 'regressify-desktop-x86_64-pc-windows-msvc.zip'; Payload = $binary },
    @{ Archive = 'regressify-cli-x86_64-pc-windows-msvc.zip'; Payload = $cliBinary }
)) {
    $archivePath = Join-Path $source $spec.Archive
    if (-not (Test-Path -LiteralPath $archivePath -PathType Leaf)) {
        if ($RequireStaticRuntime) { throw "Release archive is missing: $($spec.Archive)" }
        Compress-Archive -LiteralPath (@($spec.Payload.FullName) + $licenseInputs) -DestinationPath $archivePath -Force
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $inputs = @($spec.Payload.FullName) + $licenseInputs
        if ($archive.Entries.Count -ne $inputs.Count) { throw "Unexpected files in $($spec.Archive)." }
        foreach ($inputPath in $inputs) {
            $inputFile = Get-Item -LiteralPath $inputPath
            $entries = @($archive.Entries | Where-Object { $_.FullName -ceq $inputFile.Name })
            if ($entries.Count -ne 1 -or $entries[0].Length -ne $inputFile.Length) { throw "Invalid archive entry: $($inputFile.Name)" }
            $stream = $entries[0].Open()
            $hash = [System.Security.Cryptography.SHA256]::Create()
            try {
                $actual = [BitConverter]::ToString($hash.ComputeHash($stream)).Replace('-', '')
                if ($actual -cne (Get-FileHash -LiteralPath $inputPath -Algorithm SHA256).Hash) {
                    throw "Archive payload does not match verified build: $($inputFile.Name)"
                }
            } finally { $stream.Dispose(); $hash.Dispose() }
        }
    } finally { $archive.Dispose() }
}
& $Compiler (Join-Path $root 'installer/regressify.iss') "/DAppVersion=$Version" "/DReleaseTag=$releaseTagValue" "/DSourceDir=$source" "/DInstallerOutput=$output"
if ($LASTEXITCODE -ne 0) { throw "Installer compilation failed ($LASTEXITCODE)." }
$installer = Join-Path $output "Regressify-Setup-$Version-x64.exe"
if (-not (Test-Path -LiteralPath $installer)) { throw 'The compiler did not create the expected installer.' }
if ($SmokeTest) {
    $registration = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{ED6F0908-7E11-4608-A532-E43482AA117B}_is1'
    if (Test-Path -LiteralPath $registration) { throw 'Installer smoke tests will not modify an existing Regressify installation.' }
    $pathOwner = 'HKCU:\Software\Regressify\DesktopInstaller'
    $desktopShortcut = Join-Path ([Environment]::GetFolderPath('DesktopDirectory')) 'Regressify Desktop.lnk'
    $programGroup = Join-Path ([Environment]::GetFolderPath('Programs')) 'Regressify'
    $legacyProgramGroup = Join-Path ([Environment]::GetFolderPath('Programs')) 'Regressify Desktop'
    $startShortcut = Join-Path $programGroup 'Regressify Desktop.lnk'
    $contextMenus = @(
        @{ Path = 'HKCU:\Software\Classes\Directory\shell\RegressifyDesktop'; Argument = '%1' },
        @{ Path = 'HKCU:\Software\Classes\Directory\Background\shell\RegressifyDesktop'; Argument = '%V' }
    )
    foreach ($path in @($desktopShortcut, $programGroup, $legacyProgramGroup, $pathOwner) + @($contextMenus.Path)) {
        if (Test-Path -LiteralPath $path) { throw "Installer smoke tests will not modify existing shell integration: $path" }
    }
    function Get-UserPathState {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
        try {
            $value = if ($key) { $key.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) } else { $null }
            $kind = if ($null -ne $value) { $key.GetValueKind('Path') } else { $null }
            return [pscustomobject]@{ Value = $value; Kind = $kind }
        } finally {
            if ($key) { $key.Dispose() }
        }
    }
    $originalUserPath = Get-UserPathState
    $originalMachinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $fixture = Join-Path $root ('target/installer-smoke-' + [guid]::NewGuid().ToString('N'))
    $destination = Join-Path $fixture 'app'
    $prepareDir = Join-Path $fixture 'prepared'
    $expectedUserPath = if ([string]::IsNullOrEmpty($originalUserPath.Value)) { $destination } else { $originalUserPath.Value + ';' + $destination }
    $expectedPathKind = if ($null -eq $originalUserPath.Kind) { [Microsoft.Win32.RegistryValueKind]::ExpandString } else { $originalUserPath.Kind }
    [System.IO.Directory]::CreateDirectory($fixture) | Out-Null
    $arguments = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/SP-', '/NORESTART', '/NORELAUNCH=1', ('/DIR="' + $destination + '"'), ('/SOURCE="' + $prepareDir + '"'))
    $uninstaller = Join-Path $destination 'unins000.exe'
    $installed = Join-Path $destination 'regressify-desktop.exe'
    $installedCli = Join-Path $destination 'regressify.exe'
    $licenseFiles = @('LICENSE', 'LICENSING.md')
    $originalHash = (Get-FileHash -LiteralPath $binary.FullName -Algorithm SHA256).Hash
    $originalCliHash = (Get-FileHash -LiteralPath $cliBinary.FullName -Algorithm SHA256).Hash
    function Assert-InstallerCliPath {
        $current = Get-UserPathState
        if ($current.Value -cne $expectedUserPath -or $current.Kind -ne $expectedPathKind) {
            throw 'Installer must append its directory exactly once without changing existing user PATH data or type.'
        }
        if ([Environment]::GetEnvironmentVariable('Path', 'Machine') -cne $originalMachinePath) {
            throw 'Installer modified the machine PATH.'
        }
        $start = [System.Diagnostics.ProcessStartInfo]::new((Join-Path ([Environment]::SystemDirectory) 'cmd.exe'))
        $start.UseShellExecute = $false
        $start.CreateNoWindow = $true
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.WorkingDirectory = $fixture
        $start.Arguments = '/d /s /c "where.exe regressify && regressify --version"'
        $start.Environment['PATH'] = [Environment]::ExpandEnvironmentVariables($originalMachinePath + ';' + $current.Value)
        $process = [System.Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            if (-not $process.WaitForExit(15000)) {
                $process.Kill($true)
                throw 'CLI lookup through the installed PATH timed out.'
            }
            $output = $stdout.GetAwaiter().GetResult()
            $null = $stderr.GetAwaiter().GetResult()
            if ($process.ExitCode -ne 0 -or ($output -split '\r?\n') -inotcontains $installedCli) {
                throw 'A fresh process could not find and run the installed CLI through PATH.'
            }
        } finally { $process.Dispose() }
    }
    function Invoke-InstallerFixture([string]$Path, [string[]]$Parameters, [int]$TimeoutMilliseconds = 120000) {
        $runner = [PowerShell]::Create()
        $output = [System.Management.Automation.PSDataCollection[psobject]]::new()
        $environment = @{ HTTP_PROXY = 'http://127.0.0.1:9'; HTTPS_PROXY = 'http://127.0.0.1:9'; ALL_PROXY = 'http://127.0.0.1:9'; NO_PROXY = '' }
        # Inno's launcher exits before its second phase finishes. -Wait tracks descendants too.
        $null = $runner.AddCommand('Start-Process').AddParameter('FilePath', $Path).
            AddParameter('ArgumentList', ($Parameters -join ' ')).AddParameter('Environment', $environment).
            AddParameter('NoNewWindow').AddParameter('PassThru').AddParameter('Wait').AddParameter('ErrorAction', 'Stop')
        $pending = $runner.BeginInvoke[object, psobject]($null, $output)
        try {
            if (-not $pending.AsyncWaitHandle.WaitOne($TimeoutMilliseconds)) {
                foreach ($process in $output) {
                    # The first phase may already have exited, so kill its surviving second phase explicitly.
                    foreach ($child in Get-CimInstance Win32_Process -Filter "ParentProcessId = $($process.Id)" -OperationTimeoutSec 5) {
                        $childProcess = Get-Process -Id $child.ProcessId -ErrorAction SilentlyContinue
                        if ($childProcess) {
                            try {
                                $childProcess.Kill($true)
                                if (-not $childProcess.WaitForExit(5000)) { throw 'Installer child did not stop after timeout.' }
                            } catch [System.InvalidOperationException] {} finally { $childProcess.Dispose() }
                        }
                    }
                    if (-not $process.HasExited) {
                        $process.Kill($true)
                        if (-not $process.WaitForExit(5000)) { throw 'Installer did not stop after timeout.' }
                    }
                }
                $runner.Stop()
                throw 'Installer fixture timed out.'
            }
            $null = $runner.EndInvoke($pending)
            if ($runner.HadErrors) { throw $runner.Streams.Error[0] }
            if ($output.Count -ne 1) { throw 'Installer fixture did not return a process.' }
            return $output[0].ExitCode
        } finally {
            if (-not $pending.IsCompleted) { $runner.Stop() }
            $runner.Dispose()
            foreach ($process in $output) { $process.Dispose() }
            $output.Dispose()
            $pending.AsyncWaitHandle.Dispose()
        }
    }
    function Assert-ChromiumPreparation([string]$LogPath, [bool]$ProgressVisible) {
        $text = Get-Content -LiteralPath $LogPath -Raw
        if ([regex]::Matches($text, 'Preparing shared Chromium cache before launching Regressify Desktop\.').Count -ne 1) {
            throw 'Setup must attempt Chromium preparation exactly once.'
        }
        if ($text -notmatch 'Chromium: Checking Chromium cache') { throw 'Setup did not invoke the headless Chromium helper.' }
        if (($text -match 'Chromium progress: Checking Chromium cache') -ne $ProgressVisible) {
            throw 'Chromium progress must update during visible setup and stay hidden during fully silent setup.'
        }
        if ($text -match 'Regressify setup is running') { throw 'Chromium preparation incorrectly hit the app startup guard.' }
        if ($text -match 'Chromium preparation completed successfully\.') {
            if ($text -notmatch 'Reusing cached Chromium; no download needed' -or $text -match 'Chromium download URL:') {
                throw 'Offline installer smoke must only succeed by reusing cached Chromium.'
            }
        } elseif ($text -notmatch 'Chromium preparation failed' -or $text -notmatch 'Cannot connect to the Chromium download server') {
            throw 'Setup did not retain a recoverable Chromium network failure in its log.'
        }
    }
    function Assert-InstallerShellIntegration {
        $shortcuts = New-Object -ComObject WScript.Shell
        $shell = New-Object -ComObject Shell.Application
        foreach ($path in @($desktopShortcut, $startShortcut)) {
            if (-not (Test-Path -LiteralPath $path)) { throw "Installer did not create shortcut: $path" }
            $shortcut = $shortcuts.CreateShortcut($path)
            if ($shortcut.TargetPath -ne $installed -or $shortcut.Arguments -ne '') {
                throw "Shortcut does not launch the installed desktop app: $path"
            }
            $item = $shell.NameSpace((Split-Path $path -Parent)).ParseName((Split-Path $path -Leaf))
            if ($item.ExtendedProperty('System.AppUserModel.ID') -cne 'Regressify.Desktop') {
                throw "Shortcut does not share the desktop app's Jump List identity: $path"
            }
        }
        foreach ($entry in $contextMenus) {
            $key = Get-Item -LiteralPath $entry.Path
            if ($key.GetValue('') -cne 'Open with Regressify Desktop') { throw 'Folder context-menu label is incorrect.' }
            if ($key.GetValue('Icon') -ne ('"' + $installed + '",0')) { throw 'Folder context-menu icon is incorrect.' }
            $command = (Get-Item -LiteralPath (Join-Path $entry.Path 'command')).GetValue('')
            $expected = '"' + $installed + '" "' + $entry.Argument + '\."'
            if ($command -cne $expected) { throw "Folder context-menu command is incorrect: $command" }
        }
    }
    try {
        $prepareLog = Join-Path $fixture 'prepare.log'
        $runningForPrepare = [System.Threading.Mutex]::new($false, 'Local\Regressify.Desktop.Running.ED6F0908-7E11-4608-A532-E43482AA117B')
        try {
            $null = Invoke-InstallerFixture $installer @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/SP-', '/NORESTART', ('/PREPARE="' + $prepareDir + '"'), ('/SOURCE="' + $source + '"'), ('/LOG="' + $prepareLog + '"'))
        } finally { $runningForPrepare.Dispose() }
        if (-not (Test-Path -LiteralPath (Join-Path $prepareDir 'prepared.txt') -PathType Leaf)) {
            throw "Background payload preparation failed. See $prepareLog."
        }
        $installLog = Join-Path $fixture 'install.log'
        $foregroundPattern = 'Installer brought to foreground after running-app checks\.|Windows denied installer foreground activation\.'
        $result = Invoke-InstallerFixture $installer ($arguments + ('/LOG="' + $installLog + '"'))
        if ($result -ne 0) { throw "Fresh installation failed ($result). See $fixture." }
        if ((Get-Content -LiteralPath $installLog -Raw) -match $foregroundPattern) { throw 'Fully silent installation attempted foreground activation.' }
        Assert-ChromiumPreparation $installLog $false
        if ((Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -ne $originalHash) { throw 'Installed executable differs from the built payload.' }
        if ((Get-FileHash -LiteralPath $installedCli -Algorithm SHA256).Hash -ne $originalCliHash) { throw 'Installed CLI differs from the built payload.' }
        foreach ($name in $licenseFiles) {
            $installedLicense = Join-Path $destination $name
            if (-not (Test-Path -LiteralPath $installedLicense -PathType Leaf) -or
                (Get-FileHash -LiteralPath $installedLicense -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath (Join-Path $root $name) -Algorithm SHA256).Hash) {
                throw "Installer did not install the repository $name."
            }
        }
        & $installedCli --version
        if ($LASTEXITCODE -ne 0) { throw 'Installed CLI failed to run.' }
        Assert-InstallerCliPath
        Assert-InstallerShellIntegration
        $sentinel = Join-Path $destination 'user-workspace.txt'
        [System.IO.File]::WriteAllText($sentinel, 'User data must survive updates and uninstall.')
        $setupMutex = [System.Threading.Mutex]::new($false, 'Local\Regressify.Desktop.Setup.ED6F0908-7E11-4608-A532-E43482AA117B')
        try {
            $workerInput = Join-Path $fixture 'worker-input.json'
            $workerError = Join-Path $fixture 'worker-error.log'
            [System.IO.File]::WriteAllText($workerInput, '{}')
            $worker = Start-Process -FilePath $installed -ArgumentList '--internal-worker' -RedirectStandardInput $workerInput -RedirectStandardError $workerError -RedirectStandardOutput (Join-Path $fixture 'worker-output.log') -PassThru
            if (-not $worker.WaitForExit(15000)) {
                $worker.Kill($true)
                throw 'Worker did not reject startup during setup.'
            }
            if ($worker.ExitCode -eq 0 -or (Get-Content -LiteralPath $workerError -Raw) -notmatch 'Regressify setup is running') {
                throw 'Worker bypassed the setup guard.'
            }
        } finally { $setupMutex.Dispose() }
        $running = [System.Threading.Mutex]::new($false, 'Local\Regressify.Desktop.Running.ED6F0908-7E11-4608-A532-E43482AA117B')
        try {
            if ((Invoke-InstallerFixture $installer $arguments) -eq 0) { throw 'Installer ignored the running application mutex.' }
        } finally { $running.Dispose() }
        $legacyDir = Join-Path $fixture 'Regressify Desktop'
        [System.IO.Directory]::CreateDirectory($legacyDir) | Out-Null
        foreach ($legacyFile in @('regressify-desktop.exe', 'regressify.exe', 'LICENSE', 'LICENSING.md', 'unins000.exe', 'unins000.dat', 'unins000.msg')) {
            [System.IO.File]::WriteAllText((Join-Path $legacyDir $legacyFile), 'legacy')
        }
        [System.IO.Directory]::CreateDirectory($legacyProgramGroup) | Out-Null
        [System.IO.File]::WriteAllText((Join-Path $legacyProgramGroup 'Regressify Desktop.lnk'), 'legacy')
        Set-ItemProperty -LiteralPath $registration -Name 'Inno Setup: App Path' -Value $legacyDir
        Set-ItemProperty -LiteralPath $registration -Name 'Inno Setup: Icon Group' -Value 'Regressify Desktop'
        $foregroundLog = Join-Path $fixture 'foreground.log'
        $updateArguments = @($arguments | Where-Object { $_ -ne '/VERYSILENT' }) + @('/SILENT', ('/LOG="' + $foregroundLog + '"'))
        if ((Invoke-InstallerFixture $installer $updateArguments) -ne 0) { throw 'Visible update reinstallation failed.' }
        if (Test-Path -LiteralPath $legacyDir) { throw 'Upgrade did not clean up the legacy Regressify Desktop install directory.' }
        if (Test-Path -LiteralPath $legacyProgramGroup) { throw 'Upgrade did not clean up the legacy Regressify Desktop program group.' }
        Assert-ChromiumPreparation $foregroundLog $true
        $foregroundResult = Get-Content -LiteralPath $foregroundLog -Raw
        if ($foregroundResult -notmatch $foregroundPattern) { throw 'Visible update did not request foreground activation after the app closed.' }
        if ($foregroundResult -match 'Windows denied installer foreground activation\.') {
            Write-Warning 'Windows denied foreground activation on this runner; the visible update requested it.'
        }
        Assert-InstallerShellIntegration
        Assert-InstallerCliPath
        if (-not (Test-Path -LiteralPath $sentinel)) { throw 'Reinstallation removed user data.' }
        $skipLog = Join-Path $fixture 'skip-chromium.log'
        if ((Invoke-InstallerFixture $installer ($arguments + @('/SKIPCHROMIUM=1', ('/LOG="' + $skipLog + '"')))) -ne 0) {
            throw 'Installation with deferred Chromium preparation failed.'
        }
        $skipOutput = Get-Content -LiteralPath $skipLog -Raw
        if ($skipOutput -notmatch 'Chromium preparation skipped' -or $skipOutput -match 'Chromium: Checking Chromium cache') {
            throw 'Explicit Chromium skip did not leave preparation to Reference/Test.'
        }
        $parts = @($Version.Split('.') | ForEach-Object { [int]$_ })
        for ($index = 3; $index -ge 0; $index--) {
            if ($parts[$index] -gt 0) {
                $parts[$index]--
                $older = $parts -join '.'
                & $Compiler /Qp (Join-Path $root 'installer/regressify.iss') "/DAppVersion=$older" "/DSourceDir=$source" "/DInstallerOutput=$fixture"
                if ($LASTEXITCODE -ne 0) { throw 'Cannot compile downgrade fixture.' }
                $olderInstaller = Join-Path $fixture "Regressify-Setup-$older-x64.exe"
                $result = Invoke-InstallerFixture $olderInstaller ($arguments + ('/LOG="' + (Join-Path $fixture 'downgrade.log') + '"'))
                if ($result -ne 7) { throw "Downgrade must fail before copying files (got $result)." }
                break
            }
        }
        if ((Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -ne $originalHash) { throw 'An unsuccessful installation changed the executable.' }
        if ((Get-FileHash -LiteralPath $installedCli -Algorithm SHA256).Hash -ne $originalCliHash) { throw 'An unsuccessful installation changed the CLI.' }
        Assert-InstallerCliPath
        Write-Output 'Fresh install, CLI lookup through user PATH, Chromium cache reuse/network fallback/skip, visible update foreground handoff, fully silent setup, shortcuts, folder context menus, running-app protection and downgrade protection passed.'
    } finally {
        if (Test-Path -LiteralPath $uninstaller) {
            $uninstallLog = Join-Path $fixture 'uninstall.log'
            $result = Invoke-InstallerFixture $uninstaller @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', ('/LOG="' + $uninstallLog + '"'))
            if ($result -ne 0) { throw "Fixture uninstall failed ($result). See $fixture." }
            if (Test-Path -LiteralPath $installed) { throw 'Uninstall left the installed executable behind.' }
            if (Test-Path -LiteralPath $installedCli) { throw 'Uninstall left the installed CLI behind.' }
            foreach ($name in $licenseFiles) {
                if (Test-Path -LiteralPath (Join-Path $destination $name)) { throw "Uninstall left $name behind." }
            }
            if (Test-Path -LiteralPath $registration) { throw 'Uninstall left the fixture registration behind.' }
            if (Test-Path -LiteralPath $pathOwner) { throw 'Uninstall left PATH ownership metadata behind.' }
            $remainingPath = Get-UserPathState
            if ($remainingPath.Value -cne $originalUserPath.Value -or $remainingPath.Kind -ne $originalUserPath.Kind) {
                throw 'Uninstall did not preserve the original user PATH data and type.'
            }
            if ([Environment]::GetEnvironmentVariable('Path', 'Machine') -cne $originalMachinePath) {
                throw 'Uninstall modified the machine PATH.'
            }
            foreach ($path in @($desktopShortcut, $startShortcut) + @($contextMenus.Path)) {
                if (Test-Path -LiteralPath $path) { throw "Uninstall left shell integration behind: $path" }
            }
            if ($sentinel -and -not (Test-Path -LiteralPath $sentinel)) { throw 'Uninstall removed user data.' }
            Write-Output 'Uninstall removed both executables, its PATH entry, shortcuts and context menus; user data and unrelated PATH entries were preserved.'
        }
    }
}
if ($env:GITHUB_OUTPUT) { "installer=$installer" | Out-File -LiteralPath $env:GITHUB_OUTPUT -Encoding utf8 -Append }
Write-Output $installer