[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'SynaRoute\laya'),
    [ValidateRange(1024, 65535)][int]$Port = 8000,
    [ValidateRange(1, 64)][int]$Threads = 2,
    [switch]$CheckOnly,
    [switch]$Managed,
    [switch]$Repair
)
$ErrorActionPreference = 'Stop'
if ($Managed) {
    [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
    $OutputEncoding = [Console]::OutputEncoding
    if ([Console]::ReadLine() -ne 'start') { exit 1 }
    $env:PYTHONIOENCODING = 'utf-8'
    $env:PYTHONUNBUFFERED = '1'
    $env:PIP_DEFAULT_TIMEOUT = '30'
    $env:PIP_RETRIES = '3'
    $env:PIP_DISABLE_PIP_VERSION_CHECK = '1'
    $env:HF_HUB_ETAG_TIMEOUT = '20'
    $env:HF_HUB_DOWNLOAD_TIMEOUT = '30'
    # Only the app-owned loopback instance runs without an inherited API credential.
    Remove-Item Env:LAYA_API_KEY -ErrorAction SilentlyContinue
    if (!$env:HTTPS_PROXY) {
        $proxySettings = Get-ItemProperty -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings' -ErrorAction SilentlyContinue
        if ($proxySettings.ProxyEnable -eq 1 -and $proxySettings.ProxyServer) {
            $proxyAddress = $proxySettings.ProxyServer
            if ($proxyAddress -match '(?:^|;)https=([^;]+)') { $proxyAddress = $Matches[1] }
            elseif ($proxyAddress.Contains('=')) { $proxyAddress = $null }
            if ($proxyAddress) {
                if ($proxyAddress -notmatch '^https?://') { $proxyAddress = 'http://' + $proxyAddress }
                $env:HTTPS_PROXY = $proxyAddress
                if (!$env:HTTP_PROXY) { $env:HTTP_PROXY = $proxyAddress }
            }
        }
    }
}
function Test-Python([string]$Path) {
    if (!(Test-Path -LiteralPath $Path -PathType Leaf)) { return $false }
    $ErrorActionPreference = 'Continue'
    $global:LASTEXITCODE = 1
    try { & $Path -c 'import sys; sys.exit(0 if (3,10) <= sys.version_info[:2] <= (3,13) else 1)' 2>$null }
    catch { return $false }
    return ($LASTEXITCODE -eq 0)
}
function Set-EnvironmentPointer([string]$Name) {
    $marker = Join-Path $InstallDir 'environment.txt'
    $temporary = Join-Path $InstallDir ('environment-' + [Guid]::NewGuid().ToString('N') + '.tmp')
    [IO.File]::WriteAllText($temporary, $Name)
    if (Test-Path -LiteralPath $marker) { [IO.File]::Replace($temporary, $marker, [NullString]::Value) }
    else { [IO.File]::Move($temporary, $marker) }
}
function Test-Storage([string]$Directory, [long]$MinimumBytes) {
    try { New-Item -ItemType Directory -Path $Directory -Force | Out-Null }
    catch { throw '[permission] 无法创建安装或缓存目录 / cannot create installation or cache directory.' }
    $probe = Join-Path $Directory ('write-test-' + [Guid]::NewGuid().ToString('N'))
    try { [IO.File]::WriteAllText($probe, 'test'); [IO.File]::Delete($probe) }
    catch { throw '[permission] 安装或缓存目录不可写，请检查目录权限 / installation or cache directory is not writable.' }
    $drive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot([IO.Path]::GetFullPath($Directory)))
    if ($drive.AvailableFreeSpace -lt $MinimumBytes) { throw '[disk] 可用磁盘空间不足，请至少预留 6 GB 后重试 / reserve at least 6 GB of free disk space.' }
}
function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "命令失败（退出码 $LASTEXITCODE）。请查看上方错误，修复后重新运行。" }
}
function Find-Python {
    $ErrorActionPreference = 'Continue'
    $candidates = @((Join-Path $env:LOCALAPPDATA 'Programs\Python\Python312\python.exe'))
    $command = Get-Command python.exe -ErrorAction SilentlyContinue
    if ($command -and $command.Source -notlike '*WindowsApps*') { $candidates += $command.Source }
    $launcher = Get-Command py.exe -ErrorAction SilentlyContinue
    if ($launcher) {
        foreach ($version in @('-3.12', '-3.11', '-3.10', '-3.13')) {
            $global:LASTEXITCODE = 1
            $found = & $launcher.Source $version -c 'import sys; print(sys.executable)' 2>$null
            if ($LASTEXITCODE -eq 0 -and $found) { $candidates += $found }
        }
    }
    foreach ($candidate in $candidates) {
        if (Test-Python $candidate) { return $candidate }
    }
    return $null
}
function Test-Laya([string]$PythonPath) {
    $ErrorActionPreference = 'Continue'
    $global:LASTEXITCODE = 1
    & $PythonPath -c "import importlib.metadata as m; import torch, fastapi, uvicorn, multipart; import laya.serve; assert m.version('laya') == '0.3.29'" 2>$null
    return ($LASTEXITCODE -eq 0)
}
try {
    if ($Managed) { Write-Output 'SYNAROUTE_STAGE:checking' }
    Write-Host 'SynaRoute · Laya 本地判断服务（CPU / 多语言）' -ForegroundColor Cyan
    Write-Host '首次运行会联网安装依赖与下载模型，需要磁盘空间；模型占用以实际运行结果为准。'
    $InstallDir = [IO.Path]::GetFullPath($InstallDir)
    $environmentName = 'venv'
    $marker = Join-Path $InstallDir 'environment.txt'
    if (Test-Path -LiteralPath $marker) {
        $savedName = ([IO.File]::ReadAllText($marker)).Trim()
        if ($savedName -match '^(venv|env-[a-f0-9]{32})$') { $environmentName = $savedName }
    }
    $venv = Join-Path $InstallDir $environmentName
    $venvPython = Join-Path $venv 'Scripts\python.exe'
    $server = Join-Path $venv 'Scripts\laya-serve.exe'
    # Prefer an existing private environment before searching for system Python.
    $venvUsable = Test-Python $venvPython
    $pythonExe = if ($venvUsable) { $venvPython } else { Find-Python }
    if ($CheckOnly) {
        if ($pythonExe) { Write-Host "Python：$pythonExe" } else { Write-Host '未找到 Python 3.10–3.13；正常启动会尝试用 winget 安装 Python 3.12。' }
        Write-Host "安装目录：$InstallDir"
        Write-Host "接口：http://127.0.0.1:$Port/v1/systemone"
        Write-Host '检查完成，没有安装或启动服务。'
        exit 0
    }
    # Keep the lock for the whole service lifetime, including across app instances.
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    try { $installLock = [IO.File]::Open((Join-Path $InstallDir 'install.lock'), 'OpenOrCreate', 'ReadWrite', 'None') }
    catch { throw '[locked] 另一个 Laya 安装或服务正在使用此环境，请先停止它 / another installer or service owns this environment.' }
    $installed = $venvUsable -and (Test-Laya $venvPython) -and (Test-Path -LiteralPath $server -PathType Leaf)
    Test-Storage $InstallDir $(if (!$installed -or $Repair) { 6GB } else { 128MB })
    $cacheDir = if ($env:HF_HUB_CACHE) { $env:HF_HUB_CACHE } elseif ($env:HF_HOME) { $env:HF_HOME } else { Join-Path $env:USERPROFILE '.cache\huggingface' }
    Test-Storage $cacheDir $(if (!$installed -or $Repair) { 6GB } else { 128MB })
    if (!$installed -or $Repair) { Test-Storage ([IO.Path]::GetTempPath()) 6GB }
    if (!$pythonExe) {
        if ($Managed) { Write-Output 'SYNAROUTE_STAGE:python' }
        if (!(Get-Command winget.exe -ErrorAction SilentlyContinue)) { throw '[python] 请先安装 Python 3.12（python.org），然后重新运行；或安装 Windows 应用安装程序以启用 winget。' }
        Write-Host '正在为当前用户安装 Python 3.12…'
        Invoke-Checked 'winget.exe' @('install', '--id', 'Python.Python.3.12', '--exact', '--scope', 'user', '--accept-source-agreements', '--accept-package-agreements', '--disable-interactivity')
        $pythonExe = Find-Python
        if (!$pythonExe) { throw 'Python 安装后尚未找到，请重新打开终端再运行。' }
    }
    if (!$installed -or $Repair) {
        if ($Managed) { Write-Output 'SYNAROUTE_STAGE:dependencies' }
        # Build in a fresh directory; a failed repair never overwrites the last working environment.
        $environmentName = 'env-' + [Guid]::NewGuid().ToString('N')
        $venv = Join-Path $InstallDir $environmentName
        Invoke-Checked $pythonExe @('-m', 'venv', $venv)
        $venvPython = Join-Path $venv 'Scripts\python.exe'
        $server = Join-Path $venv 'Scripts\laya-serve.exe'
        Write-Host '正在安装 CPU 版 PyTorch 与 Laya 0.3.29；网络请求最多重试 3 次…'
        Invoke-Checked $venvPython @('-m', 'pip', 'install', '--upgrade', 'pip')
        Invoke-Checked $venvPython @('-m', 'pip', 'install', 'torch', '--index-url', 'https://download.pytorch.org/whl/cpu')
        Invoke-Checked $venvPython @('-m', 'pip', 'install', 'laya[serve]==0.3.29')
        if (!(Test-Laya $venvPython) -or !(Test-Path -LiteralPath $server)) { throw '[environment] 安装后校验失败，请点击修复环境 / installed environment validation failed; repair and retry.' }
        Set-EnvironmentPointer $environmentName
    } else {
        Write-Host '检测到可用的 Laya 环境，跳过安装，直接启动。'
    }
    $env:LAYA_HOST = '127.0.0.1'
    $env:LAYA_PORT = "$Port"
    $env:LAYA_DEVICE = 'cpu'
    $env:LAYA_MODELS = 'multilingual'
    $env:LAYA_DEFAULT_MODEL = 'multilingual'
    $env:LAYA_PRELOAD = '1'
    $env:LAYA_MAX_LOADED = '1'
    $env:LAYA_THREADS = "$Threads"
    $env:LAYA_AUTO_TASK = '0'
    $env:LAYA_JEV_STRICT = '0'
    $env:LAYA_MAX_CONCURRENT = '2'
    $env:LAYA_IDLE_UNLOAD_SECONDS = '0'
    if ($Managed) { Write-Output 'SYNAROUTE_STAGE:model' }
    Write-Host "启动中；等待模型加载完成。接口：http://127.0.0.1:$Port/v1/systemone" -ForegroundColor Green
    Write-Host '在 SynaRoute 的「动态思考」选择 Laya，先使用「仅建议」检查运行日志。'
    if (!$Managed) { Write-Host '此窗口需要保持打开。按 Ctrl+C 停止；再次运行会复用环境和模型缓存。' }
    Invoke-Checked $server @()
} catch {
    Write-Host $_.Exception.Message -ForegroundColor Red
    exit 1
} finally {
    if ($installLock) { $installLock.Dispose() }
}