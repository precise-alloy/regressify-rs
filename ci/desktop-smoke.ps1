<#
.SYNOPSIS
Build and exercise Regressify Desktop in a synthetic Windows workspace.
.DESCRIPTION
Requires an interactive Windows desktop. Captures only the application window.
Icon checks also capture the application's taskbar button.
Artifacts and the fixture workspace remain under target/desktop-smoke.
.PARAMETER OverwriteClipboard
Discard the existing clipboard without backing it up. Clear test data on exit.
#>
param(
    [switch]$SkipBuild,
    [switch]$KeepOpen,
    [switch]$OverwriteClipboard,
    [switch]$Capture,
    [switch]$ScenarioFlows,
    [switch]$ArtifactFlows,
    [switch]$CleanupFlows,
    [switch]$InstanceFlows,
    [switch]$UpdateFlows,
    [switch]$DeploymentFlows,
    [switch]$PublishingFlows,
    [switch]$TitlebarFlows,
    [switch]$GitFlows,
    [switch]$ComparisonFlows,
    [switch]$ErrorImageFlows,
    [switch]$IconFlows,
    [switch]$SchemaFlows,
    [switch]$SourceFlows
)

$ErrorActionPreference = 'Stop'
if ($PublishingFlows -and $KeepOpen) { throw 'Publishing smoke requires cleanup; do not combine -PublishingFlows with -KeepOpen.' }
if ($ScenarioFlows -or $UpdateFlows) { $Capture = $true }
$repository = Split-Path -Parent $PSScriptRoot
$artifacts = Join-Path $repository 'target/desktop-smoke'
$workspace = Join-Path $artifacts ('workspace-' + [guid]::NewGuid().ToString('N'))
if ($GitFlows) { $workspace = Join-Path ([System.IO.Path]::GetTempPath()) ('regressify-git-smoke-' + [guid]::NewGuid().ToString('N')) }
$executable = Join-Path $repository 'target/debug/regressify-desktop.exe'

if (-not $IsWindows) { throw 'This smoke test requires Windows.' }
if (Get-Process LogonUI -ErrorAction SilentlyContinue) { throw 'Windows is locked. Unlock the interactive desktop before native smoke checks; no preferences or windows were changed.' }
if (Get-Process regressify-desktop -ErrorAction SilentlyContinue) { throw 'Close desktop windows before native smoke checks because the fixture temporarily changes shared preferences.' }
if (-not $SkipBuild) {
    Push-Location $repository
    try {
        & cargo build --features gui --bin regressify-desktop --quiet
        if ($LASTEXITCODE -ne 0) { throw 'Desktop build failed.' }
    } finally { Pop-Location }
}

. (Join-Path $PSScriptRoot 'desktop-smoke/native.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/controls.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/fixture.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/icons.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/results.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/instances.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/comparison.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/error-images.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/artifacts.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/scenarios.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/deployment.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/publishing.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/titlebar.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/git.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/schema.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/default.ps1')
. (Join-Path $PSScriptRoot 'desktop-smoke/flows/updates.ps1')

$cleanupSnapshot = $null
$previousAnalyticsFixture = $env:REGRESSIFY_TEST_NO_ANALYTICS
try {
    # All fixture launches inherit this, including duplicate windows and workers.
    $env:REGRESSIFY_TEST_NO_ANALYTICS = '1'
    Initialize-FixtureClipboard -OverwriteClipboard:$OverwriteClipboard
    Write-JsonFixture $settingsPath @{ theme = 'light'; sourceFontSize = 14; sourceWrap = $true; sourceLineNumbers = $true; navigatorHidden = $false; recentWorkspaces = @() }
    if ($PublishingFlows) {
        foreach ($backup in $script:publishingBackups) {
            Write-JsonFixture $backup.Path $(if ([System.IO.Path]::GetFileName($backup.Path) -eq 'landing-apps.json') { @{ files = @() } } else { @{ tokens = @() } })
        }
    }
    Start-Fixture
    if ($IconFlows) {
        Test-WindowIcons
        Test-BrandingIcons
        return
    }
    if ($ErrorImageFlows) {
        Test-ErrorImageFlows
        return
    }
    Wait-Control 'Switch workspace' | Out-Null
    Test-ApplicationMenus
    if ($SchemaFlows) {
        Test-SchemaFlows
        return
    }
    if ($SourceFlows) {
        Test-SourceFlows
        return
    }
    if ($GitFlows) {
        Test-GitFlows
        return
    }
    if ($ComparisonFlows) {
        Test-ComparisonFlows
        return
    }
    if ($TitlebarFlows) {
        Test-TitlebarFlows
        return
    }
    if ($DeploymentFlows) {
        Test-DeploymentFlows
        return
    }
    if ($PublishingFlows) {
        Test-PublishingFlows
        return
    }
    if ($UpdateFlows) {
        Test-UpdateFlows
        return
    }
    if ($InstanceFlows) {
        Test-InstanceFlows
        return
    }
    if ($ArtifactFlows) {
        Test-ArtifactFlows
        return
    }
    if ($CleanupFlows) {
        Test-CleanupFlows
        return
    }
    if ($ScenarioFlows) {
        Test-ScenarioFlows
        return
    }
    Test-DefaultFlows
} finally {
    try {
        . (Join-Path $PSScriptRoot 'desktop-smoke/cleanup.ps1')
    } finally {
        $env:REGRESSIFY_TEST_NO_ANALYTICS = $previousAnalyticsFixture
        Restore-FixtureClipboard
    }
}
Write-Output "Native smoke test passed. Fixture: $workspace"
