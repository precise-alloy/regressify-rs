function Write-JsonFixture([string]$Path, $Value) {
    [System.IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
    [System.IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 20), [System.Text.UTF8Encoding]::new($false))
}

function Write-TestImage([string]$Path, [System.Drawing.Color]$Color) {
    [System.IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
    $bitmap = [System.Drawing.Bitmap]::new(640, 420)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $brush = [System.Drawing.SolidBrush]::new($Color)
    $font = [System.Drawing.Font]::new('Segoe UI', 28)
    try {
        $graphics.Clear([System.Drawing.Color]::White)
        $graphics.FillRectangle($brush, 30, 30, 580, 80)
        $graphics.DrawString('Checkout fixture', $font, [System.Drawing.Brushes]::White, 50, 45)
        $graphics.DrawString('Order total: $42.00', $font, [System.Drawing.Brushes]::Black, 40, 160)
        $graphics.FillRectangle($brush, 40, 270, 230, 65)
        $graphics.DrawString('Continue', $font, [System.Drawing.Brushes]::White, 50, 280)
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $font.Dispose()
        $brush.Dispose()
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

function Start-Fixture([string]$Root = $workspace, [string]$Name = 'native', [switch]$Empty) {
    if (-not $Empty -and -not $GitFlows -and -not (Test-Path (Join-Path $Root '.git'))) {
        & git -C $Root init -q
        if ($LASTEXITCODE -ne 0) { throw 'Cannot isolate native fixture Git repository.' }
    }
    $errorLog = Join-Path $artifacts ($Name + '-stderr.log')
    $start = @{ FilePath = $executable; RedirectStandardError = $errorLog; RedirectStandardOutput = (Join-Path $artifacts ($Name + '-stdout.log')); PassThru = $true }
    if (-not $Empty) { $start.ArgumentList = '"' + $Root + '"' }
    $script:process = Start-Process @start
    $script:fixtureProcesses.Add($process)
    if (-not $process.WaitForInputIdle(15000)) { throw 'Desktop did not initialize.' }
    $initialization = [System.Diagnostics.Stopwatch]::StartNew()
    do {
        $process.Refresh()
        $script:window = $process.MainWindowHandle
        if ($script:window -ne [IntPtr]::Zero -or $process.HasExited) { break }
        [RegressifySmokeNative]::DwmFlush() | Out-Null
    } while ($initialization.Elapsed.TotalSeconds -lt 15)
    if ($script:window -eq [IntPtr]::Zero) { throw "Desktop has no native window. $(Get-Content $errorLog -Raw)" }
    $script:screen = [System.Windows.Forms.Screen]::FromHandle($script:window).WorkingArea
    [RegressifySmokeNative]::SetWindowPos($script:window, [IntPtr]::Zero, $screen.Left + 20, $screen.Top + 20, [Math]::Min(1600, $screen.Width - 40), [Math]::Min(1000, $screen.Height - 40), 0x0040) | Out-Null
    if ($Empty) { Wait-Control 'Open workspace' | Out-Null } else { Wait-Control 'Workspace opened' | Out-Null }
}

function Get-FixtureDeploymentSettingsPath {
    $identity = $workspace.Replace('\', '/').ToLowerInvariant()
    $hash = [Convert]::ToHexString([System.Security.Cryptography.SHA256]::HashData([System.Text.Encoding]::UTF8.GetBytes($identity))).ToLowerInvariant()
    Join-Path (Split-Path -Parent $settingsPath) ('deployments/' + $hash + '.json')
}

function Get-FixtureCredentialName([string]$Id) {
    $hash = [System.IO.Path]::GetFileNameWithoutExtension((Get-FixtureDeploymentSettingsPath))
    "${hash}:${Id}.Regressify Desktop Deployment"
}

Write-JsonFixture (Join-Path $workspace 'visual_tests/smoke.tests.json') ([ordered]@{
    misMatchThreshold = 0.1
    scenarios = @(
        [ordered]@{ id = '8ac900fea19642cc0000'; label = 'Checkout'; url = 'https://example.com/checkout'; actions = @(@{ click = '#continue' }, @{ wait = 1000 }) },
        [ordered]@{ label = 'Signed-in account'; url = 'https://example.com/account'; actions = @() }
    )
})
Write-JsonFixture (Join-Path $workspace 'visual_tests/account.tests.json') ([ordered]@{
    scenarios = @([ordered]@{ label = 'Profile'; url = 'https://example.com/profile'; actions = @() })
})
Write-JsonFixture (Join-Path $workspace 'common/_viewports.yaml') @(
    @{ label = 'phone'; width = 390; height = 844 },
    @{ label = 'desktop'; width = 1280; height = 720 }
)
[System.IO.File]::WriteAllText((Join-Path $workspace 'regressify.yaml'), "bypassCsp: true`npostInteractionWait: 1000`nviewportNames: [desktop]`n")
if ($SchemaFlows -or $SourceFlows) {
    [System.IO.File]::WriteAllText((Join-Path $workspace 'visual_tests/schema.tests.yaml'), "scenarios: []`n")
}
if ($Capture) {
    $page = Join-Path $workspace 'fixture.html'
    [System.IO.File]::WriteAllText($page, '<!doctype html><html><head><title>QA fixture</title></head><body style="font:24px sans-serif;padding:48px;background:#f0f4f3"><h1>Checkout</h1><button id="continue">Continue</button><p>Order total: $42.00</p></body></html>')
    $url = [System.Uri]::new($page).AbsoluteUri
    Write-JsonFixture (Join-Path $workspace 'visual_tests/smoke.tests.json') ([ordered]@{
        scenarios = @(
            [ordered]@{ id = '8ac900fea19642cc0000'; label = 'Checkout'; url = $url; actions = @(@{ wait = 5000 }, @{ click = '#continue' }) },
            [ordered]@{ label = 'Signed-in account'; url = $url; actions = @(@{ wait = 2000 }) }
        )
    })
    Write-JsonFixture (Join-Path $workspace 'visual_tests/account.tests.json') ([ordered]@{
        scenarios = @([ordered]@{ label = 'Profile'; url = $url; actions = @(@{ wait = 5000 }) })
    })
}
$output = Join-Path $workspace '.regressify/smoke'
[System.IO.Directory]::CreateDirectory((Join-Path $output 'html_report')) | Out-Null
Write-TestImage (Join-Path $output 'bitmaps_reference/checkout.png') ([System.Drawing.Color]::Teal)
Write-TestImage (Join-Path $output 'bitmaps_test/smoke-run/checkout.png') ([System.Drawing.Color]::RoyalBlue)
Write-TestImage (Join-Path $output 'bitmaps_test/smoke-run/checkout_diff.png') ([System.Drawing.Color]::Crimson)
Write-JsonFixture (Join-Path $output 'bitmaps_test/smoke-run/report.json') ([ordered]@{
    id = 'smoke'
    tests = @(
        @{ status = 'fail'; pair = @{ label = '8ac900fea19642cc0000 desktop FINAL'; scenarioId = '8ac900fea19642cc0000'; scenarioLabel = 'Checkout'; selector = 'document'; referenceUrl = 'https://example.com/checkout'; url = 'https://staging.example.com/checkout'; fileName = '8ac900fea19642cc0000_desktop__FINAL.png'; reference = '../bitmaps_reference/checkout.png'; test = '../bitmaps_test/smoke-run/checkout.png'; diffImage = '../bitmaps_test/smoke-run/checkout_diff.png'; diff = @{ rawMisMatchPercentage = 4.2; mismatchPixels = 42; dimensionDifference = @{ width = 0; height = 0 } } } },
        @{ status = 'fail'; pair = @{ label = 'Account desktop'; scenarioLabel = 'Account desktop'; selector = 'body .carousel'; referenceUrl = 'https://example.com/account'; url = 'https://example.com/account'; engineErrorMsg = 'Missing selector #account' } }
    )
})

$stderr = Join-Path $artifacts 'native-stderr.log'
$stdout = Join-Path $artifacts 'native-stdout.log'
$settingsPath = Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'Regressify/Regressify Desktop/config/settings.json'
$settingsExisted = Test-Path -LiteralPath $settingsPath
$settingsBackup = Join-Path $workspace 'original-desktop-settings.json'
if ($settingsExisted) { Copy-Item -LiteralPath $settingsPath -Destination $settingsBackup }
$process = $null
$script:window = [IntPtr]::Zero
$script:fixtureClipboardBackup = $null
$script:fixtureClipboardOwned = $false
$script:fixtureProcesses = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$script:publishingBackups = @()
$script:publishingCredentials = [System.Collections.Generic.List[string]]::new()
$script:publishingLandingIds = [System.Collections.Generic.List[string]]::new()
if ($PublishingFlows) {
    foreach ($name in @('landing-apps.json', 'deployment-tokens.json')) {
        $path = Join-Path (Split-Path -Parent $settingsPath) $name
        $backup = Join-Path $workspace ('original-' + $name)
        $existed = Test-Path -LiteralPath $path
        if ($existed) { Copy-Item -LiteralPath $path -Destination $backup }
        $script:publishingBackups += @{ Path = $path; Backup = $backup; Existed = $existed }
    }
}
