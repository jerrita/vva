# 安装或更新 vva 到最新 release。环境变量 VVA_INSTALL_DIR 指定安装目录 (默认 %LOCALAPPDATA%\vva\bin)。
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = 'jerrita/vva'
$dir = if ($env:VVA_INSTALL_DIR) { $env:VVA_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'vva\bin' }

$tag = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
$name = "vva-$tag-windows-x64"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "下载 $tag ..."
    $zip = Join-Path $tmp "$name.zip"
    Invoke-WebRequest "https://github.com/$repo/releases/download/$tag/$name.zip" -OutFile $zip
    Expand-Archive $zip -DestinationPath $tmp
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    Copy-Item (Join-Path $tmp "$name\vva.exe") (Join-Path $dir 'vva.exe') -Force
}
finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host "已安装 vva $tag 到 $dir\vva.exe"

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$dir", 'User')
    Write-Host "已将 $dir 加入用户 PATH，重新打开终端后生效"
}
