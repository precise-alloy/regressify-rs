function Test-ComparisonFlows {
    if (-not ('RegressifySmokeZoomKeys' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class RegressifySmokeZoomKeys {
    [DllImport("user32.dll")] public static extern void keybd_event(byte key, byte scan, uint flags, UIntPtr extra);
}
'@
    }
    function Write-ComparisonImage([string]$Path, [int]$Width, [int]$Height) {
        $bitmap = [System.Drawing.Bitmap]::new($Width, $Height)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            for ($row = 0; $row * 80 -lt $Height; $row++) {
                for ($column = 0; $column * 80 -lt $Width; $column++) {
                    $color = [System.Drawing.Color]::FromArgb(35 + ($column * 37) % 180, 35 + ($row * 43) % 180, 40 + (($row + $column) * 59) % 150)
                    $brush = [System.Drawing.SolidBrush]::new($color)
                    try { $graphics.FillRectangle($brush, $column * 80, $row * 80, 80, 80) } finally { $brush.Dispose() }
                }
            }
            $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
    }
    function Get-ComparisonBounds([string]$Pane) {
        $preview = (Wait-Control "$Pane image preview").Current.BoundingRectangle
        $canvas = (Wait-Control "$Pane image canvas").Current.BoundingRectangle
        return [System.Drawing.RectangleF]::Intersect(
            [System.Drawing.RectangleF]::new($preview.Left, $preview.Top, $preview.Width, $preview.Height),
            [System.Drawing.RectangleF]::new($canvas.Left, $canvas.Top, $canvas.Width, $canvas.Height))
    }
    function Move-ComparisonPointer([string]$Pane, [double]$HorizontalFraction = 0.43, [double]$VerticalFraction = 0.41) {
        $bounds = Get-ComparisonBounds $Pane
        Focus-Fixture
        $horizontal = [int]($bounds.Left + $bounds.Width * $HorizontalFraction)
        $vertical = [int]($bounds.Top + $bounds.Height * $VerticalFraction)
        if (-not [RegressifySmokePointer]::SetPhysicalCursorPos($horizontal, $vertical)) { throw 'Cannot position the comparison pointer.' }
        Wait-Control "$Pane image preview" | Out-Null
        return @(($horizontal - $bounds.Left), ($vertical - $bounds.Top))
    }
    function Test-GuideLine($Bitmap, $Bounds, [double]$Horizontal, [double]$Vertical, [bool]$Across) {
        for ($shift = -3; $shift -le 3; $shift++) {
            $white = 0
            $samples = 0
            $length = if ($Across) { $Bounds.Width } else { $Bounds.Height }
            for ($distance = 24; $distance -lt $length - 24; $distance += 23) {
                $column = [int]($Bounds.Left + $(if ($Across) { $distance } else { $Horizontal + $shift }))
                $row = [int]($Bounds.Top + $(if ($Across) { $Vertical + $shift } else { $distance }))
                $color = $Bitmap.GetPixel($column, $row)
                if ($color.R -ge 235 -and $color.G -ge 235 -and $color.B -ge 235) { $white++ }
                $samples++
            }
            if ($samples -gt 0 -and $white / $samples -ge 0.8) { return $true }
        }
        return $false
    }
    function Read-StableComparisonSample($Bitmap, $Bounds, [int]$Horizontal, [int]$Vertical) {
        for ($verticalShift = 0; $verticalShift -le 8; $verticalShift++) {
            for ($horizontalShift = 0; $horizontalShift -le 8; $horizontalShift++) {
                $colors = @()
                $stable = $true
                foreach ($preview in $Bounds) {
                    $column = [int]($preview.Left + $Horizontal + $horizontalShift)
                    $row = [int]($preview.Top + $Vertical + $verticalShift)
                    $color = $Bitmap.GetPixel($column, $row)
                    foreach ($neighbor in @(@(-2, 0), @(2, 0), @(0, -2), @(0, 2))) {
                        if ($Bitmap.GetPixel($column + $neighbor[0], $row + $neighbor[1]).ToArgb() -ne $color.ToArgb()) { $stable = $false }
                    }
                    $colors += $color
                }
                if ($stable) { return $colors }
            }
        }
        throw 'Comparison pixel probe did not find a stable tile interior.'
    }
    function Assert-ComparisonPixels([string]$Name, [string]$Pane = 'Reference', [switch]$HideGuides, [double]$HorizontalFraction = 0.43, [double]$VerticalFraction = 0.41) {
        $pointer = Move-ComparisonPointer $Pane $HorizontalFraction $VerticalFraction
        $native = [RegressifySmokeNative+Rect]::new()
        if (-not [RegressifySmokeNative]::GetWindowRect($script:window, [ref]$native)) { throw 'Cannot read comparison window bounds.' }
        $bounds = @('Reference', 'Test', 'Diff') | ForEach-Object {
            $preview = Get-ComparisonBounds $_
            [System.Drawing.RectangleF]::new($preview.Left - $native.Left, $preview.Top - $native.Top, $preview.Width, $preview.Height)
        }
        $lanes = @('Reference', 'Test', 'Diff') | ForEach-Object {
            $preview = (Wait-Control "$_ image preview").Current.BoundingRectangle
            [System.Drawing.RectangleF]::new($preview.Left - $native.Left, $preview.Top - $native.Top, $preview.Width, $preview.Height)
        }
        if ($HideGuides) {
            $header = (Wait-Control 'Results').Current.BoundingRectangle
            [RegressifySmokePointer]::SetPhysicalCursorPos([int]($header.Left + $header.Width / 2), [int]($header.Top + $header.Height / 2)) | Out-Null
            Wait-Control 'Results' | Out-Null
        }
        Capture-Window $Name
        $bitmap = [System.Drawing.Bitmap]::FromFile((Join-Path $artifacts "$Name.png"))
        try {
            for ($index = 0; $index -lt $bounds.Count; $index++) {
                $preview = if ($HideGuides) { $bounds[$index] } else { $lanes[$index] }
                $horizontal = $pointer[0] + $bounds[$index].Left - $preview.Left
                $vertical = $pointer[1] + $bounds[$index].Top - $preview.Top
                foreach ($across in @($false, $true)) {
                    $visible = Test-GuideLine $bitmap $preview $horizontal $vertical $across
                    if ($visible -eq $HideGuides.IsPresent) { throw "Pointer guide visibility is incorrect in lane $index of '$Name' (horizontal: $across)." }
                }
            }
            $matches = 0
            $signature = @()
            foreach ($verticalFraction in @(0.15, 0.35, 0.55)) {
                foreach ($horizontalFraction in @(0.15, 0.35, 0.55)) {
                    $vertical = [int]($bounds[0].Height * $verticalFraction)
                    $horizontal = [int]($bounds[0].Width * $horizontalFraction)
                    $sample = Read-StableComparisonSample $bitmap $bounds $horizontal $vertical
                    $reference = $sample[0]
                    foreach ($compared in $sample[1..($sample.Count - 1)]) {
                        if ([Math]::Abs([int]$reference.R - $compared.R) -le 8 -and [Math]::Abs([int]$reference.G - $compared.G) -le 8 -and [Math]::Abs([int]$reference.B - $compared.B) -le 8) { $matches++ }
                    }
                    $signature += $reference.ToArgb()
                }
            }
            $expected = 9 * ($bounds.Count - 1)
            if ($matches -ne $expected) { throw "Comparison offsets differ in '$Name' ($matches/$expected matching pixel samples)." }
            $script:comparisonSignature = $signature -join ','
        } finally { $bitmap.Dispose() }
    }
    function Send-ComparisonWheel([string]$Pane, [bool]$Horizontal) {
        Move-ComparisonPointer $Pane | Out-Null
        $flags = if ($Horizontal) { 0x1000 } else { 0x0800 }
        $amount = if ($Horizontal) { 120 } else { -120 }
        $data = [BitConverter]::ToUInt32([BitConverter]::GetBytes([int]$amount), 0)
        [RegressifySmokeNative]::mouse_event($flags, 0, 0, $data, [UIntPtr]::Zero)
        Wait-Control "$Pane image preview" | Out-Null
    }
    function Test-ComparisonGestures([string]$Name, [string]$Pane) {
        foreach ($amount in @(120, -120)) {
            $before = (Wait-Control "$Pane image canvas").Current.BoundingRectangle
            $visible = Get-ComparisonBounds $Pane
            $pointer = Move-ComparisonPointer $Pane
            $horizontal = $visible.Left + $pointer[0]
            $vertical = $visible.Top + $pointer[1]
            $data = [BitConverter]::ToUInt32([BitConverter]::GetBytes([int]$amount), 0)
            [RegressifySmokeZoomKeys]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
            try {
                [RegressifySmokeNative]::mouse_event(0x0800, 0, 0, $data, [UIntPtr]::Zero)
                $after = (Wait-Control "$Pane image canvas").Current.BoundingRectangle
            } finally { [RegressifySmokeZoomKeys]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero) }
            if (($amount -gt 0 -and $after.Width -le $before.Width) -or ($amount -lt 0 -and $after.Width -ge $before.Width)) {
                throw "Ctrl+wheel did not zoom $Pane in '$Name'."
            }
            $anchoredX = $after.Left + ($horizontal - $before.Left) * ($after.Width / $before.Width)
            $anchoredY = $after.Top + ($vertical - $before.Top) * ($after.Height / $before.Height)
            if ([Math]::Abs($anchoredX - $horizontal) -gt 2 -or [Math]::Abs($anchoredY - $vertical) -gt 2) {
                throw "Ctrl+wheel moved the pointer's image coordinate in '$Name'."
            }
        }
        $before = (Wait-Control "$Pane image canvas").Current.BoundingRectangle
        $visible = Get-ComparisonBounds $Pane
        $pointer = Move-ComparisonPointer $Pane
        [RegressifySmokeNative]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
        try {
            [RegressifySmokePointer]::SetPhysicalCursorPos([int]($visible.Left + $pointer[0] - 42), [int]($visible.Top + $pointer[1] - 54)) | Out-Null
            $after = (Wait-Control "$Pane image canvas").Current.BoundingRectangle
            if ([Math]::Abs($after.Left - $before.Left + 42) -gt 2 -or [Math]::Abs($after.Top - $before.Top + 54) -gt 2) {
                throw "Left-drag did not pan the image in '$Name'."
            }
        } finally { [RegressifySmokeNative]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero) }
        Assert-ComparisonPixels "$Name-zoom-pan" $Pane
    }
    function Test-FittedComparisonPan([switch]$VerticalPan) {
        $prefix = if ($VerticalPan) { 'short' } else { 'fitted' }
        foreach ($image in @(@('reference', 200, 1600), @('test', 300, 800), @('diff', 250, 1200))) {
            $width = if ($VerticalPan) { $image[2] } else { $image[1] }
            $height = if ($VerticalPan) { $image[1] } else { $image[2] }
            Write-ComparisonImage (Join-Path $output ("bitmaps_test/smoke-run/$prefix-" + $image[0] + '.png')) $width $height
        }
        Write-JsonFixture $reportPath @{
            id = 'smoke'
            tests = @(@{ status = 'fail'; pair = @{
                label = 'Fitted comparison'; fileName = 'fitted.png'
                reference = "../bitmaps_test/smoke-run/$prefix-reference.png"
                test = "../bitmaps_test/smoke-run/$prefix-test.png"
                diffImage = "../bitmaps_test/smoke-run/$prefix-diff.png"
            } })
        }
        Invoke-Control 'smoke.tests.json'
        Invoke-Control 'Results'
        Invoke-Control 'Refresh reports'
        Wait-Control 'Mismatch Fitted comparison' | Out-Null
        Wait-Control $(if ($VerticalPan) { 'Test 800 x 300' } else { 'Test 300 x 800' }) | Out-Null
        Invoke-Control 'Side by side'
        foreach ($dimensions in @(@(1000, 640), @(1600, 1000))) {
            [RegressifySmokeNative]::SetWindowPos($script:window, [IntPtr]::Zero, $screen.Left + 20, $screen.Top + 20, $dimensions[0], $dimensions[1], 0x0040) | Out-Null
            foreach ($theme in @('Light', 'Dark')) {
                Select-Theme $theme
                Invoke-Control 'Actual size (100%)'
                Invoke-Control 'Fit area'
                foreach ($pane in @('Reference', 'Test', 'Diff')) {
                    foreach ($direction in @(-1, 1)) {
                        $preview = (Wait-Control "$pane image preview").Current.BoundingRectangle
                        $before = (Wait-Control "$pane image canvas").Current.BoundingRectangle
                        if (($VerticalPan -and $before.Height -ge $preview.Height) -or
                            (-not $VerticalPan -and $before.Width -ge $preview.Width)) { throw 'The fitted comparison fixture must leave drag space on the tested axis.' }
                        $horizontal = [int]($before.Left + $before.Width / 2)
                        $vertical = [int]($before.Top + $before.Height / 2)
                        $delta = if ($VerticalPan) {
                            if ($direction -lt 0) { $preview.Top - $before.Top - 12 } else { $preview.Bottom - $before.Bottom + 12 }
                        } elseif ($direction -lt 0) { $preview.Left - $before.Left - 12 } else { $preview.Right - $before.Right + 12 }
                        Focus-Fixture
                        if (-not [RegressifySmokePointer]::SetPhysicalCursorPos($horizontal, $vertical)) { throw 'Cannot position the fitted comparison pointer.' }
                        [RegressifySmokeNative]::DwmFlush() | Out-Null
                        [RegressifySmokeNative]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
                        try {
                            [RegressifySmokeNative]::DwmFlush() | Out-Null
                            $targetX = if ($VerticalPan) { $horizontal } else { [int]($horizontal + $delta) }
                            $targetY = if ($VerticalPan) { [int]($vertical + $delta) } else { $vertical }
                            if (-not [RegressifySmokePointer]::SetPhysicalCursorPos($targetX, $targetY)) { throw 'Cannot drag the fitted comparison pointer.' }
                            foreach ($target in @('Reference', 'Test', 'Diff')) {
                                $canvas = (Wait-Control "$target image canvas").Current.BoundingRectangle
                                $viewport = (Wait-Control "$target image preview").Current.BoundingRectangle
                                $distance = if ($VerticalPan) {
                                    if ($direction -lt 0) { $canvas.Top - $viewport.Top } else { $viewport.Bottom - $canvas.Bottom }
                                } elseif ($direction -lt 0) { $canvas.Left - $viewport.Left } else { $viewport.Right - $canvas.Right }
                                if ([Math]::Abs($distance) -gt 1) { throw "Fitted $target did not stop at its lane boundary when dragging $pane." }
                                if ([Math]::Abs($canvas.Width - $before.Width) -gt 1 -or [Math]::Abs($canvas.Height - $before.Height) -gt 1) {
                                    throw 'Dragging a fitted comparison changed its zoom.'
                                }
                            }
                        } finally {
                            [RegressifySmokeNative]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
                            [RegressifySmokeNative]::DwmFlush() | Out-Null
                        }
                    }
                }
                Move-ComparisonPointer 'Reference' 0.9 0.9 | Out-Null
                Capture-Window ("comparison-$prefix-pan-" + $theme.ToLowerInvariant() + '-' + $dimensions[0])
                Invoke-Control 'Fit area'
                foreach ($pane in @('Reference', 'Test', 'Diff')) {
                    $canvas = (Wait-Control "$pane image canvas").Current.BoundingRectangle
                    $viewport = (Wait-Control "$pane image preview").Current.BoundingRectangle
                    if ([Math]::Abs(($canvas.Left + $canvas.Width / 2) - ($viewport.Left + $viewport.Width / 2)) -gt 1 -or
                        [Math]::Abs($canvas.Top - $viewport.Top) -gt 1) {
                        throw 'Fit area did not restore centered horizontal and top-aligned vertical placement.'
                    }
                }
            }
        }
    }
    function Test-BlendPixels {
        $referenceColor = [System.Drawing.Color]::FromArgb(200, 40, 60)
        $testColor = [System.Drawing.Color]::FromArgb(40, 160, 200)
        foreach ($image in @(@('blend-reference.png', $referenceColor), @('blend-test.png', $testColor))) {
            $bitmap = [System.Drawing.Bitmap]::new(960, 720)
            try {
                $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
                try { $graphics.Clear($image[1]) } finally { $graphics.Dispose() }
                if ($image[0] -eq 'blend-test.png') {
                    # One contrasting source pixel wide, visible near the left edge at 25x.
                    for ($row = 0; $row -lt 8; $row++) { $bitmap.SetPixel(1, $row, $referenceColor) }
                }
                $bitmap.Save((Join-Path $output ('bitmaps_test/smoke-run/' + $image[0])), [System.Drawing.Imaging.ImageFormat]::Png)
            } finally { $bitmap.Dispose() }
        }
        Write-JsonFixture $reportPath @{
            id = 'smoke'
            tests = @(@{ status = 'fail'; pair = @{
                label = 'Blend parity'; fileName = 'blend.png'
                reference = '../bitmaps_test/smoke-run/blend-reference.png'
                test = '../bitmaps_test/smoke-run/blend-test.png'
            } })
        }
        Invoke-Control 'smoke.tests.json'
        Invoke-Control 'Results'
        Invoke-Control 'Refresh reports'
        Wait-Control 'Mismatch Blend parity' | Out-Null
        Wait-Control 'Test 960 x 720' | Out-Null
        foreach ($blendDimensions in @(@(1000, 640), @(1600, 1000))) {
            [RegressifySmokeNative]::SetWindowPos($script:window, [IntPtr]::Zero, $screen.Left + 20, $screen.Top + 20, $blendDimensions[0], $blendDimensions[1], 0x0040) | Out-Null
            foreach ($theme in @('Light', 'Dark')) {
                Select-Theme $theme
                foreach ($mode in @('Wipe', 'Overlay')) {
                    Invoke-Control $mode
                    Invoke-Control 'Fit area'
                    $label = if ($mode -eq 'Wipe') { 'Wipe position' } else { 'Test image opacity' }
                    Invoke-Control ('Adjust ' + $label) -Pointer
                    foreach ($endpoint in @(0, 100)) {
                        if ($endpoint -eq 0) { Send-FixtureKeys Home } else { Send-FixtureKeys End }
                        Wait-Control "$endpoint%" $label | Out-Null
                        $canvas = (Wait-Control 'Reference image canvas').Current.BoundingRectangle
                        $preview = (Wait-Control 'Reference image preview').Current.BoundingRectangle
                        if ($canvas.Right -gt $preview.Right + 1 -or $canvas.Bottom -gt $preview.Bottom + 1) {
                            throw "$mode does not fit the preview at $($blendDimensions[0])px."
                        }
                        $name = 'comparison-blend-' + $theme.ToLowerInvariant() + '-' + $blendDimensions[0] + '-' + $mode.ToLowerInvariant() + '-' + $endpoint
                        Capture-Window $name
                        $native = [RegressifySmokeNative+Rect]::new()
                        if (-not [RegressifySmokeNative]::GetWindowRect($script:window, [ref]$native)) { throw 'Cannot read blend window bounds.' }
                        $bitmap = [System.Drawing.Bitmap]::FromFile((Join-Path $artifacts "$name.png"))
                        try {
                            $expected = if (($mode -eq 'Wipe' -and $endpoint -eq 0) -or ($mode -eq 'Overlay' -and $endpoint -eq 100)) { $testColor } else { $referenceColor }
                            foreach ($fraction in @(0.25, 0.75)) {
                                $actual = $bitmap.GetPixel([int]($canvas.Left - $native.Left + $canvas.Width * $fraction), [int]($canvas.Top - $native.Top + $canvas.Height * 0.5))
                                if ([Math]::Abs($actual.R - $expected.R) -gt 3 -or [Math]::Abs($actual.G - $expected.G) -gt 3 -or [Math]::Abs($actual.B - $expected.B) -gt 3) {
                                    throw "$mode $endpoint% rendered the wrong source pixels in '$name'."
                                }
                            }
                        } finally { $bitmap.Dispose() }
                    }
                    Send-FixtureKeys LeftArrow
                    Wait-Control '99%' $label | Out-Null
                }
            }
        }
        Invoke-Control 'Test'
        Invoke-Control 'Actual size (100%)'
        foreach ($zoom in @(110, 125, 150, 175, 200, 250, 300, 400, 500, 600, 800, 1000, 1250, 1500, 2000, 2500)) {
            Invoke-Control 'Zoom in'
            Wait-Control "$zoom.0%" | Out-Null
        }
        Invoke-Control 'Zoom in' -Pointer
        Wait-Control '2500.0%' | Out-Null
        $canvas = (Wait-Control 'Test image canvas').Current.BoundingRectangle
        $native = [RegressifySmokeNative+Rect]::new()
        if (-not [RegressifySmokeNative]::GetWindowRect($script:window, [ref]$native)) { throw 'Cannot read magnified window bounds.' }
        $pixelWidth = $canvas.Width / 960
        Capture-Window 'comparison-pixelated-2500'
        $bitmap = [System.Drawing.Bitmap]::FromFile((Join-Path $artifacts 'comparison-pixelated-2500.png'))
        try {
            $y = [int]($canvas.Top - $native.Top + $pixelWidth / 2)
            $left = $canvas.Left - $native.Left
            $verticalLine = $false
            for ($x = [int]($left + $pixelWidth / 2); $x -lt $left + 2.5 * $pixelWidth; $x++) {
                # Allow one device pixel for the conversion from layout to UIA coordinates.
                $actual = $bitmap.GetPixel($x, $y)
                if ([Math]::Abs($x - $left - $pixelWidth) -le 1 -or [Math]::Abs($x - $left - 2 * $pixelWidth) -le 1) {
                    if ($actual.G -gt $referenceColor.G + 10 -and $actual.G -lt $testColor.G - 10) { $verticalLine = $true }
                    continue
                }
                $expected = if ($x -gt $left + $pixelWidth -and $x -lt $left + 2 * $pixelWidth) { $referenceColor } else { $testColor }
                if ([Math]::Abs($actual.R - $expected.R) -gt 3 -or [Math]::Abs($actual.G - $expected.G) -gt 3 -or [Math]::Abs($actual.B - $expected.B) -gt 3) {
                    throw 'The 2500% comparison blended or misplaced a source pixel.'
                }
            }
            if (-not $verticalLine) { throw 'The pixel grid did not render on a magnified source-pixel boundary.' }
        } finally { $bitmap.Dispose() }
        Invoke-Control 'Zoom out'
        Wait-Control '2000.0%' | Out-Null
    }
    Write-ComparisonImage (Join-Path $output 'bitmaps_reference/checkout.png') 1600 2200
    Write-ComparisonImage (Join-Path $output 'bitmaps_test/smoke-run/checkout.png') 1280 1800
    Write-ComparisonImage (Join-Path $output 'bitmaps_test/smoke-run/checkout_diff.png') 1600 2400
    $reportPath = Join-Path $output 'bitmaps_test/smoke-run/report.json'
    $report = Get-Content -LiteralPath $reportPath -Raw | ConvertFrom-Json
    $report.tests[0].pair.diff.dimensionDifference.width = -320
    $report.tests[0].pair.diff.dimensionDifference.height = -400
    Write-JsonFixture $reportPath $report
    Invoke-Control 'smoke.tests.json'
    Invoke-Control 'Results'
    Wait-Control 'Test 1280 x 1800' | Out-Null
    Invoke-Control 'Side by side'
    Wait-Control 'Reference 1600 x 2200' | Out-Null
    Wait-Control 'Diff 1600 x 2400' | Out-Null
    foreach ($dimensions in @(@(1000, 640), @(1600, 1000), @(1080, 720))) {
        [RegressifySmokeNative]::SetWindowPos($script:window, [IntPtr]::Zero, $screen.Left + 20, $screen.Top + 20, $dimensions[0], $dimensions[1], 0x0040) | Out-Null
        foreach ($theme in @('Light', 'Dark')) {
            Select-Theme $theme
            Invoke-Control 'Error Account desktop'
            Wait-Control 'Missing selector #account' | Out-Null
            Test-CaptureInformation ('information-same-url-' + $theme.ToLowerInvariant() + '-' + $dimensions[0]) -SameUrl
            Invoke-Control 'Mismatch Checkout'
            Wait-Control 'Reference 1600 x 2200' | Out-Null
            Wait-Control 'Test 1280 x 1800' | Out-Null
            Test-CaptureInformation ('information-distinct-urls-' + $theme.ToLowerInvariant() + '-' + $dimensions[0]) -DiffX -320 -DiffY -400
            Test-ImageContextMenus
            foreach ($removed in @('Next capture', 'Previous capture', 'Next captures', 'Previous captures')) {
                if ($null -ne (Find-Control $removed)) { throw "Removed pagination control '$removed' is still present." }
            }
            $tab = (Wait-Control 'Side by side').Current.BoundingRectangle
            $fit = (Wait-Control 'Fit area').Current.BoundingRectangle
            if ($fit.Left -lt $tab.Right -or [Math]::Abs(($fit.Top + $fit.Height / 2) - ($tab.Top + $tab.Height / 2)) -gt 3) {
                throw 'Image controls are not aligned to the right of the tabs.'
            }
            Invoke-Control 'Actual size (100%)'
            $actual = (Wait-Control 'Reference image canvas').Current.BoundingRectangle
            $name = 'comparison-' + $theme.ToLowerInvariant() + '-' + $dimensions[0]
            Assert-ComparisonPixels "$name-initial"
            Assert-ComparisonPixels "$name-moved-guides" -HorizontalFraction 0.61 -VerticalFraction 0.54
            foreach ($pane in @('Reference', 'Test', 'Diff')) {
                foreach ($horizontal in @($false, $true)) {
                    $before = $script:comparisonSignature
                    Send-ComparisonWheel $pane $horizontal
                    Assert-ComparisonPixels ("$name-" + $pane.ToLowerInvariant() + $(if ($horizontal) { '-horizontal' } else { '-vertical' })) $pane
                    if ($before -eq $script:comparisonSignature) { throw "The $pane wheel did not move comparison pixels (horizontal: $horizontal)." }
                }
            }
            foreach ($pane in @('Reference', 'Test', 'Diff')) {
                Test-ComparisonGestures ("$name-" + $pane.ToLowerInvariant()) $pane
            }
            Invoke-Control 'Fit area'
            foreach ($pane in @('Reference', 'Test', 'Diff')) {
                $preview = (Wait-Control "$pane image preview").Current.BoundingRectangle
                $canvas = (Wait-Control "$pane image canvas").Current.BoundingRectangle
                $scale = [Math]::Min(1.0, [Math]::Min($preview.Width / $actual.Width, $preview.Height / $actual.Height))
                if ($canvas.Left -lt $preview.Left - 1 -or $canvas.Top -lt $preview.Top - 1 -or $canvas.Right -gt $preview.Right + 1 -or $canvas.Bottom -gt $preview.Bottom + 1) {
                    throw "Fit area clipped the $pane image in '$name'."
                }
                if ([Math]::Abs($canvas.Width - $actual.Width * $scale) -gt 2 -or [Math]::Abs($canvas.Height - $actual.Height * $scale) -gt 2) {
                    throw "Fit area used an incorrect scale for $pane in '$name': canvas=$canvas, actual=$actual, viewport=$preview, scale=$scale."
                }
                if ([Math]::Abs(($canvas.Left + $canvas.Width / 2) - ($preview.Left + $preview.Width / 2)) -gt 1) {
                    throw "Fit area did not center the $pane image in '$name'."
                }
            }
            Assert-ComparisonPixels "$name-fit" 'Test'
            Assert-ComparisonPixels "$name-no-guides" -HideGuides
            Invoke-Control 'Fit width'
            foreach ($pane in @('Reference', 'Test', 'Diff')) {
                $preview = (Wait-Control "$pane image preview").Current.BoundingRectangle
                $canvas = (Wait-Control "$pane image canvas").Current.BoundingRectangle
                $scale = [Math]::Min(1.0, $preview.Width / $actual.Width)
                if ([Math]::Abs($canvas.Width - $actual.Width * $scale) -gt 2 -or [Math]::Abs($canvas.Height - $actual.Height * $scale) -gt 2) {
                    throw "Fit width used an incorrect scale for $pane in '$name'."
                }
            }
            Assert-ComparisonPixels "$name-fit-width" 'Diff'
            Invoke-Control 'Fit area'
            foreach ($pane in @('Reference', 'Test', 'Diff')) {
                Move-ComparisonPointer $pane | Out-Null
                Send-FixturePointerClick
                Wait-Control "$pane image preview" | Out-Null
                foreach ($other in @('Reference', 'Test', 'Diff') | Where-Object { $_ -ne $pane }) {
                    if ($null -ne (Find-Control "$other image preview")) { throw "Clicking $pane did not open its tab." }
                }
                Invoke-Control 'Fit width'
                Capture-Window ("$name-" + $pane.ToLowerInvariant() + '-tab')
                Invoke-Control 'Side by side'
                Wait-Control 'Diff 1600 x 2400' | Out-Null
            }
        }
    }
    Invoke-Control 'account.tests.json'
    Wait-Control 'No captures for this suite' | Out-Null
    if ($null -ne (Find-Control 'Test image preview') -or $null -ne (Find-Control 'Mismatch Checkout')) { throw 'An empty suite displayed another suite''s captures.' }
    Capture-Window 'comparison-empty-suite'
    [RegressifySmokeNative]::SetWindowPos($script:window, [IntPtr]::Zero, $screen.Left + 20, $screen.Top + 20, 1000, 640, 0x0040) | Out-Null
    foreach ($theme in @('Light', 'Dark')) {
        Select-Theme $theme
        Wait-Control 'No captures for this suite' | Out-Null
        if ($null -ne (Find-Control 'Test image preview') -or $null -ne (Find-Control 'Mismatch Checkout')) { throw 'The minimum-size empty suite displayed another suite''s captures.' }
        Capture-Window ('comparison-empty-suite-' + $theme.ToLowerInvariant() + '-minimum')
    }
    [RegressifySmokeNative]::SetWindowPos($script:window, [IntPtr]::Zero, $screen.Left + 20, $screen.Top + 20, 1080, 720, 0x0040) | Out-Null
    Hover-Control 'account.tests.json'
    [RegressifySmokeNative]::mouse_event(0x0800, 0, 0, ([uint32]::MaxValue - 119), [UIntPtr]::Zero)
    [RegressifySmokeNative]::DwmFlush() | Out-Null
    Invoke-Control 'smoke.tests.json'
    Wait-Control 'Test 1280 x 1800' | Out-Null
    Test-ReportApproval
    Test-ErrorImageFlows
    Test-FittedComparisonPan
    Test-FittedComparisonPan -VerticalPan
    Test-BlendPixels
    Write-Output 'Six-mode Results, fit modes, bounded fitted-image dragging, image-tab navigation, pointer-centered zoom, drag panning, linked scrolling, crosshairs, and wipe/overlay pixels passed native checks in light/dark desktop and compact windows.'
}
