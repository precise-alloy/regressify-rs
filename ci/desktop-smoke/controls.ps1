function Focus-Fixture {
    $activation = New-Object -ComObject WScript.Shell
    $deadline = [System.Diagnostics.Stopwatch]::StartNew()
    do {
        if ($process.HasExited) { throw 'The native fixture exited before input could be delivered.' }
        if ([RegressifySmokeFocus]::GetForegroundWindow() -eq $script:window) { return }
        [RegressifySmokeNative]::SetForegroundWindow($script:window) | Out-Null
        if ([RegressifySmokeFocus]::GetForegroundWindow() -eq $script:window) { return }
        $activation.AppActivate([int]$process.Id) | Out-Null
        if ([RegressifySmokeFocus]::GetForegroundWindow() -eq $script:window) { return }
        [RegressifySmokeNative]::DwmFlush() | Out-Null
    } while ($deadline.Elapsed.TotalSeconds -lt 2)
    throw 'Cannot focus the native fixture. Keep Windows unlocked and the interactive desktop available.'
}

function Send-FixtureText([string]$Text) {
    Focus-Fixture
    [RegressifySmokeText]::Type($Text)
    [RegressifySmokeNative]::DwmFlush() | Out-Null
}

function Send-FixtureKeys([ConsoleKey]$Key, [switch]$Control, [switch]$Shift, [switch]$Alt) {
    Focus-Fixture
    [RegressifySmokeText]::Press($Key, $Control.IsPresent, $Shift.IsPresent, $Alt.IsPresent)
    [RegressifySmokeNative]::DwmFlush() | Out-Null
}

function Wait-Settings([scriptblock]$Match) {
    $deadline = [System.Diagnostics.Stopwatch]::StartNew()
    do {
        $settings = Get-Content -LiteralPath $settingsPath -Raw | ConvertFrom-Json
        if (& $Match $settings) { return }
        if ($process.HasExited) { throw 'Desktop exited before preferences were saved.' }
        [RegressifySmokeNative]::DwmFlush() | Out-Null
    } while ($deadline.Elapsed.TotalSeconds -lt 10)
    throw 'Desktop preferences were not persisted.'
}

function Select-Theme([string]$Theme) {
    $current = @('Light', 'Dark', 'System') | Where-Object { $null -ne (Find-Control ('Appearance: ' + $_)) } | Select-Object -First 1
    if ([string]::IsNullOrEmpty($current)) { throw 'Appearance control is missing.' }
    if ($current -ne $Theme) {
        Invoke-Control ('Appearance: ' + $current) -Pointer
        Invoke-Control $Theme
    }
    Wait-Control ('Appearance: ' + $Theme) | Out-Null
    Wait-Settings { param($settings) $settings.theme -eq $Theme.ToLowerInvariant() }
}

function Find-Control([string]$Name, [string]$Within = '') {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($script:window)
    if ($Within) {
        $container = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Within)
        $root = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $container)
        if ($null -eq $root) { return $null }
    }
    $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
}

function Wait-Control([string]$Name, [string]$Within = '', [int]$TimeoutSeconds = 60) {
    $deadline = [System.Diagnostics.Stopwatch]::StartNew()
    $stable = [System.Diagnostics.Stopwatch]::StartNew()
    $previousBounds = $null
    do {
        $element = Find-Control $Name $Within
        if ($null -ne $element -and -not $element.Current.IsOffscreen) {
            $bounds = $element.Current.BoundingRectangle
            if ($bounds -ne $previousBounds) {
                $previousBounds = $bounds
                $stable.Restart()
            } elseif ($stable.ElapsedMilliseconds -ge 150) { return $element }
        } else {
            $previousBounds = $null
            $stable.Restart()
        }
        if ($process.HasExited) { throw "Desktop exited with code $($process.ExitCode)." }
        [RegressifySmokeNative]::DwmFlush() | Out-Null
    } while ($deadline.Elapsed.TotalSeconds -lt $TimeoutSeconds)
    $pointer = [RegressifySmokePointer+Point]::new()
    if ([RegressifySmokePointer]::GetPhysicalCursorPos([ref]$pointer)) {
        Write-Output "Pointer at ($($pointer.X), $($pointer.Y)); fixture foreground: $([RegressifySmokeFocus]::GetForegroundWindow() -eq $script:window)."
        foreach ($target in @('Scenario information', 'Scenario information details')) {
            $control = Find-Control $target
            if ($null -ne $control) { Write-Output "Hover target '$target': $($control.Current.BoundingRectangle)." }
        }
    }
    Capture-Window 'missing-control'
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($script:window)
    $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) |
        Where-Object { $_.Current.ControlType -eq [System.Windows.Automation.ControlType]::MenuItem } |
        ForEach-Object { Write-Output ('Available menu item: ' + $_.Current.Name) }
    Capture-Window 'control-failure'
    throw "Native control '$Name' was not found."
}

function Test-ControlInvokable([string]$Name) {
    $pattern = $null
    (Wait-Control $Name).TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)
}

function Test-MenuActionEnabled([string]$Name) {
    $element = Wait-Control $Name
    $element.Current.ControlType -eq [System.Windows.Automation.ControlType]::MenuItem -and $element.Current.IsEnabled
}

function Send-FixturePointerClick([switch]$Right) {
    $down = if ($Right) { 8 } else { 2 }
    $up = if ($Right) { 16 } else { 4 }
    [RegressifySmokeNative]::DwmFlush() | Out-Null
    [RegressifySmokeNative]::mouse_event($down, 0, 0, 0, [UIntPtr]::Zero)
    try {
        [RegressifySmokeNative]::DwmFlush() | Out-Null
    } finally {
        [RegressifySmokeNative]::mouse_event($up, 0, 0, 0, [UIntPtr]::Zero)
    }
    [RegressifySmokeNative]::DwmFlush() | Out-Null
}

function Initialize-FixtureClipboard([switch]$OverwriteClipboard) {
    if (-not $OverwriteClipboard) {
        $script:fixtureClipboardBackup = Invoke-FixtureClipboard { Copy-FixtureClipboardData ([System.Windows.Forms.Clipboard]::GetDataObject()) }
    }
    # Own cleanup before SetText in case initialization partially changes the clipboard.
    $script:fixtureClipboardOwned = $true
    Invoke-FixtureClipboard { [System.Windows.Forms.Clipboard]::SetText('Regressify smoke fixture') }
}

function Restore-FixtureClipboard {
    if (-not $script:fixtureClipboardOwned) { return }
    Invoke-FixtureClipboard {
        if ($null -ne $script:fixtureClipboardBackup) { [System.Windows.Forms.Clipboard]::SetDataObject($script:fixtureClipboardBackup, $true) }
        else { [System.Windows.Forms.Clipboard]::Clear() }
    }
    $script:fixtureClipboardOwned = $false
    $script:fixtureClipboardBackup = $null
}

function Copy-FixtureClipboardData($Source, [switch]$FixtureOwned) {
    if ($null -eq $Source) { return $null }
    $formats = @($Source.GetFormats($false))
    if ($formats.Count -eq 0) { return $null }
    $snapshot = [System.Windows.Forms.DataObject]::new()
    foreach ($format in $formats) {
        $value = $Source.GetData($format, $false)
        if ($null -eq $value) {
            if ($FixtureOwned) { continue }
            throw 'A clipboard format could not be backed up; clipboard tests were not started.'
        }
        if ($value -is [System.IO.Stream]) {
            if (-not $value.CanRead -or -not $value.CanSeek -or $value.Length -gt 64MB) {
                throw 'A clipboard stream cannot be safely backed up; clipboard tests were not started.'
            }
            $stream = $value
            $position = $stream.Position
            $copy = [System.IO.MemoryStream]::new()
            try {
                $stream.Position = 0
                $stream.CopyTo($copy)
                $copy.Position = 0
                $value = $copy
            } catch {
                $copy.Dispose()
                throw
            } finally { $stream.Position = $position }
        } elseif ($value -is [System.ICloneable]) {
            $value = $value.Clone()
        } elseif (-not $value.GetType().IsValueType) {
            throw 'A clipboard format cannot be independently backed up; clipboard tests were not started.'
        }
        $snapshot.SetData($format, $false, $value)
    }
    if ($snapshot.GetFormats($false).Length -eq 0) { return $null }
    return $snapshot
}

function Invoke-FixtureClipboard([scriptblock]$Operation, [ValidateRange(0, 10000)][int]$TimeoutMilliseconds = 2000) {
    $deadline = [System.Diagnostics.Stopwatch]::StartNew()
    do {
        try { return (& $Operation) } catch {
            $cause = $_.Exception.GetBaseException()
            if ($cause -isnot [System.Runtime.InteropServices.ExternalException] -or
                $cause.HResult -ne -2147221040 -or
                $deadline.ElapsedMilliseconds -ge $TimeoutMilliseconds) {
                Write-Warning ('Clipboard access failed after {0}ms ({1}, HRESULT 0x{2:X8}).' -f $deadline.ElapsedMilliseconds, $cause.GetType().FullName, $cause.HResult)
                throw
            }
            [RegressifySmokeNative]::DwmFlush() | Out-Null
        }
    } while ($true)
}

function Wait-FixtureClipboardText([uint32]$PreviousSequence, [string]$Expected, [string]$Failure) {
    Wait-WindowState {
        if ([RegressifySmokeClipboard]::GetClipboardSequenceNumber() -eq $PreviousSequence) { return $false }
        # EmptyClipboard can advance the sequence before the text is available.
        (Invoke-FixtureClipboard { [System.Windows.Forms.Clipboard]::GetText() }) -ceq $Expected
    } $Failure
}

function Invoke-Control([string]$Name, [switch]$Pointer, [string]$Within = '') {
    $element = Wait-Control $Name $Within
    $pattern = $null
    if (-not $Pointer -and $element.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
        ([System.Windows.Automation.InvokePattern]$pattern).Invoke()
    } elseif (-not $Pointer -and $element.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern)) {
        ([System.Windows.Automation.SelectionItemPattern]$pattern).Select()
    } elseif (-not $Pointer -and $element.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$pattern)) {
        ([System.Windows.Automation.TogglePattern]$pattern).Toggle()
    } else {
        [RegressifySmokeNative]::SetWindowPos($script:window, [IntPtr]::Zero, 0, 0, 0, 0, 0x0043) | Out-Null
        Focus-Fixture
        $bounds = $element.Current.BoundingRectangle
        if (-not [RegressifySmokePointer]::SetPhysicalCursorPos([int]($bounds.Left + $bounds.Width / 2), [int]($bounds.Top + $bounds.Height / 2))) {
            throw "Cannot position the mouse over '$Name'."
        }
        Send-FixturePointerClick
        return
    }
    [RegressifySmokeNative]::DwmFlush() | Out-Null
}

function Hover-Control([string]$Name) {
    $bounds = (Wait-Control $Name).Current.BoundingRectangle
    Focus-Fixture
    if (-not [RegressifySmokePointer]::SetPhysicalCursorPos([int]($bounds.Left + $bounds.Width / 2), [int]($bounds.Top + $bounds.Height / 2))) {
        throw "Cannot hover '$Name'."
    }
    [RegressifySmokeNative]::DwmFlush() | Out-Null
}

function Read-ControlText([string]$Name) {
    $element = Wait-Control $Name
    $pattern = $null
    if ($element.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$pattern)) {
        return ([System.Windows.Automation.ValuePattern]$pattern).Current.Value
    }
    if ($element.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern, [ref]$pattern)) {
        return ([System.Windows.Automation.TextPattern]$pattern).DocumentRange.GetText(-1)
    }
    throw "Control '$Name' does not expose accessible text."
}

function Wait-SourceText([string]$Text, [string]$ControlName = 'Configuration source') {
    $deadline = [System.Diagnostics.Stopwatch]::StartNew()
    do {
        $value = Read-ControlText $ControlName
        if ($value.Contains($Text)) { return }
        [RegressifySmokeNative]::DwmFlush() | Out-Null
    } while ($deadline.Elapsed.TotalSeconds -lt 10)
    throw "Control '$ControlName' did not contain '$Text'."
}

function Write-FixtureControlText([string]$Name, [string]$Text) {
    Invoke-Control $Name -Pointer
    Wait-Control $Name | Out-Null
    Send-FixtureKeys A -Control
    Send-FixtureText $Text
    try { Wait-SourceText $Text $Name } catch {
        Capture-Window 'text-input-failure'
        throw
    }
}

function Test-ApplicationMenus {
    $metadata = & cargo metadata --no-deps --format-version 1 --manifest-path (Join-Path $repository 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw 'Cannot determine the expected desktop version.' }
    $version = (($metadata | ConvertFrom-Json).packages | Where-Object name -eq 'regressify').version
    $versionBounds = (Wait-Control "v$version").Current.BoundingRectangle
    $fileBounds = (Wait-Control 'File').Current.BoundingRectangle
    $statusBounds = (Wait-Control 'Application status').Current.BoundingRectangle
    if ([Math]::Abs($versionBounds.Top - $fileBounds.Top) -gt 20 -or [Math]::Abs($statusBounds.Top - $fileBounds.Top) -gt 20) {
        throw 'Version and application status must be in the title bar, not a bottom status bar.'
    }
    Invoke-Control 'File' -Pointer
    foreach ($name in @('Open in VS Code', 'Open in File Explorer')) {
        if (-not (Test-MenuActionEnabled $name)) { throw "Workspace action '$name' is unavailable." }
    }
    Capture-Window 'file-external-workspace-actions'
    Send-FixtureKeys Escape
    Invoke-Control 'Help' -Pointer
    Invoke-Control 'About' -Pointer
    Wait-Control 'Regressify Desktop' | Out-Null
    Wait-Control "Version: $version" | Out-Null
    Wait-Control 'Author and lead developer: Tuyen Pham' | Out-Null
    Wait-Control 'Developer and docs maintainer: Dinh Pham' | Out-Null
    Wait-Control 'Developers: Hung Le, Dung Nguyen Tien' | Out-Null
    Wait-Control 'Advisors (QA): Nga Nguyen, Hanh Nguyen, Duyen Pham' | Out-Null
    if (-not (Test-ControlInvokable 'https://regressify.eshn.dev/')) { throw 'About website must be an external link.' }
    Wait-Control 'This program comes with ABSOLUTELY NO WARRANTY. It is free software; you may redistribute it under the terms of the license below.' | Out-Null
    if (-not (Test-ControlInvokable 'GNU AGPL v3 or later')) { throw 'About license must be an external link.' }
    if (-not (Test-ControlInvokable 'https://github.com/precise-alloy/regressify-rs')) { throw 'About source must be an external link.' }
    Capture-Window 'help-about'
    Invoke-Control 'Close' -Within 'About actions' -Pointer
    Wait-WindowState { $null -eq (Find-Control "Version: $version") } 'About dialog did not close.'
}
