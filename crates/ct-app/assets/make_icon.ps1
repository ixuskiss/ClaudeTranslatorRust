# Generates the tray icon: a blue speech bubble with "A" overlapping a white one with
# "가". Writes tray-32.rgba (straight-alpha RGBA, row-major, 32x32), a 256 px preview and a
# zoomed 32 px preview (nearest neighbour, to judge legibility at real size).
# Usage: powershell -File make_icon.ps1 -OutDir <dir>
param([string]$OutDir = $PSScriptRoot)
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

function Point([float]$x, [float]$y) { New-Object System.Drawing.PointF($x, $y) }

# Draws $text centred in $rect, scaled so its ink fills $fill of the rect (largest glyph),
# with an extra $stroke-wide outline in the same colour so strokes survive at 32 px.
function Draw-Glyph($g, [string]$text, [string]$family, $rect, $color, [float]$fill, [float]$stroke) {
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddString($text, (New-Object System.Drawing.FontFamily($family)),
        [int][System.Drawing.FontStyle]::Bold, 100, (Point 0 0),
        [System.Drawing.StringFormat]::GenericTypographic)
    $b = $path.GetBounds()
    $scale = [Math]::Min($rect.Width * $fill / $b.Width, $rect.Height * $fill / $b.Height)
    $m = New-Object System.Drawing.Drawing2D.Matrix
    $m.Translate($rect.X + $rect.Width / 2, $rect.Y + $rect.Height / 2)
    $m.Scale($scale, $scale)
    $m.Translate(-($b.X + $b.Width / 2), -($b.Y + $b.Height / 2))
    $path.Transform($m)
    $g.FillPath((New-Object System.Drawing.SolidBrush($color)), $path)
    if ($stroke -gt 0) {
        $pen = New-Object System.Drawing.Pen($color, $stroke)
        $pen.LineJoin = 'Round'
        $g.DrawPath($pen, $path)
    }
}

function Draw-Icon([int]$S) {
    $bmp = New-Object System.Drawing.Bitmap($S, $S, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)
    $k = $S / 128.0
    $blue = [System.Drawing.Color]::FromArgb(255, 0x1F, 0x6F, 0xEB)
    $white = [System.Drawing.Color]::White

    # Back bubble (blue), top-left, tail at bottom-left; white "A" as large as it fits.
    $bx = 0; $by = 0; $bw = 80 * $k; $bh = 70 * $k
    $g.FillPath((New-Object System.Drawing.SolidBrush($blue)), (New-RoundRect $bx $by $bw $bh (14*$k)))
    $tail = New-Object System.Drawing.Drawing2D.GraphicsPath
    $tail.AddPolygon(@((Point (10*$k) (64*$k)), (Point (6*$k) (86*$k)), (Point (32*$k) (66*$k))))
    $g.FillPath((New-Object System.Drawing.SolidBrush($blue)), $tail)
    # The front bubble covers the bottom-right of the back one; keep "A" in the free area.
    $aRect = New-Object System.Drawing.RectangleF((2*$k), (2*$k), (52*$k), (56*$k))
    Draw-Glyph $g 'A' 'Segoe UI' $aRect $white 0.86 (4*$k)

    # Front bubble (white, blue outline), bottom-right, tail at bottom-right; blue "가".
    $fx = 42 * $k; $fy = 46 * $k; $fw = 86 * $k; $fh = 72 * $k; $ow = 6 * $k
    $front = New-RoundRect ($fx + $ow/2) ($fy + $ow/2) ($fw - $ow) ($fh - $ow) (14*$k)
    $tail2 = New-Object System.Drawing.Drawing2D.GraphicsPath
    $tail2.AddPolygon(@((Point (98*$k) (112*$k)), (Point (120*$k) (128*$k)), (Point (114*$k) (110*$k))))
    $g.FillPath((New-Object System.Drawing.SolidBrush($blue)), $tail2)
    $g.FillPath((New-Object System.Drawing.SolidBrush($white)), $front)
    $g.DrawPath((New-Object System.Drawing.Pen($blue, $ow)), $front)
    $kRect = New-Object System.Drawing.RectangleF(($fx + $ow), ($fy + $ow), ($fw - 2*$ow), ($fh - 2*$ow))
    Draw-Glyph $g ([string][char]0xAC00) 'Malgun Gothic' $kRect $blue 0.80 (6*$k)
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

# Zoomed 32 px preview (8x, nearest neighbour) to judge real-size legibility.
$zoom = New-Object System.Drawing.Bitmap(256, 256, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g3 = [System.Drawing.Graphics]::FromImage($zoom)
$g3.InterpolationMode = 'NearestNeighbor'
$g3.PixelOffsetMode = 'Half'
$g3.Clear([System.Drawing.Color]::FromArgb(255, 0x30, 0x30, 0x30))
$g3.DrawImage($small, 0, 0, 256, 256)
$g3.Dispose()
$zoom.Save((Join-Path $OutDir 'tray-preview-32x8.png'), [System.Drawing.Imaging.ImageFormat]::Png)

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
