# OntoDB 一键安装脚本 (Windows PowerShell)
# 用法: irm https://get.ontodb.io/install.ps1 | iex

param(
    [string]$Version = "v0.3.0",
    [string]$InstallDir = "$env:USERPROFILE\.ontodb",
    [string]$DataDir = "$env:USERPROFILE\.ontodb\data",
    [int]$Port = 7912
)

$ErrorActionPreference = "Stop"

function Write-Banner {
    Write-Host ""
    Write-Host "  ╔══════════════════════════════════════════════╗" -ForegroundColor Cyan
    Write-Host "  ║     OntoDB — 本体驱动的六模态语义数据库       ║" -ForegroundColor Cyan
    Write-Host "  ║     安装脚本 $Version                         ║" -ForegroundColor Cyan
    Write-Host "  ╚══════════════════════════════════════════════╝" -ForegroundColor Cyan
    Write-Host ""
}

function Write-Log {
    param([string]$Message, [string]$Type = "info")
    switch ($Type) {
        "success" { Write-Host "[✓] $Message" -ForegroundColor Green }
        "warning" { Write-Host "[!] $Message" -ForegroundColor Yellow }
        "error"   { Write-Host "[✗] $Message" -ForegroundColor Red }
        default   { Write-Host "[i] $Message" -ForegroundColor Blue }
    }
}

function Test-Platform {
    if (-not [System.Environment]::Is64BitOperatingSystem) {
        Write-Log "不支持 32 位系统" "error"
        exit 1
    }
    Write-Log "检测到平台: windows-x86_64"
}

function Install-OntoDB {
    $url = "https://release.ontodb.io/ontodb-$Version-windows-x86_64.zip"
    $tmpFile = "$env:TEMP\ontodb-$Version.zip"
    
    Write-Log "下载 OntoDB $Version..."
    
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -Uri $url -OutFile $tmpFile -UseBasicParsing
    } catch {
        Write-Log "下载失败: $_" "error"
        exit 1
    }
    
    Write-Log "下载完成"
    
    # 解压
    if (Test-Path $InstallDir) {
        Remove-Item $InstallDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Expand-Archive -Path $tmpFile -DestinationPath $InstallDir
    Remove-Item $tmpFile -Force
    
    Write-Log "安装到 $InstallDir"
}

function New-DataDir {
    if (-not (Test-Path $DataDir)) {
        New-Item -ItemType Directory -Path $DataDir -Force | Out-Null
    }
    Write-Log "数据目录: $DataDir"
}

function New-Config {
    $configFile = "$InstallDir\ontodb.toml"
    
    if (Test-Path $configFile) {
        Write-Log "配置文件已存在，跳过生成" "warning"
        return
    }
    
    $config = @"
# OntoDB 配置文件

[data]
dir = "$($DataDir -replace '\\', '/')"

[server]
http = "127.0.0.1:$Port"

[auth]
enabled = false

[performance]
memtable_size_mb = 64
block_cache_mb = 256
sync_wal_on_commit = false

[security]
rate_limit_rpm = 60
max_query_size_mb = 10
"@
    
    Set-Content -Path $configFile -Value $config -Encoding UTF8
    Write-Log "生成配置文件: $configFile"
}

function Add-ToPath {
    $binDir = "$InstallDir\bin"
    $currentPath = [Environment]::GetEnvironmentVariable("Path", "User")
    
    if ($currentPath -notlike "*$binDir*") {
        [Environment]::SetEnvironmentVariable("Path", "$currentPath;$binDir", "User")
        $env:Path = "$env:Path;$binDir"
        Write-Log "已添加到 PATH (重启终端生效)"
    }
}

function Install-Service {
    # 创建启动脚本
    $startScript = "$InstallDir\start-ontodb.bat"
    $batContent = @"
@echo off
echo Starting OntoDB...
"$InstallDir\bin\ontodb-server.exe" --data-dir "$DataDir" --http 0.0.0.0:$Port --no-rate-limit
pause
"@
    Set-Content -Path $startScript -Value $batContent -Encoding ASCII
    Write-Log "创建启动脚本: $startScript"
    
    # 创建 Windows 服务 (可选)
    $serviceName = "OntoDB"
    $existingService = Get-Service -Name $serviceName -ErrorAction SilentlyContinue
    
    if ($existingService) {
        Write-Log "Windows 服务已存在" "warning"
        return
    }
    
    try {
        $exePath = "$InstallDir\bin\ontodb-server.exe"
        if (Test-Path $exePath) {
            New-Service -Name $serviceName -BinaryPathName "`"$exePath`" --data-dir `"$DataDir`" --http 0.0.0.0:$Port" -DisplayName "OntoDB Semantic Database" -Description "OntoDB 本体驱动的六模态语义数据库" -StartupType Manual
            Write-Log "Windows 服务已创建 (手动启动)"
        }
    } catch {
        Write-Log "创建 Windows 服务失败 (需要管理员权限)" "warning"
    }
}

function Show-Usage {
    Write-Host ""
    Write-Host "  ╔══════════════════════════════════════════════╗" -ForegroundColor Green
    Write-Host "  ║              安装完成！                       ║" -ForegroundColor Green
    Write-Host "  ╚══════════════════════════════════════════════╝" -ForegroundColor Green
    Write-Host ""
    Write-Host "  启动服务器:" -ForegroundColor White
    Write-Host "    双击 $InstallDir\start-ontodb.bat" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "  或命令行启动:" -ForegroundColor White
    Write-Host "    $InstallDir\bin\ontodb-server.exe --data-dir $DataDir --http 127.0.0.1:$Port" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "  访问地址:" -ForegroundColor White
    Write-Host "    Web 控制台:  http://127.0.0.1:$Port/console" -ForegroundColor Cyan
    Write-Host "    API 文档:    http://127.0.0.1:$Port/api/docs" -ForegroundColor Cyan
    Write-Host "    健康检查:    http://127.0.0.1:$Port/api/health" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "  CLI 工具:" -ForegroundColor White
    Write-Host "    $InstallDir\bin\ontodb-cli.exe 127.0.0.1:$Port" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "  配置文件: $InstallDir\ontodb.toml" -ForegroundColor White
    Write-Host "  数据目录: $DataDir" -ForegroundColor White
    Write-Host ""
}

# 主流程
function Main {
    Write-Banner
    Test-Platform
    Install-OntoDB
    New-DataDir
    New-Config
    Add-ToPath
    Install-Service
    Show-Usage
}

Main
