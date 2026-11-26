function Test-InstanceFlows {
    if (-not ('RegressifyInstanceWindow' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class RegressifyInstanceWindow {
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr window);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr window, int command);
}
'@
    }
    function Wait-OwnerForeground([System.Diagnostics.Process]$Owner) {
        $deadline = [System.Diagnostics.Stopwatch]::StartNew()
        do {
            if ($Owner.HasExited) { throw 'The workspace owner exited during activation.' }
            $Owner.Refresh()
            $handle = $Owner.MainWindowHandle
            if ($handle -ne [IntPtr]::Zero -and [RegressifySmokeFocus]::GetForegroundWindow() -eq $handle -and -not [RegressifyInstanceWindow]::IsIconic($handle)) { return }
            [RegressifySmokeNative]::DwmFlush() | Out-Null
        } while ($deadline.Elapsed.TotalSeconds -lt 10)
        throw 'The existing workspace window was not restored and foregrounded.'
    }
    function Open-Duplicate([string]$Root, [System.Diagnostics.Process]$Owner, [string]$Name) {
        $duplicate = Start-Process -FilePath $executable -ArgumentList ('"' + $Root + '"') -PassThru `
            -RedirectStandardOutput (Join-Path $artifacts ($Name + '-stdout.log')) -RedirectStandardError (Join-Path $artifacts ($Name + '-stderr.log'))
        $script:fixtureProcesses.Add($duplicate)
        if (-not $duplicate.WaitForExit(15000)) { throw 'A duplicate launcher did not exit after workspace handoff.' }
        if ($duplicate.ExitCode -ne 0) { throw "Duplicate launcher failed with exit code $($duplicate.ExitCode)." }
        Wait-OwnerForeground $Owner
    }
    $first = $process
    $source = Join-Path $workspace 'visual_tests/smoke.tests.json'
    $originalSource = (Get-FileHash $source).Hash
    Invoke-Control 'smoke.tests.json'
    Invoke-Control 'Checkout'
    Invoke-Control 'Duplicate selected item'
    Wait-Control 'Unsaved changes' | Out-Null
    Capture-Window 'instances-first-draft'
    $secondRoot = Join-Path $artifacts ('workspace-' + [guid]::NewGuid().ToString('N'))
    Write-JsonFixture (Join-Path $secondRoot 'visual_tests/second.tests.json') @{ scenarios = @(@{ label = 'Second workspace'; url = 'about:blank' }) }
    Start-Fixture -Root $secondRoot -Name 'instance-second'
    $second = $process
    if ($first.HasExited) { throw 'Opening another workspace closed the first instance.' }
    Wait-Control ($secondRoot.Replace('\', '/')) | Out-Null
    Capture-Window 'instances-second-workspace'
    [RegressifyInstanceWindow]::ShowWindow($first.MainWindowHandle, 6) | Out-Null
    Open-Duplicate (Join-Path $workspace '.') $first 'instance-duplicate'
    $script:process = $first
    $script:window = $first.MainWindowHandle
    Wait-Control 'Unsaved changes' | Out-Null
    Capture-Window 'instances-duplicate-focused'
    if ((Get-FileHash $source).Hash -ne $originalSource) { throw 'Workspace handoff changed the saved document.' }
    $script:process = $second
    $script:window = $second.MainWindowHandle
    [RegressifyInstanceWindow]::ShowWindow($first.MainWindowHandle, 6) | Out-Null
    Open-Duplicate $workspace.ToUpperInvariant() $first 'instance-case-alias'
    Start-Fixture -Empty -Name 'instance-empty'
    $empty = $process
    Invoke-Control (Split-Path -Leaf $workspace)
    Wait-Control 'Switched to the existing workspace window' | Out-Null
    Wait-OwnerForeground $first
    Capture-Window 'instances-empty-redirect'
    Start-Fixture -Empty -Name 'instance-empty-second'
    if ($empty.HasExited) { throw 'An empty desktop instance was globally restricted.' }
    Capture-Window 'instances-concurrent-empty'
    $first.Kill($true)
    $first.WaitForExit()
    Start-Fixture -Root $workspace -Name 'instance-crash-recovery'
    $recovered = $process
    if ($second.HasExited) { throw 'Recovering one workspace affected another instance.' }
    if ((Get-FileHash $source).Hash -ne $originalSource) { throw 'Recovery changed the saved source.' }
    Capture-Window 'instances-crash-recovery'
    Open-Duplicate $workspace $recovered 'instance-recovered-activation'
    Invoke-Control 'File' -Pointer
    Invoke-Control 'Close workspace'
    Wait-Control 'Workspace closed' | Out-Null
    Start-Fixture -Root $workspace -Name 'instance-reopened'
    if ($recovered.HasExited) { throw 'Closing a workspace should leave its window available.' }
    Capture-Window 'instances-reopened-workspace'
    Write-Output 'Multiple workspace/empty windows, duplicate activation, minimized restoration, draft preservation, and close/crash ownership release passed.'
}
