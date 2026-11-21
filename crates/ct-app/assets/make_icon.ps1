param([string]$OutDir)
Add-Type -AssemblyName System.Drawing

function New-RoundRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function Draw-Icon([int]$S) {
    $bmp = New-Object System.Drawing.Bitmap($S, $S, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.TextRenderingHint = 'AntiAliasGridFit'
    $g.Clear([System.Drawing.Color]::Transparent)
    $k = $S / 128.0
    $blue = [System.Drawing.Color]::FromArgb(255, 0x1F, 0x6F, 0xEB)
    $white = [System.Drawing.Color]::White

    # Back bubble (blue) with a tail at bottom-left, white "A".
    $back = New-RoundRect (4*$k) (6*$k) (78*$k) (64*$k) (16*$k)
    $tail = New-Object System.Drawing.Drawing2D.GraphicsPath
    $tail.AddPolygon(@(
        (New-Object System.Drawing.PointF((18*$k), (66*$k))),
        (New-Object System.Drawing.PointF((14*$k), (86*$k))),
        (New-Object System.Drawing.PointF((38*$k), (68*$k)))))
    $g.FillPath((New-Object System.Drawing.SolidBrush($blue)), $back)
    $g.FillPath((New-Object System.Drawing.SolidBrush($blue)), $tail)
    $fontA = New-Object System.Drawing.Font('Segoe UI', (44*$k), [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
    $fmt = New-Object System.Drawing.StringFormat
    $fmt.Alignment = 'Center'; $fmt.LineAlignment = 'Center'
    $g.DrawString('A', $fontA, (New-Object System.Drawing.SolidBrush($white)), (New-Object System.Drawing.RectangleF((4*$k), (4*$k), (66*$k), (64*$k))), $fmt)

    # Front bubble (white, blue outline) with a tail at bottom-right, blue "가".
    $front = New-RoundRect (46*$k) (48*$k) (76*$k) (62*$k) (16*$k)
    $tail2 = New-Object System.Drawing.Drawing2D.GraphicsPath
    $tail2.AddPolygon(@(
        (New-Object System.Drawing.PointF((96*$k), (106*$k))),
        (New-Object System.Drawing.PointF((112*$k), (124*$k))),
        (New-Object System.Drawing.PointF((108*$k), (104*$k)))))
    $g.FillPath((New-Object System.Drawing.SolidBrush($white)), $front)
    $g.FillPath((New-Object System.Drawing.SolidBrush($white)), $tail2)
    $pen = New-Object System.Drawing.Pen($blue, (7*$k))
    $g.DrawPath($pen, $front)
    $fontK = New-Object System.Drawing.Font('Malgun Gothic', (38*$k), [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
    $g.DrawString([string][char]0xAC00, $fontK, (New-Object System.Drawing.SolidBrush($blue)), (New-Object System.Drawing.RectangleF((46*$k), (48*$k), (76*$k), (62*$k))), $fmt)
    $g.Dispose()
    return $bmp
}

$big = Draw-Icon 256
$big.Save((Join-Path $OutDir 'tray-preview-256.png'), [System.Drawing.Imaging.ImageFormat]::Png)

# 32x32 from a high-res render for clean anti-aliasing.
$src = Draw-Icon 128
$small = New-Object System.Drawing.Bitmap(32, 32, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g2 = [System.Drawing.Graphics]::FromImage($small)
$g2.InterpolationMode = 'HighQualityBicubic'
$g2.PixelOffsetMode = 'HighQuality'
$g2.Clear([System.Drawing.Color]::Transparent)
$g2.DrawImage($src, 0, 0, 32, 32)
$g2.Dispose()
$small.Save((Join-Path $OutDir 'tray-preview-32.png'), [System.Drawing.Imaging.ImageFormat]::Png)

# Raw straight-alpha RGBA, row-major, 32*32*4 bytes.
$bytes = New-Object byte[] (32*32*4)
for ($y = 0; $y -lt 32; $y++) {
    for ($x = 0; $x -lt 32; $x++) {
        $c = $small.GetPixel($x, $y)
        $i = ($y*32 + $x) * 4
        $bytes[$i] = $c.R; $bytes[$i+1] = $c.G; $bytes[$i+2] = $c.B; $bytes[$i+3] = $c.A
    }
}
[System.IO.File]::WriteAllBytes((Join-Path $OutDir 'tray-32.rgba'), $bytes)
"ok"
