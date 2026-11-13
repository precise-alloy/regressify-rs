# Documentation screenshot helper for the Regressify desktop app; see
# .github/skills/docs-screenshots/SKILL.md. It snapshots and restores the per-user
# desktop folder so personal workspaces, targets, and settings never appear in
# captures or survive them. Prefer the accessibility actions (uinvoke, uvalue,
# utype): they need no foreground window and send no real pointer or key input.
param([string]$Action, [string]$Arg = '', [int]$X = 0, [int]$Y = 0, [string]$Names = '', [string]$Within = '', [int]$Index = 0)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing, System.Windows.Forms, UIAutomationClient, UIAutomationTypes
if (-not ('DocShot' -as [type])) {
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class DocShot {
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, IntPtr e);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, IntPtr extra);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, IntPtr pid);
  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
}
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path
# Captures, the demo workspace, and the app-data snapshot live outside the repository.
$root = if ($env:REGRESSIFY_SHOTS) { $env:REGRESSIFY_SHOTS } else { Join-Path ([IO.Path]::GetTempPath()) 'regressify-shots' }
$exe = if ($env:REGRESSIFY_DESKTOP_EXE) { $env:REGRESSIFY_DESKTOP_EXE } else { Join-Path $repo 'target/release/regressify-desktop.exe' }
$workspace = Join-Path $root 'northwind-qa'
$manifest = Join-Path $PSScriptRoot 'manifest.json'
$pidFile = Join-Path $root 'desktop.pid'
$settings = Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'Regressify/Regressify Desktop/config/settings.json'
$appDir = Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'Regressify/Regressify Desktop'
$snapshot = Join-Path $root 'appdata-snapshot'


# Finds visible controls by exact accessible name (optionally inside a named container),
# the same lookup the native smoke tests use.
function Find-Named($h, [string]$Name, [string]$Container = '') {
  $root = [System.Windows.Automation.AutomationElement]::FromHandle($h)
  if ($Container) {
    $c = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Container)
    $root = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c)
    if ($null -eq $root) { return @() }
  }
  $cond = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
  @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond) | Where-Object { -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Width -gt 0 })
}
function Rel-Rect($h, $el) {
  $r = New-Object DocShot+RECT; [DocShot]::GetWindowRect($h, [ref]$r) | Out-Null
  $b = $el.Current.BoundingRectangle
  [ordered]@{ x = [int]($b.X - $r.Left); y = [int]($b.Y - $r.Top); w = [int]$b.Width; h = [int]$b.Height }
}

function Get-Window {
  $p = Get-Process regressify-desktop -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
  if (-not $p) { throw 'Desktop window not found.' }
  $p.MainWindowHandle
}
# Input must only ever reach the desktop app. Keep it topmost for the session and
# refuse to send clicks or keys unless it really is the foreground window.
function Focus-Window($h) {
  [DocShot]::ShowWindow($h, 9) | Out-Null
  [DocShot]::SetWindowPos($h, [IntPtr](-1), 0, 0, 0, 0, 0x0003) | Out-Null
  for ($i = 0; $i -lt 10; $i++) {
    if ([DocShot]::GetForegroundWindow() -eq $h) { Start-Sleep -Milliseconds 200; return }
    # A bare Alt tap lifts the foreground lock, as the smoke harness relies on AppActivate.
    [DocShot]::keybd_event(0x12, 0, 0, [IntPtr]::Zero); [DocShot]::keybd_event(0x12, 0, 2, [IntPtr]::Zero)
    [DocShot]::SetForegroundWindow($h) | Out-Null
    # Windows only lets the foreground thread hand over focus; borrow its input queue.
    $fg = [DocShot]::GetWindowThreadProcessId([DocShot]::GetForegroundWindow(), [IntPtr]::Zero); $me = [DocShot]::GetCurrentThreadId()
    if ($fg -ne $me) { [DocShot]::AttachThreadInput($me, $fg, $true) | Out-Null }
    [DocShot]::BringWindowToTop($h) | Out-Null; [DocShot]::SetForegroundWindow($h) | Out-Null
    if ($fg -ne $me) { [DocShot]::AttachThreadInput($me, $fg, $false) | Out-Null }
    if ($i -eq 5) { [DocShot]::ShowWindow($h, 6) | Out-Null; Start-Sleep -Milliseconds 300; [DocShot]::ShowWindow($h, 9) | Out-Null }
    (New-Object -ComObject WScript.Shell).AppActivate((Get-Process regressify-desktop | Select-Object -First 1).Id) | Out-Null
    Start-Sleep -Milliseconds 200
  }
  throw 'Desktop window is not in the foreground; refusing to send input.'
}

switch ($Action) {
  'start' {
    if (Get-Process regressify-desktop -ErrorAction SilentlyContinue) { throw 'Another Regressify Desktop instance is running.' }
    # Snapshot the whole per-user desktop folder (settings, landing apps, targets, token
    # catalogs, run snapshots) so stop can restore it exactly. stop treats the snapshot as the
    # authority and deletes files it lacks, so a copy becomes the snapshot only once complete;
    # an interrupted copy stays a discarded `.partial` folder. An existing snapshot is from a
    # session that never stopped, and still holds the originals.
    if (-not (Test-Path $snapshot)) {
      $partial = "$snapshot.partial"
      if (Test-Path $partial) { Remove-Item $partial -Recurse -Force }
      if (Test-Path $appDir) { Copy-Item $appDir $partial -Recurse }
      else { New-Item -ItemType Directory $partial | Out-Null; New-Item (Join-Path $partial '.absent') | Out-Null }
      Rename-Item $partial (Split-Path -Leaf $snapshot)
    }
    # Captures start from empty app data, so no personal landing apps, token catalogs, targets,
    # or recent workspaces can appear in an image; stop restores everything from the snapshot.
    if (Test-Path $appDir) { Get-ChildItem $appDir -Force | Remove-Item -Recurse -Force }
    if ($Arg) { New-Item -ItemType Directory -Force (Split-Path $settings) | Out-Null; [IO.File]::WriteAllText($settings, $Arg) }
    New-Item -ItemType Directory -Force $root | Out-Null
    # Windows passes one command-line string; quoting keeps a workspace path with spaces as a
    # single argument, and Windows paths cannot contain quotes. No shell is involved.
    $proc = Start-Process -FilePath $exe -ArgumentList "`"$workspace`"" -PassThru
    Set-Content -Encoding ASCII $pidFile $proc.Id
    for ($i = 0; $i -lt 60; $i++) { try { $h = Get-Window; break } catch { Start-Sleep -Milliseconds 500 } }
    [DocShot]::MoveWindow($h, 40, 40, 1440, 900, $true) | Out-Null
    Start-Sleep -Seconds 3; 'started'
  }
  'keys' { $h = Get-Window; Focus-Window $h; [System.Windows.Forms.SendKeys]::SendWait($Arg); Start-Sleep -Milliseconds 1200; 'sent' }
  'click' {
    $h = Get-Window; Focus-Window $h; $r = New-Object DocShot+RECT; [DocShot]::GetWindowRect($h, [ref]$r) | Out-Null
    [DocShot]::SetCursorPos($r.Left + $X, $r.Top + $Y) | Out-Null; Start-Sleep -Milliseconds 150
    if ($Arg -eq 'right') { [DocShot]::mouse_event(8, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 60; [DocShot]::mouse_event(16, 0, 0, 0, [IntPtr]::Zero) }
    else { [DocShot]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 60; [DocShot]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero) }
    if ($Arg -eq 'double') { Start-Sleep -Milliseconds 80; [DocShot]::mouse_event(2,0,0,0,[IntPtr]::Zero); [DocShot]::mouse_event(4,0,0,0,[IntPtr]::Zero) }
    if ($Arg -ne 'stay') { Start-Sleep -Milliseconds 100; [DocShot]::SetCursorPos($r.Left + 720, $r.Top + 4) | Out-Null }
    Start-Sleep -Milliseconds 1200; 'clicked'
  }
  # Drags from (X, Y) to "x,y" in window coordinates, e.g. to resize split panes.
  'drag' {
    $h = Get-Window; Focus-Window $h; $r = New-Object DocShot+RECT; [DocShot]::GetWindowRect($h, [ref]$r) | Out-Null
    $to = $Arg -split ','
    [DocShot]::SetCursorPos($r.Left + $X, $r.Top + $Y) | Out-Null; Start-Sleep -Milliseconds 150
    [DocShot]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 150
    for ($i = 1; $i -le 10; $i++) {
      [DocShot]::SetCursorPos($r.Left + $X + [int](([int]$to[0] - $X) * $i / 10), $r.Top + $Y + [int](([int]$to[1] - $Y) * $i / 10)) | Out-Null; Start-Sleep -Milliseconds 40
    }
    [DocShot]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 100
    [DocShot]::SetCursorPos($r.Left + 720, $r.Top + 4) | Out-Null; Start-Sleep -Milliseconds 800; 'dragged'
  }
  # Waits (up to 10 minutes) until the user brings the app to the foreground.
  'waitfocus' {
    $h = Get-Window
    for ($i = 0; $i -lt 1200; $i++) { if ([DocShot]::GetForegroundWindow() -eq $h) { Start-Sleep -Milliseconds 500; 'focused'; return }; Start-Sleep -Milliseconds 500 }
    throw 'The desktop app was not brought to the foreground.'
  }
  # Accessibility actions: no pointer or keyboard input, so the window need not be in front.
  'uinvoke' {
    $h = Get-Window; $els = Find-Named $h $Arg $Within
    if ($els.Count -le $Index) { throw "Control not found: $Arg" }
    $el = $els[$Index]; $pattern = $null
    if ($el.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) { $pattern.Invoke() }
    elseif ($el.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$pattern)) { $pattern.Toggle() }
    elseif ($el.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern)) { $pattern.Select() }
    elseif ($el.TryGetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern, [ref]$pattern)) { $pattern.Expand() }
    else { throw "No invoke pattern: $Arg ($(($el.GetSupportedPatterns() | ForEach-Object { $_.ProgrammaticName }) -join ', '))" }
    Start-Sleep -Milliseconds 1200; "uinvoked $Arg"
  }
  'uvalue' {
    $h = Get-Window; $els = Find-Named $h $Within
    if ($els.Count -le $Index) { throw "Control not found: $Within" }
    $pattern = $null
    if (-not $els[$Index].TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$pattern)) { throw "No value pattern: $Within" }
    $pattern.SetValue($Arg); Start-Sleep -Milliseconds 600; "set $Within"
  }
  # Focuses a field through accessibility and posts characters to the app's own queue,
  # for masked fields that expose no value pattern. Nothing reaches other windows.
  'utype' {
    $h = Get-Window; $els = Find-Named $h $Within
    if ($els.Count -le $Index) { throw "Control not found: $Within" }
    $els[$Index].SetFocus(); Start-Sleep -Milliseconds 400
    foreach ($ch in $Arg.ToCharArray()) { [DocShot]::PostMessage($h, 0x0102, [IntPtr][int]$ch, [IntPtr]1) | Out-Null; Start-Sleep -Milliseconds 30 }
    Start-Sleep -Milliseconds 600; "typed into $Within"
  }
  'patterns' {
    $h = Get-Window; foreach ($el in (Find-Named $h $Arg $Within)) { "$($el.Current.ControlType.ProgrammaticName): $(($el.GetSupportedPatterns() | ForEach-Object { $_.ProgrammaticName }) -join ', ')" }
  }
  'size' { $h = Get-Window; [DocShot]::MoveWindow($h, 40, 40, 1440, 900, $true) | Out-Null; Start-Sleep -Milliseconds 1200; 'sized' }
  'park' { $h = Get-Window; $r = New-Object DocShot+RECT; [DocShot]::GetWindowRect($h, [ref]$r) | Out-Null; [DocShot]::SetCursorPos($r.Left + 720, $r.Top + 4) | Out-Null; Start-Sleep -Milliseconds 800; 'parked' }
  'dump' {
    $h = Get-Window; $root = [System.Windows.Automation.AutomationElement]::FromHandle($h)
    $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    foreach ($el in $all) { $n = $el.Current.Name; if ($n -and -not $el.Current.IsOffscreen) { $r = Rel-Rect $h $el; if ($r.w -gt 0) { "{0,-28} {1,-50} x={2} y={3} w={4} h={5}" -f $el.Current.ControlType.ProgrammaticName.Replace('ControlType.',''), $n, $r.x, $r.y, $r.w, $r.h } } }
  }
  'find' {
    $h = Get-Window; $i = 0
    foreach ($el in (Find-Named $h $Arg $Within)) { $r = Rel-Rect $h $el; "[$i] $($el.Current.ControlType.ProgrammaticName) x=$($r.x) y=$($r.y) w=$($r.w) h=$($r.h)"; $i++ }
    if ($i -eq 0) { "not found: $Arg" }
  }
  'invoke' {
    $h = Get-Window; $els = Find-Named $h $Arg $Within
    if ($els.Count -le $Index) { throw "Control not found: $Arg" }
    $r = Rel-Rect $h $els[$Index]; Focus-Window $h
    $w = New-Object DocShot+RECT; [DocShot]::GetWindowRect($h, [ref]$w) | Out-Null
    [DocShot]::SetCursorPos($w.Left + $r.x + [int]($r.w / 2), $w.Top + $r.y + [int]($r.h / 2)) | Out-Null; Start-Sleep -Milliseconds 150
    [DocShot]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 60; [DocShot]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero)
    # Park the pointer in the title bar so hover states and tooltips do not leak into captures.
    Start-Sleep -Milliseconds 100; [DocShot]::SetCursorPos($w.Left + 720, $w.Top + 4) | Out-Null
    Start-Sleep -Milliseconds 1200; "invoked $Arg"
  }
  'shot' {
    $h = Get-Window; $r = New-Object DocShot+RECT; [DocShot]::GetWindowRect($h, [ref]$r) | Out-Null
    $w = $r.Right - $r.Left; $ht = $r.Bottom - $r.Top
    $bmp = New-Object System.Drawing.Bitmap $w, $ht; $g = [System.Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc()
    try { if (-not [DocShot]::PrintWindow($h, $dc, 2)) { throw 'PrintWindow failed.' } } finally { $g.ReleaseHdc($dc); $g.Dispose() }
    $path = Join-Path $root "raw/$Arg.png"; New-Item -ItemType Directory -Force (Split-Path $path) | Out-Null
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
    # Names: "A|B|C", each optionally "Name@index" or "Name>Container". Without -Names,
    # every accessible-name key the manifest annotates on this capture is located.
    if (-not $Names -and (Test-Path $manifest)) {
      $keys = foreach ($item in (Get-Content $manifest -Raw -Encoding UTF8 | ConvertFrom-Json)) {
        $raw = if ($item.raw) { $item.raw } else { $item.file }
        if ($raw -eq $Arg) { $item.callouts | Where-Object { $_.key -notlike 'rect:*' } | ForEach-Object { $_.key } }
      }
      $Names = (@($keys) | Select-Object -Unique) -join '|'
    }
    $rects = [ordered]@{}
    foreach ($spec in ($Names -split '\|' | Where-Object { $_ })) {
      $name = $spec; $idx = 0; $cont = ''
      if ($name -match '^(.*)@(\d+)$') { $name = $Matches[1]; $idx = [int]$Matches[2] }
      if ($name -match '^(.*)>(.*)$') { $name = $Matches[1]; $cont = $Matches[2] }
      $els = Find-Named $h $name $cont
      $rects[$spec] = if ($els.Count -gt $idx) { Rel-Rect $h $els[$idx] } else { $null }
    }
    ($rects | ConvertTo-Json -Depth 4) | Set-Content -Encoding UTF8 ([IO.Path]::ChangeExtension($path, '.json'))
    "$path ${w}x${ht}"; foreach ($k in $rects.Keys) { if ($null -eq $rects[$k]) { "MISSING: $k" } }
  }
  'stop' {
    $owned = if (Test-Path $pidFile) { [int](Get-Content $pidFile) } else { -1 }
    Get-Process regressify-desktop -ErrorAction SilentlyContinue | Where-Object { $_.Id -eq $owned } | ForEach-Object { if ($_.MainWindowHandle -ne 0) { [DocShot]::SetWindowPos($_.MainWindowHandle, [IntPtr](-2), 0, 0, 0, 0, 0x0003) | Out-Null }; $_.CloseMainWindow() | Out-Null; if (-not $_.WaitForExit(5000)) { $_.Kill() } }
    Start-Sleep -Milliseconds 500
    Remove-Item $pidFile -ErrorAction SilentlyContinue
    if (Test-Path $snapshot) {
      if (Test-Path (Join-Path $snapshot '.absent')) { Remove-Item $appDir -Recurse -Force -ErrorAction SilentlyContinue }
      else {
        $before = @{}; Get-ChildItem $snapshot -Recurse -File | ForEach-Object { $before[$_.FullName.Substring($snapshot.Length)] = $_.FullName }
        Get-ChildItem $appDir -Recurse -File | ForEach-Object {
          $rel = $_.FullName.Substring((Resolve-Path $appDir).Path.Length)
          if (-not $before.ContainsKey($rel)) { Remove-Item $_.FullName -Force; "removed new file $rel" }
        }
        foreach ($rel in $before.Keys) {
          $dest = Join-Path $appDir $rel
          if (-not (Test-Path $dest) -or (Get-FileHash $dest).Hash -ne (Get-FileHash $before[$rel]).Hash) {
            New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null; Copy-Item $before[$rel] $dest -Force; "restored $rel"
          }
        }
      }
      Remove-Item $snapshot -Recurse -Force; 'app data restored'
    }
  }
}
