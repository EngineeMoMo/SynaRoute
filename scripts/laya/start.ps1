[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'SynaRoute\laya'),
    [ValidateRange(1024, 65535)][int]$Port = 8000,
    [ValidateRange(1, 64)][int]$Threads = 2,
    [switch]$CheckOnly,
    [switch]$Managed,
    [switch]$Repair,
    [switch]$NoInstall,
    [switch]$ModelMirror
)
$ErrorActionPreference = 'Stop'
# Verified together with laya 0.3.29 and requirements.lock.txt (real install and inference); update all
# of them together. tokenizer_config.json is not pinned: laya rewrites it in place before loading.
$PinnedRevision = '7b928d828b7b0e022f929d9bd2e44165aa270148'
$PinnedDigests = '{"rl_agent_config.json":"25061739243b617ad88d1219ba6f8a9c86c5881ca28df024fa2d9b3b2fcc30c6","model.safetensors":"9d628fd971b700382ac6f65920a86f149777b2e748e0c955fb3b19695aa8f204","encoder/config.json":"83f6916d13ef0f556ac461f28308dc2bffa7ebeadee8ec9e2db5812020ea5bb4","tokenizer/tokenizer.json":"609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f"}'
# python.org's published SHA-256 for python-3.12.10-amd64.exe (the same digest is in its sigstore bundle).
$PythonSha256 = '67b5635e80ea51072b87941312d00ec8927c4db9ba18938f7ad2d27b328b95fb'
if ($Managed) {
    [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
    $OutputEncoding = [Console]::OutputEncoding
    if ([Console]::ReadLine() -ne 'start') { exit 1 }
    $env:PYTHONIOENCODING = 'utf-8'
    $env:PYTHONUNBUFFERED = '1'
    $env:PIP_DEFAULT_TIMEOUT = '60'
    $env:PIP_RETRIES = '5'
    $env:PIP_DISABLE_PIP_VERSION_CHECK = '1'
    $env:HF_HUB_ETAG_TIMEOUT = '20'
    $env:HF_HUB_DOWNLOAD_TIMEOUT = '30'
    # Only the app-owned loopback instance runs without an inherited API credential.
    Remove-Item Env:LAYA_API_KEY -ErrorAction SilentlyContinue
}
function Test-Python([string]$Path, [switch]$Build) {
    if (!(Test-Path -LiteralPath $Path -PathType Leaf)) { return $false }
    $ErrorActionPreference = 'Continue'
    $global:LASTEXITCODE = 1
    # A new environment needs exactly what the hash-locked wheels were built for: CPython 3.12 on x64.
    # Running an existing environment keeps the older 3.10-3.13 rule, so a working install never rebuilds.
    $code = if ($Build) { "import sys, sysconfig; sys.exit(0 if sys.version_info[:2] == (3,12) and sysconfig.get_platform() == 'win-amd64' else 1)" } else { 'import sys; sys.exit(0 if (3,10) <= sys.version_info[:2] <= (3,13) else 1)' }
    try { & $Path -c $code 2>$null }
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
function Test-Writable([string]$Directory) {
    try { New-Item -ItemType Directory -Path $Directory -Force | Out-Null }
    catch { throw "[permission] 无法创建目录 $Directory / cannot create directory." }
    $probe = Join-Path $Directory ('write-test-' + [Guid]::NewGuid().ToString('N'))
    try { [IO.File]::WriteAllText($probe, 'test'); [IO.File]::Delete($probe) }
    catch { throw "[permission] 目录 $Directory 不可写，请检查目录权限 / directory is not writable." }
}
function Get-FreeBytes([string]$Root) {
    # DriveInfo rejects UNC roots; the caller then skips the space check rather than guessing.
    try { return [IO.DriveInfo]::new($Root).AvailableFreeSpace } catch { return $null }
}
function Test-Space($Needs) {
    # Directories on the same drive share its free space, so needs are summed per drive before comparing.
    $byRoot = [ordered]@{}
    foreach ($need in $Needs) {
        $root = [IO.Path]::GetPathRoot([IO.Path]::GetFullPath($need.Path))
        if ($byRoot.Contains($root)) { $byRoot[$root] += $need.Bytes } else { $byRoot[$root] = $need.Bytes }
    }
    foreach ($root in $byRoot.Keys) {
        $free = Get-FreeBytes $root
        if ($null -eq $free) { Write-Host "无法读取 $root 的可用空间，跳过空间预检。"; continue }
        if ($free -lt $byRoot[$root]) { throw ('[disk] 磁盘 {0} 可用 {1:N2} GiB，本次需要约 {2:N2} GiB（安装目录、模型缓存、pip 缓存与临时文件中位于该盘的部分合计）/ insufficient free space.' -f $root, ($free / 1GB), ($byRoot[$root] / 1GB)) }
    }
}
function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "命令失败（退出码 $LASTEXITCODE）。请查看上方错误，修复后重新运行。" }
}
function Find-Python {
    # Only an interpreter that can build the locked environment counts (see Test-Python -Build).
    $ErrorActionPreference = 'Continue'
    $candidates = @((Join-Path $env:LOCALAPPDATA 'Programs\Python\Python312\python.exe'))
    $command = Get-Command python.exe -ErrorAction SilentlyContinue
    if ($command -and $command.Source -notlike '*WindowsApps*') { $candidates += $command.Source }
    $launcher = Get-Command py.exe -ErrorAction SilentlyContinue
    if ($launcher) {
        $global:LASTEXITCODE = 1
        $found = & $launcher.Source '-3.12' -c 'import sys; print(sys.executable)' 2>$null
        if ($LASTEXITCODE -eq 0 -and $found) { $candidates += $found }
    }
    foreach ($candidate in $candidates) {
        if (Test-Python $candidate -Build) { return $candidate }
    }
    return $null
}
function Assert-Architecture {
    # The locked wheels are win_amd64. Windows on Arm runs them under x64 emulation, which only
    # Windows 11 (build 22000+) provides; 32-bit Windows cannot run them at all.
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ($arch -eq 'AMD64') { return }
    if ($arch -eq 'ARM64') {
        $build = 0
        try { $build = [int](Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' -ErrorAction Stop).CurrentBuildNumber } catch {}
        if ($build -ge 22000) { Write-Host 'Arm 设备：将使用 x64 版 Python 与依赖，通过系统 x64 模拟运行，速度可能低于 x64 设备。'; return }
    }
    throw ('[platform] 当前系统（{0}）无法运行本地服务所需的 x64 组件；Arm 设备需要 Windows 11。可改用其他设备上的 Laya 服务并填写其接口 / unsupported platform.' -f $arch)
}
function Install-Python {
    $downloadDir = Join-Path $InstallDir 'downloads'
    New-Item -ItemType Directory -Path $downloadDir -Force | Out-Null
    $setup = Join-Path $downloadDir 'python-3.12.10-amd64.exe'
    $url = 'https://www.python.org/ftp/python/3.12.10/python-3.12.10-amd64.exe'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $downloaded = $false
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        Write-Host "正在从 Python 官网下载 Python 3.12.10（约 26 MB），第 $attempt / 3 次尝试…"
        try {
            $request = @{ Uri = $url; OutFile = $setup; UseBasicParsing = $true; TimeoutSec = 120; ErrorAction = 'Stop' }
            if ($env:HTTPS_PROXY) { $request.Proxy = $env:HTTPS_PROXY }
            $ProgressPreference = 'SilentlyContinue'
            Invoke-WebRequest @request | Out-Null
            $downloaded = $true
            break
        } catch {
            if ($attempt -eq 3) { throw ('[network] Python 官网下载失败，已尝试 3 次：{0}。请检查网络或代理后点击重试，将自动重新下载。' -f $_.Exception.Message) }
        }
    }
    if (!$downloaded) { throw '[network] Python 下载未完成，请重试。' }
    # The exact published file, before trusting the signature on it; a mismatch is never run.
    if ((Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash -ne $PythonSha256) {
        Remove-Item -LiteralPath $setup -Force -ErrorAction SilentlyContinue
        throw '[python] 下载的 Python 安装包与 python.org 公布的 SHA-256 不一致，已删除且未执行。可能是下载不完整，或网络、代理改写了文件；请检查网络后点击重试。'
    }
    Write-Host '正在验证 Python 安装包的发布者签名…'
    $signature = Get-AuthenticodeSignature -LiteralPath $setup
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch '(?:^|,\s*)O=Python Software Foundation(?:,|$)') {
        throw '[python] Python 安装包签名验证失败，未执行安装。请检查网络及系统时间后点击重试，程序会重新下载。'
    }
    Write-Host '正在为当前用户安装 Python，完成后自动继续…'
    # Stopping inside the installer can leave a half-installed Python; the desktop defers a stop
    # request until the end marker. The end marker is written however the installer returns.
    if ($Managed) { Write-Host 'SYNAROUTE_CRITICAL:begin' }
    try {
        $process = Start-Process -FilePath $setup -ArgumentList @('/quiet', '/norestart', 'InstallAllUsers=0', 'Include_launcher=0', 'PrependPath=0', 'AppendPath=0', 'Include_test=0', 'Include_doc=0', 'Shortcuts=0', 'Include_pip=1') -WindowStyle Hidden -Wait -PassThru
    } finally {
        if ($Managed) { Write-Host 'SYNAROUTE_CRITICAL:end' }
    }
    if ($process.ExitCode -notin @(0, 3010)) { throw "[python] Python 安装失败（退出码 $($process.ExitCode)）。请点击重试以重新下载安装。" }
    $found = Find-Python
    if (!$found) { throw '[python] Python 安装完成但运行校验未通过，请点击重试；若系统提示需要重启，请重启后重试。仍失败时可在「应用和功能」中卸载 Python 3.12.10 后再重试。' }
    # The installer keeps its own copy for repair and uninstall; ours is no longer needed.
    Remove-Item -LiteralPath $setup -Force -ErrorAction SilentlyContinue
    return $found
}
function Remove-StaleEnvironments([string]$Directory, [string]$Keep) {
    # Failed or cancelled attempts leave env-<id> directories (often hundreds of MB) behind; only the
    # pointer target is kept. Links are skipped: they are not ours, and safety must not depend on how
    # Remove-Item treats a link (5.1.26100 removes only the link; older builds differ).
    foreach ($dir in Get-ChildItem -LiteralPath $Directory -Directory -Force -ErrorAction SilentlyContinue) {
        if ($dir.Name -notmatch '^env-[a-f0-9]{32}$' -or $dir.Name -eq $Keep -or ($dir.Attributes -band [IO.FileAttributes]::ReparsePoint)) { continue }
        $name = $dir.Name
        try { Remove-Item -LiteralPath $dir.FullName -Recurse -Force -ErrorAction Stop; Write-Host "已清理未使用的环境 $name" }
        catch { Write-Host "未能清理环境 $name，可稍后手动删除。" }
    }
}
function Get-HubRepo {
    $hub = if ($env:HF_HUB_CACHE) { $env:HF_HUB_CACHE } elseif ($env:HF_HOME) { Join-Path $env:HF_HOME 'hub' } else { Join-Path $env:USERPROFILE '.cache\huggingface\hub' }
    return (Join-Path $hub 'models--convaiinnovations--laya')
}
function Test-ModelCached([string]$Revision) {
    return ($Revision -match '^[0-9a-f]{40}$' -and (Test-Path -LiteralPath (Join-Path (Get-HubRepo) "snapshots\$Revision\multilingual\model.safetensors")))
}
function Get-ModelRevision {
    # Unpinned loads ask the Hub for the newest commit at every start: about 20 s extra where
    # huggingface.co is unreachable, and weights that can change under the pinned package.
    # Prefer the snapshot already cached so an existing install never downloads again.
    $ref = Join-Path (Get-HubRepo) 'refs\main'
    if (Test-Path -LiteralPath $ref -PathType Leaf) {
        $sha = ([IO.File]::ReadAllText($ref)).Trim()
        if (Test-ModelCached $sha) { return $sha }
    }
    return $PinnedRevision
}
function Get-ManagedProxy($Settings = (Get-ItemProperty -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings' -ErrorAction SilentlyContinue), [scriptblock]$ResolvePac = { param([Uri]$Target) [Net.WebRequest]::GetSystemWebProxy().GetProxy($Target) }) {
    # pip and huggingface_hub read only a manual proxy, never a PAC script. Like Windows, a configured
    # PAC script wins over the manual entry; its answer for pypi.org is used for every download.
    if ($Settings.AutoConfigURL) {
        $target = [Uri]'https://pypi.org/'
        try {
            $resolved = & $ResolvePac $target
            if ($resolved -and $resolved.AbsoluteUri -ne $target.AbsoluteUri) { return $resolved.GetLeftPart([UriPartial]::Authority) }
            if ($resolved) { return $null }
        } catch {}
    }
    if ($Settings.ProxyEnable -eq 1 -and $Settings.ProxyServer) {
        $address = $Settings.ProxyServer
        if ($address -match '(?:^|;)https=([^;]+)') { $address = $Matches[1] }
        elseif ($address.Contains('=')) { return $null }
        if ($address -notmatch '^https?://') { $address = 'http://' + $address }
        return $address
    }
    return $null
}
function Test-PortAnswers([int]$TcpPort) {
    # A bind test is not enough on Windows: 127.0.0.1:P binds even while another program serves 0.0.0.0:P or [::]:P.
    $client = [Net.Sockets.TcpClient]::new()
    try { return ($client.ConnectAsync('127.0.0.1', $TcpPort).Wait(300) -and $client.Connected) }
    catch { return $false }
    finally { $client.Dispose() }
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
    if ($CheckOnly) {
        $found = if ($venvUsable) { $venvPython } else { Find-Python }
        if ($found) { Write-Host "Python：$found" } else { Write-Host '未找到 Python 3.12（x64）；确认安装后将从 Python 官网自动下载并安装。' }
        Write-Host "安装目录：$InstallDir"
        Write-Host "接口：http://127.0.0.1:$Port/v1/systemone"
        Write-Host '检查完成，没有安装或启动服务。'
        exit 0
    }
    if ($Managed -and !$env:HTTPS_PROXY) {
        $proxy = Get-ManagedProxy
        if ($proxy) { $env:HTTPS_PROXY = $proxy; if (!$env:HTTP_PROXY) { $env:HTTP_PROXY = $proxy } }
    }
    # Keep the lock for the whole service lifetime, including across app instances.
    Test-Writable $InstallDir
    try { $installLock = [IO.File]::Open((Join-Path $InstallDir 'install.lock'), 'OpenOrCreate', 'ReadWrite', 'None') }
    catch { throw '[locked] 另一个 Laya 安装或服务正在使用此环境，请先停止它 / another installer or service owns this environment.' }
    $installed = $venvUsable -and (Test-Laya $venvPython) -and (Test-Path -LiteralPath $server -PathType Leaf)
    $setup = !$installed -or $Repair
    # The desktop starts an installed environment without the download dialog; it must never install from that path.
    if ($NoInstall -and $setup) { throw '[install] 本地环境需要安装或修复，未下载任何内容 / the local environment needs setup; nothing was downloaded.' }
    # Managed launches are confirmed in the desktop UI; standalone launches ask here.
    if (!$Managed -and $setup) {
        Write-Host '将联网下载缺失的 Python（官网安装包约 26 MB）、CPU PyTorch、Laya 及模型。首次安装约需 3.5 GB 磁盘空间。'
        if ((Read-Host '确认下载并安装？输入 Y 继续，其他输入取消') -notmatch '^(?i:y|yes)$') { Write-Host '已取消，未下载或安装。'; exit 0 }
    }
    # Without a complete cached snapshot this is the pinned revision, so a download (from the Hub or the
    # mirror) is always checked against the pinned digests below.
    if (!$env:LAYA_REVISION) { $env:LAYA_REVISION = Get-ModelRevision }
    $cacheDir = if ($env:HF_HUB_CACHE) { $env:HF_HUB_CACHE } elseif ($env:HF_HOME) { $env:HF_HOME } else { Join-Path $env:USERPROFILE '.cache\huggingface' }
    Test-Writable $cacheDir
    # Measured: environment 0.91 GB, model 0.65 GB, pip cache 0.18 GB; each need carries headroom.
    $needs = @(@{ Path = $InstallDir; Bytes = $(if ($setup) { 1.5GB } else { 128MB }) })
    if (!(Test-ModelCached $env:LAYA_REVISION)) { $needs += @{ Path = $cacheDir; Bytes = 1GB } }
    if ($setup) {
        Test-Writable ([IO.Path]::GetTempPath())
        $pipCache = if ($env:PIP_CACHE_DIR) { $env:PIP_CACHE_DIR } else { Join-Path $env:LOCALAPPDATA 'pip\Cache' }
        $needs += @(@{ Path = [IO.Path]::GetTempPath(); Bytes = 512MB }, @{ Path = $pipCache; Bytes = 512MB })
    }
    Test-Space $needs
    if ($setup) {
        $lockFile = Join-Path $PSScriptRoot 'requirements.lock.txt'
        if (!(Test-Path -LiteralPath $lockFile -PathType Leaf)) { throw '[environment] 缺少依赖锁定文件 requirements.lock.txt（应与本脚本在同一目录），未下载任何内容 / dependency lock file is missing.' }
        Assert-Architecture
        # The previous environment's interpreter still builds the new one when it matches the lock.
        $pythonExe = if (Test-Python $venvPython -Build) { $venvPython } else { Find-Python }
        if (!$pythonExe) {
            if ($Managed) { Write-Output 'SYNAROUTE_STAGE:python' }
            $pythonExe = Install-Python
        }
        if ($Managed) { Write-Output 'SYNAROUTE_STAGE:dependencies' }
        # Build in a fresh directory; a failed repair never overwrites the last working environment.
        Remove-StaleEnvironments $InstallDir $environmentName
        $environmentName = 'env-' + [Guid]::NewGuid().ToString('N')
        $venv = Join-Path $InstallDir $environmentName
        Invoke-Checked $pythonExe @('-m', 'venv', $venv)
        $venvPython = Join-Path $venv 'Scripts\python.exe'
        $server = Join-Path $venv 'Scripts\laya-serve.exe'
        # Exact versions and SHA-256 of every wheel, as verified together; any other file is rejected.
        Write-Host '正在安装经过校验的 CPU 版 PyTorch 与 Laya 0.3.29（每个文件都核对 SHA-256）；下载失败会自动重试…'
        Invoke-Checked $venvPython @('-m', 'pip', 'install', '--require-hashes', '--only-binary', ':all:', '-r', $lockFile, '--index-url', 'https://download.pytorch.org/whl/cpu', '--extra-index-url', 'https://pypi.org/simple')
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
    # laya refuses to load a pinned-revision file whose SHA-256 differs (checked before any weight is parsed).
    if ($env:LAYA_REVISION -eq $PinnedRevision -and !$env:LAYA_SHA256_DIGESTS) { $env:LAYA_SHA256_DIGESTS = $PinnedDigests }
    if ($ModelMirror -and !$env:HF_ENDPOINT) {
        $env:HF_ENDPOINT = 'https://hf-mirror.com'
        if ($env:LAYA_REVISION -eq $PinnedRevision) { Write-Host '模型将从镜像 hf-mirror.com 下载，并逐个核对已验证的 SHA-256。' }
        else { Write-Host '模型将从镜像 hf-mirror.com 下载；当前模型版本不是已验证版本，无法核对 SHA-256。' }
    }
    # The desktop health probe runs every second; uvicorn's access log would push real errors out of the kept log.
    if ($Managed) { $env:LAYA_LOG_LEVEL = 'warning' }
    if (Test-PortAnswers $Port) { throw ('[port] 端口 {0} 已被其他程序占用，未启动服务。桌面版点击重试会自动换端口；独立运行请用 -Port 指定其他端口 / port {0} is already in use.' -f $Port) }
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
