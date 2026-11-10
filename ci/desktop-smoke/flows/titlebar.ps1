function Test-TitlebarControlBounds([string]$Theme, [int[]]$Dimensions, [string]$ExpectedGitBranch = '') {
    Focus-Fixture
    $bounds = [RegressifySmokeNative+Rect]::new()
    if (-not [RegressifySmokeNative]::GetWindowRect($script:window, [ref]$bounds)) { throw 'Cannot read titlebar bounds.' }
    $dpi = [RegressifySmokeWindowState]::GetDpiForWindow($script:window)
    $scale = $dpi / 96.0
    $border = if ([RegressifySmokeWindowState]::IsZoomed($script:window)) { 0 } else {
        [RegressifySmokeWindowState]::GetSystemMetricsForDpi(32, $dpi) + [RegressifySmokeWindowState]::GetSystemMetricsForDpi(92, $dpi)
    }
    if ($Dimensions.Count -ne 2) { throw 'Titlebar checks require the requested width and height.' }
    $expectedWidth = [Math]::Max($Dimensions[0], [Math]::Round(1000 * $scale) + 2 * $border)
    $expectedHeight = [Math]::Max($Dimensions[1], [Math]::Round(640 * $scale) + $border)
    if ($bounds.Right - $bounds.Left -ne $expectedWidth -or $bounds.Bottom - $bounds.Top -ne $expectedHeight) {
        throw "Titlebar capture must use ${expectedWidth}x${expectedHeight} outer bounds for the requested size, not a raised minimum."
    }
    # Preserve the 95px contract and the three 34px slots used by Invoke-WindowButton.
    $rightLimit = $bounds.Right - $border - [Math]::Max(95, 3 * 34) * $scale
    $names = @('File', 'Publish', 'Help', 'Editor', 'Runs', 'Results', 'Application status', 'Commands', 'Deploy', "Appearance: $Theme", 'Switch workspace')
    $names += $(if ($null -ne (Find-Control 'Hide navigator')) { 'Hide navigator' } else { 'Show navigator' })
    foreach ($name in $names) {
        $control = Wait-Control $name
        $rectangle = $control.Current.BoundingRectangle
        if ($rectangle.Width -le 0 -or $rectangle.Height -le 0 -or
            $rectangle.Left -lt $bounds.Left + $border -or $rectangle.Right -gt $rightLimit -or
            $rectangle.Top -lt $bounds.Top -or $rectangle.Bottom -gt $bounds.Top + 38 * $scale) {
            throw "Titlebar control '$name' is outside the header or overlaps the native window-control reserve: $rectangle."
        }
        if ($name -in @('Commands', 'Deploy', "Appearance: $Theme", 'Switch workspace')) {
            if ($control.Current.ControlType -ne [System.Windows.Automation.ControlType]::Button -or
                -not $control.Current.IsEnabled) {
                throw "Titlebar action '$name' must remain an enabled native button."
            }
            if ($name -eq "Appearance: $Theme") {
                if (-not $control.Current.IsKeyboardFocusable) { throw 'The appearance dropdown must support native keyboard focus.' }
            } elseif (-not (Test-ControlInvokable $name)) {
                throw "Titlebar action '$name' must retain native Invoke support."
            }
        }
    }
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($script:window)
    $clientWidth = ($bounds.Right - $bounds.Left - 2 * $border) / $scale
    $versions = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) |
        Where-Object { $_.Current.Name -match '^v\d+\.\d+\.\d+(?:[-+].+)?$' })
    if ($versions.Count -ne [int]($clientWidth -ge 1080)) {
        throw 'The title-bar version must be visible only at client widths of 1080px or wider.'
    }
    if ($versions.Count -eq 1) {
        $versionBounds = $versions[0].Current.BoundingRectangle
        $logoBounds = (Wait-Control 'Regressify logo').Current.BoundingRectangle
        if ($versionBounds.Width -le 0 -or $versionBounds.Height -le 0 -or
            [Math]::Abs(($versionBounds.Top + $versionBounds.Height / 2) - ($logoBounds.Top + $logoBounds.Height / 2)) -gt $scale) {
            throw 'The title-bar version must share the application name and logo row.'
        }
    }
    $workspaceControl = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $workspace.Replace('\', '/'))) |
        Where-Object { $_.Current.BoundingRectangle.Bottom -le $bounds.Top + 38 * $scale } |
        Select-Object -First 1
    if (($null -ne $workspaceControl) -ne ($clientWidth -ge 1400)) {
        throw 'The workspace name must be visible only at client widths of 1400px or wider.'
    }
    if ($null -ne $workspaceControl) {
        $workspaceBounds = $workspaceControl.Current.BoundingRectangle
        $resultsBounds = (Wait-Control 'Results').Current.BoundingRectangle
        if ([Math]::Abs($workspaceBounds.Left - $resultsBounds.Right - 16 * $scale) -gt 1) {
            throw 'The workspace name must retain a 16px gap after Results, allowing native pixel rounding.'
        }
    }
    $gitButtons = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) |
        Where-Object { $_.Current.ControlType -eq [System.Windows.Automation.ControlType]::Button -and $_.Current.Name.StartsWith('Git: ') })
    if ($gitButtons.Count -ne 1) { throw 'The titlebar must expose exactly one named Git button.' }
    $git = $gitButtons[0].Current
    $rectangle = $git.BoundingRectangle
    if ($rectangle.Width -le 0 -or $rectangle.Width -gt 112 * $scale -or $rectangle.Height -le 0 -or
        $rectangle.Left -lt $bounds.Left + $border -or $rectangle.Right -gt $rightLimit -or
        $rectangle.Top -lt $bounds.Top -or $rectangle.Bottom -gt $bounds.Top + 38 * $scale) {
        throw "Git badge exceeds its 112px cap or overlaps the native window-control reserve: $rectangle."
    }
    if ($ExpectedGitBranch -and (-not $git.Name.StartsWith("Git: $ExpectedGitBranch | 1 changed files | ") -or
        $git.Name -notmatch ' \| \d+ incoming \| \d+ outgoing\r?\n\S')) {
        throw 'The conflict Git button lost its full branch or status accessibility name.'
    }
}

function Test-TitlebarFlows {
    Invoke-Control 'smoke.tests.json'
    Invoke-Control 'Checkout'
    Invoke-Control 'Source'
    foreach ($dimensions in @(@(1000, 640), @(1600, 1000))) {
        [RegressifySmokeNative]::SetWindowPos($script:window, [IntPtr]::Zero, $screen.Left + 20, $screen.Top + 20, $dimensions[0], $dimensions[1], 0x0040) | Out-Null
        Select-Theme 'Dark'
        foreach ($theme in @('Light', 'Dark')) {
            Select-Theme $theme
            Test-TitlebarControlBounds $theme -Dimensions $dimensions
            $appearance = Wait-Control "Appearance: $theme"
            for ($attempt = 0; $attempt -lt 64 -and -not [System.Windows.Automation.Automation]::Compare([System.Windows.Automation.AutomationElement]::FocusedElement, $appearance); $attempt++) {
                Send-FixtureKeys Tab
            }
            Wait-WindowState { [System.Windows.Automation.Automation]::Compare([System.Windows.Automation.AutomationElement]::FocusedElement, $appearance) } 'The appearance dropdown did not receive native keyboard focus.'
            Send-FixtureKeys Enter
            Wait-Control 'System' | Out-Null
            Wait-Control 'Light' | Out-Null
            Wait-Control 'Dark' | Out-Null
            Send-FixtureKeys Escape
            Wait-WindowState { $null -eq (Find-Control 'System') } 'Escape did not dismiss the keyboard-opened appearance menu.'
            Capture-Window "stock-sizing-final-$($theme.ToLowerInvariant())-$($dimensions[0])"
            Invoke-Control 'Commands' -Pointer
            Wait-Control 'New suite' | Out-Null
            Wait-Control 'Landing apps' | Out-Null
            Capture-Window "stock-sizing-commands-$($theme.ToLowerInvariant())-$($dimensions[0])"
            Send-FixtureKeys Escape
            Wait-WindowState { $null -eq (Find-Control 'Landing apps') } 'Titlebar command search did not close.'
            Invoke-Control 'Deploy' -Pointer
            Wait-Control 'Replace the whole site with the selection' | Out-Null
            Capture-Window "stock-sizing-deploy-$($theme.ToLowerInvariant())-$($dimensions[0])"
            Invoke-Control 'Close'
            Wait-WindowState { $null -eq (Find-Control 'Replace the whole site with the selection') } 'Titlebar deployment dialog did not close.'
        }
    }
    Select-Theme 'Light'
    Invoke-Control 'Commands' -Pointer
    Wait-Control 'New suite' | Out-Null
    Capture-Window 'titlebar-command-pointer'
    Send-FixtureKeys Escape
    Invoke-Control 'Hide navigator' -Pointer
    Wait-Control 'Show navigator' | Out-Null
    Invoke-Control 'Show navigator' -Pointer
    Wait-Control 'Hide navigator' | Out-Null
    Invoke-Control 'Runs' -Pointer
    Wait-Control 'Run settings' | Out-Null
    Invoke-Control 'Results' -Pointer
    Wait-Control 'Approve This Scenario' | Out-Null
    Invoke-Control 'Editor' -Pointer
    Invoke-Control 'smoke.tests.json'
    Wait-Control 'Run Suite Reference' | Out-Null
    Invoke-Control 'Deploy' -Pointer
    Wait-Control 'Replace the whole site with the selection' | Out-Null
    Capture-Window 'titlebar-deploy-pointer'
    Invoke-Control 'Close'
    $before = [RegressifySmokeNative+Rect]::new()
    [RegressifySmokeNative]::GetWindowRect($script:window, [ref]$before) | Out-Null
    $dragControl = Find-Control ($workspace.Replace('\', '/'))
    if ($null -eq $dragControl) { $dragControl = Wait-Control 'Regressify logo' }
    $drag = $dragControl.Current.BoundingRectangle
    Focus-Fixture
    $horizontal = [int]($drag.Left + $drag.Width / 2)
    $vertical = [int]($drag.Top + $drag.Height / 2)
    [RegressifySmokePointer]::SetPhysicalCursorPos($horizontal, $vertical) | Out-Null
    [RegressifySmokeNative]::DwmFlush() | Out-Null
    [RegressifySmokeNative]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
    try {
        foreach ($step in 1..10) {
            [RegressifySmokePointer]::SetPhysicalCursorPos($horizontal + 6 * $step, $vertical + 4 * $step) | Out-Null
            [RegressifySmokeNative]::DwmFlush() | Out-Null
        }
    } finally {
        [RegressifySmokeNative]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
    }
    Wait-WindowState {
        $after = [RegressifySmokeNative+Rect]::new()
        [RegressifySmokeNative]::GetWindowRect($script:window, [ref]$after) | Out-Null
        [Math]::Abs($after.Left - $before.Left) -ge 20 -and [Math]::Abs($after.Top - $before.Top) -ge 10
    } 'Dragging the workspace title did not move the window.'
    Capture-Window 'titlebar-dragged'
    Invoke-WindowButton 'Maximize'
    Wait-WindowState { [RegressifySmokeWindowState]::IsZoomed($script:window) } 'Native maximize button did not maximize the window.'
    Capture-Window 'titlebar-maximized'
    Invoke-WindowButton 'Maximize'
    Wait-WindowState { -not [RegressifySmokeWindowState]::IsZoomed($script:window) } 'Native restore button did not restore the window.'
    Capture-Window 'titlebar-restored'
    Invoke-WindowButton 'Minimize'
    Wait-WindowState { [RegressifySmokeWindowState]::IsIconic($script:window) } 'Native minimize button did not minimize the window.'
    [RegressifySmokeWindowState]::ShowWindowAsync($script:window, 9) | Out-Null
    Wait-WindowState { -not [RegressifySmokeWindowState]::IsIconic($script:window) } 'Minimized fixture did not restore.'
    Focus-Fixture
    Capture-Window 'titlebar-unminimized'
    Invoke-Control 'smoke.tests.json'
    Invoke-Control 'Checkout'
    Invoke-Control 'Source'
    Invoke-Control 'Configuration source' -Pointer
    $script:cleanupPath = Join-Path $workspace 'visual_tests/smoke.tests.json'
    $script:cleanupSnapshot = (Get-FileHash $script:cleanupPath -Algorithm SHA256).Hash
    Send-FixtureKeys End -Control
    Send-FixtureText 'titlebar-close-probe'
    Wait-SourceText 'titlebar-close-probe'
    Invoke-WindowButton 'Close'
    Wait-Control 'Discard' | Out-Null
    Wait-Control 'Cancel' | Out-Null
    Capture-Window 'titlebar-close-guard'
    Invoke-Control 'Cancel'
    if ($process.HasExited) { throw 'Cancelling titlebar close discarded the window.' }
    Wait-SourceText 'titlebar-close-probe'
    if ((Get-FileHash $script:cleanupPath -Algorithm SHA256).Hash -ne $script:cleanupSnapshot) { throw 'Titlebar close changed the saved source.' }
    Write-Output 'Native pointer controls, dragging, maximize/restore, minimize, and unsaved-close guard passed.'
}
