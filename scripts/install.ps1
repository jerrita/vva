# 安装或更新 vva 到最新 release。环境变量 VVA_INSTALL_DIR 指定安装目录 (默认 %LOCALAPPDATA%\vva\bin)。
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = 'jerrita/vva'
$dir = if ($env:VVA_INSTALL_DIR) { $env:VVA_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'vva\bin' }
$exe = Join-Path $dir 'vva.exe'
$old = Join-Path $dir 'vva.exe.old'

# Windows PowerShell 5.1 无 -MaximumRetryCount，网络请求统一经此重试
function Invoke-Retry([scriptblock]$action) {
    for ($i = 1; ; $i++) {
        try { return & $action }
        catch { if ($i -ge 3) { throw } ; Start-Sleep -Seconds $i }
    }
}

# 上次更新时被占用而改名保留的旧二进制
Remove-Item $old -Force -ErrorAction SilentlyContinue

# 经 releases/latest 重定向解析最新 tag，不占用 GitHub API 限额
$latest = Invoke-Retry {
    $response = [Net.WebRequest]::Create("https://github.com/$repo/releases/latest").GetResponse()
    try { $response.ResponseUri.AbsoluteUri } finally { $response.Close() }
}
$tag = ($latest -split '/')[-1]
if ($tag -notlike 'v*') { throw '无法获取最新版本' }
$expected = "vva $($tag.TrimStart('v'))"

$current = if (Test-Path $exe) { try { & $exe --version 2>$null } catch { $null } }
if ($current -eq $expected) {
    Write-Host "已是最新版本 $tag ($exe)"
    return
}

$name = "vva-$tag-windows-x64"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "下载 $tag ..."
    $zip = Join-Path $tmp "$name.zip"
    Invoke-Retry { Invoke-WebRequest "https://github.com/$repo/releases/download/$tag/$name.zip" -OutFile $zip -UseBasicParsing }
    Expand-Archive $zip -DestinationPath $tmp
    $new = Join-Path $tmp "$name\vva.exe"
    $installed = & $new --version
    if ($installed -ne $expected) { throw "版本不符: 期望 $tag，实际 $installed" }

    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    # 运行中的 exe 不可覆盖但可改名，先移开旧文件再放入新文件
    if (Test-Path $exe) { Move-Item $exe $old -Force }
    Copy-Item $new $exe -Force
}
finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
Remove-Item $old -Force -ErrorAction SilentlyContinue
if ($current) {
    Write-Host "已从 $($current -replace '^vva ', '') 更新到 ${tag}: $exe"
} else {
    Write-Host "已安装 vva $tag 到 $exe"
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$dir", 'User')
    Write-Host "已将 $dir 加入用户 PATH，重新打开终端后生效"
}
