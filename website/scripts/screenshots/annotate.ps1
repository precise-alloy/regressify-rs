# Bakes numbered callouts and highlight rings into documentation screenshots.
# Manifest: [{ "file": "x", "callouts": [{ "n": 1, "key": "Run Test" }], "crop": null | "x,y,w,h" | "auto" | "auto:160" }]
# Rects come from raw/<file>.json written by shot.ps1 at capture time, so
# recapturing after a UI change repositions every callout automatically.
param([string]$Manifest = (Join-Path $PSScriptRoot 'manifest.json'), [string]$OutDir = (Join-Path $PSScriptRoot '../../src/assets/docs/desktop'), [string]$Only = '')
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$root = if ($env:REGRESSIFY_SHOTS) { $env:REGRESSIFY_SHOTS } else { Join-Path ([IO.Path]::GetTempPath()) 'regressify-shots' }
# Rings use the brand violet, which stays distinct from the app's magenta diff
# pixels; badges carry the full brand gradient (coral -> magenta -> violet).
$accent = [System.Drawing.Color]::FromArgb(255, 122, 126, 250)
$brand = @([System.Drawing.Color]::FromArgb(255, 255, 122, 89), [System.Drawing.Color]::FromArgb(255, 255, 0, 255), [System.Drawing.Color]::FromArgb(255, 122, 126, 250))
$halo = [System.Drawing.Color]::FromArgb(200, 10, 10, 10)

function Rounded([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
  $p = New-Object System.Drawing.Drawing2D.GraphicsPath
  $d = $r * 2
  $p.AddArc($x, $y, $d, $d, 180, 90); $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
  $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90); $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
  $p.CloseFigure(); $p
}

foreach ($item in (Get-Content $Manifest -Raw -Encoding UTF8 | ConvertFrom-Json)) {
  if ($Only -and $item.file -ne $Only) { continue }
  $rawName = if ($item.raw) { $item.raw } else { $item.file }
  $raw = Join-Path $root "raw/$rawName.png"
  $rects = Get-Content ([IO.Path]::ChangeExtension($raw, '.json')) -Raw | ConvertFrom-Json
  $src = [System.Drawing.Bitmap]::FromFile($raw); $bmp = New-Object System.Drawing.Bitmap $src; $src.Dispose()
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = 'AntiAlias'; $g.TextRenderingHint = 'AntiAliasGridFit'
  $font = New-Object System.Drawing.Font 'Segoe UI', 13, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
  # Masks replace machine-specific text (user paths) before any callout is drawn.
  foreach ($m in @($item.masks | Where-Object { $_ })) {
    $bg = $bmp.GetPixel($m.x - 4, $m.y + [int]($m.h / 2))
    $g.FillRectangle((New-Object System.Drawing.SolidBrush $bg), $m.x, $m.y, $m.w, $m.h)
    $mf = New-Object System.Drawing.Font 'Segoe UI', ([float]$m.size), ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
    $g.DrawString($m.text, $mf, (New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 220, 220, 220))), $m.x - 2, $m.y + 1)
  }
  $boxes = @(); $rings = @()
  foreach ($c in $item.callouts) {
    # "rect:x,y,w,h" marks areas that expose no accessible name (pane headers, popups).
    if ($c.key -like 'rect:*') { $v = $c.key.Substring(5) -split ','; $r = [pscustomobject]@{ x = [int]$v[0]; y = [int]$v[1]; w = [int]$v[2]; h = [int]$v[3] } }
    else { $r = $rects.($c.key) }
    if ($null -eq $r) { throw "$($item.file): no rect for '$($c.key)'" }
    $boxes += $r
    $pad = 4; $rings += [pscustomobject]@{ n = $c.n; x = $r.x - $pad; y = $r.y - $pad; w = $r.w + 2 * $pad; h = $r.h + 2 * $pad }
  }
  # Pass 1: rings. Pass 2: badges on top, so a neighbouring ring never hides a number.
  foreach ($q in $rings) {
    $path = Rounded $q.x $q.y $q.w $q.h 8
    $g.DrawPath((New-Object System.Drawing.Pen $halo, 5), $path)
    $g.DrawPath((New-Object System.Drawing.Pen $accent, 2.5), $path)
  }
  $d = 24; $placed = @()
  $fmt = New-Object System.Drawing.StringFormat; $fmt.Alignment = 'Center'; $fmt.LineAlignment = 'Center'
  foreach ($q in $rings) {
    # Prefer the top-left corner; fall back to other corners when a badge is already there.
    $corners = @(@($q.x, $q.y), @(($q.x + $q.w), $q.y), @($q.x, ($q.y + $q.h)), @(($q.x + $q.w), ($q.y + $q.h)))
    $best = $null
    foreach ($cn in $corners) {
      $bx = [Math]::Max(2, [Math]::Min($bmp.Width - $d - 2, $cn[0] - $d / 2)); $by = [Math]::Max(2, [Math]::Min($bmp.Height - $d - 2, $cn[1] - $d / 2))
      $clash = $placed | Where-Object { [Math]::Abs($_[0] - $bx) -lt ($d + 2) -and [Math]::Abs($_[1] - $by) -lt ($d + 2) }
      if (-not $clash) { $best = @($bx, $by); break }
      if ($null -eq $best) { $best = @($bx, $by) }
    }
    $bx = $best[0]; $by = $best[1]; $placed += ,@($bx, $by)
    $g.FillEllipse((New-Object System.Drawing.SolidBrush $halo), $bx - 2, $by - 2, $d + 4, $d + 4)
    $grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush((New-Object System.Drawing.RectangleF $bx, $by, $d, $d), $brand[0], $brand[2], [float]135)
    $blend = New-Object System.Drawing.Drawing2D.ColorBlend 4; $blend.Colors = @($brand[0], $brand[1], $brand[1], $brand[2]); $blend.Positions = [float[]]@(0, 0.42, 0.64, 1); $grad.InterpolationColors = $blend
    $g.FillEllipse($grad, $bx, $by, $d, $d)
    # A 1px dark offset keeps white digits legible over the light coral end.
    $g.DrawString([string]$q.n, $font, (New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(150, 0, 0, 0))), (New-Object System.Drawing.RectangleF ($bx + 1), ($by + 1.5), $d, $d), $fmt)
    $g.DrawString([string]$q.n, $font, [System.Drawing.Brushes]::White, (New-Object System.Drawing.RectangleF $bx, ($by + 0.5), $d, $d), $fmt)
  }
  $g.Dispose()
  $out = $bmp
  if ($item.crop) {
    if ($item.crop -like 'auto*') {
      $padC = if ($item.crop -match ':(\d+)$') { [int]$Matches[1] } else { 140 }
      $l = ($boxes | ForEach-Object { $_.x } | Measure-Object -Minimum).Minimum - $padC
      $t = ($boxes | ForEach-Object { $_.y } | Measure-Object -Minimum).Minimum - $padC
      $rr = ($boxes | ForEach-Object { $_.x + $_.w } | Measure-Object -Maximum).Maximum + $padC
      $bb = ($boxes | ForEach-Object { $_.y + $_.h } | Measure-Object -Maximum).Maximum + $padC
      $cx = [Math]::Max(0, $l); $cy = [Math]::Max(0, $t)
      $rect = New-Object System.Drawing.Rectangle $cx, $cy, ([Math]::Min($bmp.Width, $rr) - $cx), ([Math]::Min($bmp.Height, $bb) - $cy)
    } else { $v = $item.crop -split ','; $rect = New-Object System.Drawing.Rectangle ([int]$v[0]), ([int]$v[1]), ([int]$v[2]), ([int]$v[3]) }
    $out = $bmp.Clone($rect, $bmp.PixelFormat); $bmp.Dispose()
  }
  $dest = Join-Path $OutDir "$($item.file).png"; $out.Save($dest, [System.Drawing.Imaging.ImageFormat]::Png); $out.Dispose()
  "$($item.file): $($item.callouts.Count) callouts -> $dest"
}
