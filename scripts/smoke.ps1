# Installs the real installer, compresses a picture with the installed app, opens its window,
# then uninstalls. Any failure stops the build.
param([Parameter(Mandatory)][string]$Installer)

$ErrorActionPreference = 'Stop'
$name = [IO.Path]::GetFileNameWithoutExtension($Installer)
$shots = Join-Path $PWD 'smoke'
New-Item -ItemType Directory -Force $shots | Out-Null
Add-Type -AssemblyName System.Drawing, System.Windows.Forms

function Assert($condition, $message) { if (-not $condition) { throw "FAILED: $message" } }

Write-Host "== $name"
$p = Start-Process $Installer -ArgumentList '/S' -Wait -PassThru
Assert ($p.ExitCode -eq 0) "installer exit code $($p.ExitCode)"
$dir = Join-Path $env:LOCALAPPDATA 'NenAnh'
$exe = Join-Path $dir 'NenAnh.exe'
Assert (Test-Path $exe) "$exe is missing"

# Fixture in a Vietnamese path.
$work = Join-Path $env:RUNNER_TEMP "Ảnh cưới Đà Lạt $name"
New-Item -ItemType Directory -Force $work | Out-Null
$src = Join-Path $work 'ảnh đẹp.jpg'
$bmp = [Drawing.Bitmap]::new(400, 300)
$rnd = [Random]::new(1)
for ($y = 0; $y -lt 300; $y++) {
  for ($x = 0; $x -lt 400; $x++) {
    $n = $rnd.Next(8)
    $r = [int](120 + 90 * [Math]::Sin($x / 25)) + $n
    $g = [int](120 + 90 * [Math]::Cos($y / 30)) + $n
    $bmp.SetPixel($x, $y, [Drawing.Color]::FromArgb($r, $g, 90 + $n))
  }
}
$codec = [Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object MimeType -eq 'image/jpeg'
$params = [Drawing.Imaging.EncoderParameters]::new(1)
$params.Param[0] = [Drawing.Imaging.EncoderParameter]::new([Drawing.Imaging.Encoder]::Quality, [long]98)
$bmp.Save($src, $codec, $params)
$bmp.Dispose()
$hash = (Get-FileHash $src).Hash

$out = Join-Path $work 'kết quả'
$p = Start-Process $exe -ArgumentList '--smoke', "`"$src`"", "`"$out`"" -Wait -PassThru
Assert ($p.ExitCode -eq 0) "compression exit code $($p.ExitCode)"
Assert ((Get-FileHash $src).Hash -eq $hash) 'original was modified'
foreach ($ext in 'jpg', 'webp', 'avif') {
  $file = Get-Item (Join-Path $out "ảnh đẹp.$ext")
  Assert ($file.Length -lt (Get-Item $src).Length) "$ext is not smaller"
  Write-Host "$ext $((Get-Item $src).Length) -> $($file.Length)"
}
$img = [Drawing.Image]::FromFile((Join-Path $out 'ảnh đẹp.jpg'))
Assert ($img.Width -eq 400 -and $img.Height -eq 300) 'dimensions changed'
$img.Dispose()

$p = Start-Process $exe -PassThru
Start-Sleep 15
$p.Refresh()
Assert (-not $p.HasExited) 'window closed by itself'
Assert ($p.MainWindowTitle -eq 'Nén Ảnh') "window title is '$($p.MainWindowTitle)'"
$b = [Windows.Forms.Screen]::PrimaryScreen.Bounds
$shot = [Drawing.Bitmap]::new($b.Width, $b.Height)
[Drawing.Graphics]::FromImage($shot).CopyFromScreen($b.Location, [Drawing.Point]::Empty, $b.Size)
$shot.Save((Join-Path $shots "$name.png"))
Stop-Process -Id $p.Id -Force
Start-Sleep 3

Start-Process (Join-Path $dir 'uninstall.exe') -ArgumentList '/S' -Wait
for ($i = 0; $i -lt 20 -and (Test-Path $exe); $i++) { Start-Sleep 1 }
Assert (-not (Test-Path $exe)) 'uninstall left the program behind'
Write-Host "OK $name"
