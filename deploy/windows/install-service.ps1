# Pulse 探针的 Windows 服务安装脚本。
#
# 需要管理员权限（注册服务），但**服务本身以虚拟服务账号运行，不是 SYSTEM**。
# 这与 Linux 上「安装需 root、运行不需要」是同一个取舍。
#
# 用法（管理员 PowerShell）：
#   .\install-service.ps1 -Server wss://panel.example.com -Token <TOKEN>

param(
    [Parameter(Mandatory=$true)][string]$Server,
    [Parameter(Mandatory=$true)][string]$Token,
    [string]$InstallDir = "$env:ProgramData\Pulse",
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
$ServiceName = "pulse-agent"

function Assert-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $p  = New-Object Security.Principal.WindowsPrincipal($id)
    if (-not $p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw "需要管理员权限来注册服务。请以管理员身份重新运行 PowerShell。"
    }
}

Assert-Admin

# 幂等卸载：服务不存在也不报错
if ($Uninstall) {
    if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) {
        Stop-Service  -Name $ServiceName -Force -ErrorAction SilentlyContinue
        sc.exe delete $ServiceName | Out-Null
        Write-Host "服务已删除"
    } else {
        Write-Host "服务未安装，无需卸载"
    }
    if (Test-Path $InstallDir) { Remove-Item -Recurse -Force $InstallDir }
    Write-Host "卸载完成"
    exit 0
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

# token 写进服务专属的环境，不进命令行 —— 命令行在任务管理器里可见
$envFile = Join-Path $InstallDir "env"
@"
PULSE_SERVER=$Server
PULSE_TOKEN=$Token
"@ | Set-Content -Path $envFile -Encoding UTF8
# 只有管理员与服务账号可读
icacls $envFile /inheritance:r /grant:r "Administrators:(R)" "NT SERVICE\$ServiceName:(R)" | Out-Null

# 幂等：已存在则先删再建，配置改动才能生效
if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) {
    Write-Host "服务已存在，重新注册以应用新配置"
    Stop-Service -Name $ServiceName -Force -ErrorAction SilentlyContinue
    sc.exe delete $ServiceName | Out-Null
    Start-Sleep -Seconds 2
}

$exe = Join-Path $InstallDir "pulse-agent.exe"
if (-not (Test-Path $exe)) { throw "找不到 $exe，请先把二进制放到该目录" }

# 以虚拟服务账号运行，不是 LocalSystem —— 权限最小化
sc.exe create $ServiceName binPath= "`"$exe`"" start= auto obj= "NT SERVICE\$ServiceName" | Out-Null
sc.exe description $ServiceName "Pulse monitoring agent (read-only metrics)" | Out-Null
# 崩溃后自动重启，但有退避，避免无限重启
sc.exe failure $ServiceName reset= 600 actions= restart/5000/restart/10000/restart/30000 | Out-Null

Start-Service -Name $ServiceName

# 副作用后验证：轮询到 Running 为止，失败时打印诊断而不是让用户自己去翻
$deadline = (Get-Date).AddSeconds(30)
while ((Get-Date) -lt $deadline) {
    $svc = Get-Service -Name $ServiceName
    if ($svc.Status -eq "Running") { Write-Host "✔ 服务已启动"; exit 0 }
    Start-Sleep -Milliseconds 500
}
Write-Warning "服务在 30 秒内未进入 Running 状态，最近的事件日志："
Get-EventLog -LogName Application -Newest 20 -ErrorAction SilentlyContinue |
    Where-Object { $_.Source -like "*pulse*" } | Format-List
exit 1
