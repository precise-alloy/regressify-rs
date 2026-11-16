if (($DeploymentFlows -or $PublishingFlows) -and -not ('RegressifySmokeCredentials' -as [type])) {
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class RegressifySmokeCredentials {
    [StructLayout(LayoutKind.Sequential)]
    struct Credential {
        public uint Flags, Type;
        public IntPtr TargetName, Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint CredentialBlobSize;
        public IntPtr CredentialBlob;
        public uint Persist, AttributeCount;
        public IntPtr Attributes, TargetAlias, UserName;
    }
    [DllImport("advapi32.dll", EntryPoint = "CredReadW", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool Read(string target, uint type, uint flags, out IntPtr credential);
    [DllImport("advapi32.dll", EntryPoint = "CredDeleteW", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool Delete(string target, uint type, uint flags);
    [DllImport("advapi32.dll")]
    static extern void CredFree(IntPtr credential);
    public static bool ExistsLocally(string target) {
        IntPtr pointer;
        if (!Read(target, 1, 0, out pointer)) {
            int error = Marshal.GetLastWin32Error();
            if (error == 1168) return false;
            throw new Win32Exception(error);
        }
        try {
            if (Marshal.PtrToStructure<Credential>(pointer).Persist != 2)
                throw new Exception("Fixture credential must be machine-local.");
            return true;
        } finally { CredFree(pointer); }
    }
    public static void Remove(string target) {
        if (Delete(target, 1, 0)) return;
        int error = Marshal.GetLastWin32Error();
        if (error != 1168) throw new Win32Exception(error);
    }
}
'@
}

function Clear-DeploymentFixtureSelection {
    Wait-Control 'Deselect all suites' | Out-Null
    if (Test-ControlInvokable 'Deselect all suites') {
        Invoke-Control 'Deselect all suites'
    }
}

function Test-DeploymentCopy([string]$Control, [string]$Expected, [switch]$SelectAll) {
    $clipboard = Invoke-FixtureClipboard { Copy-FixtureClipboardData ([System.Windows.Forms.Clipboard]::GetDataObject()) -FixtureOwned:$script:fixtureClipboardOwned }
    try {
        Invoke-FixtureClipboard { [System.Windows.Forms.Clipboard]::SetText('No deployment text copied') }
        $clipboardSequence = [RegressifySmokeClipboard]::GetClipboardSequenceNumber()
        Invoke-Control $Control -Pointer
        if ($SelectAll) {
            Send-FixtureKeys A -Control
            Send-FixtureKeys C -Control
        }
        Wait-FixtureClipboardText $clipboardSequence $Expected "Incorrect copied text from '$Control'."
    } finally {
        Invoke-FixtureClipboard {
            if ($null -ne $clipboard) { [System.Windows.Forms.Clipboard]::SetDataObject($clipboard, $true) } else { [System.Windows.Forms.Clipboard]::Clear() }
        }
    }
}

function Test-DeploymentFlows {
    Clear-DeploymentFixtureSelection
    if (Test-ControlInvokable 'Deploy') { throw 'Deploy must be disabled without selected suites.' }
    Invoke-Control 'Deployment targets'
    Wait-Control 'No deployment targets' | Out-Null
    Capture-Window 'deploy-no-selection'
    Invoke-Control 'Add target'
    Write-FixtureControlText 'Target name' 'QA preview'
    Invoke-Control 'Save target'
    Wait-Control 'Enter a deployment token' | Out-Null
    Invoke-Control 'Deployment token' -Pointer
    Wait-Control 'Deployment token' | Out-Null
    $syntheticToken = ('a' * 64) + '-' + ('b' * 36) + '000'
    Send-FixtureText $syntheticToken
    $tokenInput = Wait-Control 'Deployment token'
    if (-not $tokenInput.Current.IsPassword) { throw 'Deployment token must be a protected password field.' }
    $valuePattern = $null
    if ($tokenInput.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$valuePattern)) {
        if (([System.Windows.Automation.ValuePattern]$valuePattern).Current.Value -eq $syntheticToken) { throw 'The token is exposed through accessibility.' }
    }
    Write-FixtureControlText 'Azure environment' 'preview'
    Capture-Window 'deploy-target-editor'
    Invoke-Control 'Save target'
    Wait-Control 'QA preview' | Out-Null
    $targetSettingsPath = Get-FixtureDeploymentSettingsPath
    $targetSettings = Get-Content -LiteralPath $targetSettingsPath -Raw
    if ($targetSettings.Contains($syntheticToken) -or $targetSettings.Contains('tokenEnv')) { throw 'Target settings must contain only a secure credential reference.' }
    $credentialId = ($targetSettings | ConvertFrom-Json).targets[0].credentialId
    $credentialName = Get-FixtureCredentialName $credentialId
    if (-not [RegressifySmokeCredentials]::ExistsLocally($credentialName)) { throw 'The target token was not saved to the machine-local credential store.' }
    Invoke-Control 'Edit target'
    Write-FixtureControlText 'Target name' 'QA reports'
    Invoke-Control 'Save target'
    Wait-Control 'QA reports' | Out-Null
    if (-not [RegressifySmokeCredentials]::ExistsLocally($credentialName)) { throw 'Editing a target must preserve its saved token.' }
    Invoke-Control 'Close'
    $script:process.CloseMainWindow() | Out-Null
    if (-not $script:process.WaitForExit(10000)) { throw 'Deployment fixture did not close normally.' }
    $publishedUrl = 'https://reports.example.com/'
    $targetSettings = Get-Content -LiteralPath $targetSettingsPath -Raw | ConvertFrom-Json -AsHashtable
    $targetSettings.targets[0].publishedUrl = $publishedUrl
    $targetSettings.reportPublications = @(@{ targetId = $targetSettings.targets[0].id; suiteIds = @('smoke') })
    Write-JsonFixture $targetSettingsPath $targetSettings
    Start-Fixture
    Invoke-Control 'Deployment targets'
    Wait-Control 'QA reports' | Out-Null
    if (-not [RegressifySmokeCredentials]::ExistsLocally($credentialName)) { throw 'The saved token did not survive an app restart.' }
    $link = Wait-Control 'Open deployed site'
    if ($link.Current.ControlType -ne [System.Windows.Automation.ControlType]::Hyperlink) { throw 'The deployed URL must use a native link.' }
    $linkBounds = $link.Current.BoundingRectangle
    $copyBounds = (Wait-Control 'Copy deployed URL').Current.BoundingRectangle
    if ($copyBounds.Left -lt $linkBounds.Right -or $copyBounds.Left - $linkBounds.Right -gt 12) { throw 'Copy must be next to the deployed link.' }
    if ($linkBounds.Width -gt 280) { throw 'A short deployed URL must not fill the dialog width.' }
    Test-DeploymentCopy 'Copy deployed URL' $publishedUrl
    Capture-Window 'deploy-target-restored'
    Invoke-Control 'Close'
    Clear-DeploymentFixtureSelection
    Invoke-Control 'smoke.tests.json'
    Wait-Control 'Run Suite Reference' | Out-Null
    Invoke-Control 'Deploy'
    Wait-Control 'Replace the whole site with the selection' | Out-Null
    if (Test-ControlInvokable 'Deploy selected reports') { throw 'An unreadable site must not be updated, or replaced without confirmation.' }
    Capture-Window 'deploy-partial-selection'
    foreach ($dimensions in @(@(1000, 640), @(1600, 1000), @(1080, 720))) {
        [RegressifySmokeNative]::SetWindowPos($script:window, [IntPtr]::Zero, $screen.Left + 20, $screen.Top + 20, $dimensions[0], $dimensions[1], 0x0040) | Out-Null
        $captureSize = if ($dimensions[1] -eq 640) { 'minimum' } elseif ($dimensions[0] -eq 1080) { 'compact' } else { 'large' }
        foreach ($theme in @('Dark', 'Light')) {
            Invoke-Control 'Close'
            Select-Theme $theme
            Capture-Window ($captureSize + '-titlebar-' + $theme.ToLowerInvariant())
            Invoke-Control 'Runs'
            $siteBounds = (Wait-Control 'Open Site').Current.BoundingRectangle
            $reportBounds = (Wait-Control 'Open Report').Current.BoundingRectangle
            if ($siteBounds.Top -ne $reportBounds.Top -or $siteBounds.Left -lt $reportBounds.Right) { throw 'Open Site must be beside Open Report.' }
            Capture-Window ('deploy-runs-links-' + $theme.ToLowerInvariant() + '-' + $captureSize)
            Invoke-Control 'Deploy'
            Wait-Control 'Replace the whole site with the selection' | Out-Null
            if (Test-ControlInvokable 'Deploy selected reports') { throw 'Resizing must not bypass the replacement confirmation.' }
            $link = Wait-Control 'Open deployed site'
            if ($link.Current.ControlType -ne [System.Windows.Automation.ControlType]::Hyperlink) { throw 'The resized deployed URL must remain a native link.' }
            $linkBounds = $link.Current.BoundingRectangle
            $copyBounds = (Wait-Control 'Copy deployed URL').Current.BoundingRectangle
            if ($copyBounds.Left -lt $linkBounds.Right -or $copyBounds.Left - $linkBounds.Right -gt 12) { throw 'Resizing separated Copy from the deployed link.' }
            if ($linkBounds.Width -gt 280) { throw 'Resizing stretched a short deployed URL across the dialog.' }
            Capture-Window ('deploy-' + $theme.ToLowerInvariant() + '-' + $captureSize)
            Invoke-Control 'Edit target'
            Wait-Control 'Azure environment' | Out-Null
            if ((Read-ControlText 'Azure environment') -cne 'preview') { throw 'Resizing changed the saved Azure environment.' }
            Capture-Window ('deploy-azure-' + $theme.ToLowerInvariant() + '-' + $captureSize)
            Invoke-Control 'Cancel edit'
            Wait-Control 'Replace the whole site with the selection' | Out-Null
        }
    }
    Invoke-Control 'Replace the whole site with the selection'
    Wait-Control 'Replaces everything on QA reports; the suites it serves are unknown' | Out-Null
    if (Test-ControlInvokable 'Deploy selected reports') { throw 'Replacing an unknown site must require confirmation.' }
    Invoke-Control 'I understand everything else on the site will be removed'
    Wait-WindowState { Test-ControlInvokable 'Deploy selected reports' } 'Confirmed replacement must be invokable.'
    Capture-Window 'deploy-replace-confirmed'
    [RegressifySmokeCredentials]::Remove($credentialName)
    Invoke-Control 'Deploy selected reports'
    Wait-Control 'Deployment failed' | Out-Null
    Wait-Control 'Report deployment' | Out-Null
    Wait-Control 'Deployment log' | Out-Null
    if ($null -ne (Find-Control 'Scenarios / viewports: 0')) { throw 'Deployment activity must not show scenario counters.' }
    if ($null -ne (Find-Control 'Refresh saved captures')) { throw 'Deployment activity must not show the suite statistics row.' }
    Wait-Control 'Cannot load the deployment token: No saved deployment token. Edit the target and enter its deployment token' | Out-Null
    Wait-SourceText 'outcome=Failure' 'Deployment log text'
    $deploymentLog = Read-ControlText 'Deployment log text'
    Test-DeploymentCopy 'Copy deployment log' $deploymentLog
    Test-DeploymentCopy 'Deployment log text' $deploymentLog -SelectAll
    foreach ($dimensions in @(@(1000, 640), @(1600, 1000), @(1080, 720))) {
        [RegressifySmokeNative]::SetWindowPos($script:window, [IntPtr]::Zero, $screen.Left + 20, $screen.Top + 20, $dimensions[0], $dimensions[1], 0x0040) | Out-Null
        $captureSuffix = if ($dimensions[1] -eq 640) { '-minimum' } elseif ($dimensions[0] -eq 1080) { '-compact' } else { '' }
        foreach ($theme in @('Light', 'Dark')) {
            Select-Theme $theme
            Wait-Control 'Deployment failed' | Out-Null
            Wait-SourceText 'outcome=Failure' 'Deployment log text'
            Test-DeploymentCopy 'Deployment log text' $deploymentLog -SelectAll
            Capture-Window ('deploy-outcome-' + $theme.ToLowerInvariant() + $captureSuffix)
        }
    }
    Invoke-Control 'View Log'
    Wait-SourceText 'outcome=Failure' 'Run log'
    Capture-Window 'deploy-missing-token'
    [RegressifySmokeNative]::SetWindowPos($script:window, [IntPtr]::Zero, $screen.Left + 20, $screen.Top + 20, 1000, 640, 0x0040) | Out-Null
    foreach ($theme in @('Light', 'Dark')) {
        Select-Theme $theme
        Wait-SourceText 'outcome=Failure' 'Run log'
        Capture-Window ('deploy-missing-token-' + $theme.ToLowerInvariant() + '-minimum')
    }
    [RegressifySmokeNative]::SetWindowPos($script:window, [IntPtr]::Zero, $screen.Left + 20, $screen.Top + 20, 1080, 720, 0x0040) | Out-Null
    Invoke-Control 'Deployment targets'
    Wait-Control 'QA reports' | Out-Null
    Invoke-Control 'Edit target'
    Invoke-Control 'Deployment token' -Pointer
    Wait-Control 'Deployment token' | Out-Null
    Send-FixtureText $syntheticToken
    Invoke-Control 'Save target'
    Wait-Control 'QA reports' | Out-Null
    Wait-WindowState { $null -eq (Find-Control 'Save target') -and [RegressifySmokeCredentials]::ExistsLocally($credentialName) } 'A missing credential must be replaceable.'
    Invoke-Control 'Remove target'
    Wait-Control 'Keep target' | Out-Null
    Capture-Window 'deploy-remove-confirmation'
    Invoke-Control 'Keep target'
    Invoke-Control 'Remove target'
    Invoke-Control 'Confirm remove target'
    Wait-Control 'No deployment targets' | Out-Null
    if ([RegressifySmokeCredentials]::ExistsLocally($credentialName)) { throw 'Removing a target must delete its saved credential.' }
    Invoke-Control 'Close'
    Invoke-Control 'Deployment targets'
    Invoke-Control 'Add target'
    Invoke-Control 'Deployment provider' -Pointer
    Wait-Control 'Netlify' | Out-Null
    Capture-Window 'deploy-provider-menu'
    Invoke-Control 'Netlify'
    Wait-Control 'Netlify Site ID' | Out-Null
    if ($null -ne (Find-Control 'Azure environment')) { throw 'Netlify must not expose Azure-only settings.' }
    Write-FixtureControlText 'Target name' 'Netlify QA'
    Invoke-Control 'Save target'
    Wait-Control 'Enter the Netlify Site ID (Project ID) from project configuration' | Out-Null
    $netlifySiteId = [guid]::NewGuid().ToString('D')
    Write-FixtureControlText 'Netlify Site ID' $netlifySiteId
    Invoke-Control 'Personal access token' -Pointer
    Wait-Control 'Personal access token' | Out-Null
    $netlifyToken = 'synthetic-netlify-token-' + [guid]::NewGuid().ToString('N')
    Send-FixtureText $netlifyToken
    $tokenInput = Wait-Control 'Personal access token'
    if (-not $tokenInput.Current.IsPassword) { throw 'Netlify tokens must be protected password fields.' }
    Capture-Window 'deploy-netlify-editor'
    Invoke-Control 'Save target'
    Wait-Control 'Netlify QA' | Out-Null
    $targetSettings = Get-Content -LiteralPath $targetSettingsPath -Raw
    $target = ($targetSettings | ConvertFrom-Json).targets[0]
    if ($target.provider -ne 'netlify' -or $target.siteId -ne $netlifySiteId -or $targetSettings.Contains($netlifyToken)) { throw 'Netlify settings must persist their provider and Site ID without the token.' }
    $credentialName = Get-FixtureCredentialName $target.credentialId
    if (-not [RegressifySmokeCredentials]::ExistsLocally($credentialName)) { throw 'Netlify token was not saved securely.' }
    Invoke-Control 'Edit target'
    Invoke-Control 'Deployment provider' -Pointer
    Invoke-Control 'Azure Static Web Apps'
    Wait-Control 'Azure environment' | Out-Null
    Invoke-Control 'Save target'
    Wait-Control 'Enter a new deployment token when changing providers' | Out-Null
    Invoke-Control 'Cancel edit'
    Invoke-Control 'Close'
    $script:process.CloseMainWindow() | Out-Null
    if (-not $script:process.WaitForExit(10000)) { throw 'Netlify fixture did not close normally.' }
    Start-Fixture
    Invoke-Control 'Deployment targets'
    Wait-Control 'Netlify QA' | Out-Null
    if (-not [RegressifySmokeCredentials]::ExistsLocally($credentialName)) { throw 'Netlify token did not survive restart.' }
    Capture-Window 'deploy-netlify-restored'
    Invoke-Control 'Close'
    foreach ($dimensions in @(@(1000, 640), @(1600, 1000), @(1080, 720))) {
        [RegressifySmokeNative]::SetWindowPos($script:window, [IntPtr]::Zero, $screen.Left + 20, $screen.Top + 20, $dimensions[0], $dimensions[1], 0x0040) | Out-Null
        $captureSuffix = if ($dimensions[1] -eq 640) { '-minimum' } elseif ($dimensions[0] -eq 1080) { '-compact' } else { '' }
        foreach ($theme in @('Light', 'Dark')) {
            Select-Theme $theme
            Invoke-Control 'Deployment targets'
            Invoke-Control 'Edit target'
            Wait-Control 'Netlify Site ID' | Out-Null
            if ((Read-ControlText 'Netlify Site ID') -ne $netlifySiteId) { throw 'Saved Netlify Site ID was not restored.' }
            Capture-Window ('deploy-netlify-' + $theme.ToLowerInvariant() + $captureSuffix)
            Invoke-Control 'Cancel edit'
            Invoke-Control 'Close'
        }
    }
    [RegressifySmokeCredentials]::Remove($credentialName)
    Clear-DeploymentFixtureSelection
    Invoke-Control 'smoke.tests.json'
    Wait-Control 'Run Suite Reference' | Out-Null
    Invoke-Control 'Deploy'
    Invoke-Control 'Replace the whole site with the selection'
    Invoke-Control 'I understand everything else on the site will be removed'
    Wait-WindowState { Test-ControlInvokable 'Deploy selected reports' } 'Confirmed replacement must be invokable.'
    Invoke-Control 'Deploy selected reports'
    Wait-Control 'Deployment failed' | Out-Null
    Wait-Control 'Report deployment' | Out-Null
    Wait-Control 'Deployment log' | Out-Null
    Wait-Control 'Cannot load the deployment token: No saved deployment token. Edit the target and enter its deployment token' | Out-Null
    Capture-Window 'deploy-netlify-outcome'
    Invoke-Control 'View Log'
    Wait-SourceText 'outcome=Failure' 'Run log'
    Capture-Window 'deploy-netlify-missing-token'
    Invoke-Control 'Deployment targets'
    Invoke-Control 'Edit target'
    Invoke-Control 'Personal access token' -Pointer
    Wait-Control 'Personal access token' | Out-Null
    Send-FixtureText $netlifyToken
    Invoke-Control 'Save target'
    Wait-WindowState { $null -eq (Find-Control 'Save target') -and [RegressifySmokeCredentials]::ExistsLocally($credentialName) } 'Netlify token replacement did not finish.'
    Invoke-Control 'Remove target'
    Invoke-Control 'Confirm remove target'
    Wait-Control 'No deployment targets' | Out-Null
    if ([RegressifySmokeCredentials]::ExistsLocally($credentialName)) { throw 'Netlify credential was not removed with its target.' }
    Invoke-Control 'Close'
    Write-Output 'Azure and Netlify provider selection, credentials, restart persistence, adjacent saved-site links, selectable deployment logs, Update/Replace guards for unreadable sites, and safe missing-token failures passed without remote deployment.'
}
