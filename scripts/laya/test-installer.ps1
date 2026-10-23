# Offline fault tests. Executes the real installer with a deterministic fake Python executable.
$ErrorActionPreference = 'Stop'
$root = Join-Path ([IO.Path]::GetTempPath()) ('synaroute-laya-tests-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
$installer = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot 'start.ps1'))
$fake = Join-Path $root 'fake.exe'
Add-Type -OutputAssembly $fake -OutputType ConsoleApplication -TypeDefinition @"
using System;
using System.IO;
class FakePython {
  static int Main(string[] args) {
    string exe = System.Reflection.Assembly.GetExecutingAssembly().Location;
    string dir = Path.GetDirectoryName(exe);
    string trace = Environment.GetEnvironmentVariable("LAYA_TEST_TRACE");
    string command = String.Join(" ", args);
    File.AppendAllText(trace, Path.GetFileName(exe) + " " + command + Environment.NewLine);
    if (Path.GetFileName(exe) == "laya-serve.exe") {
      string digests = Environment.GetEnvironmentVariable("LAYA_SHA256_DIGESTS");
      File.AppendAllText(trace, "LAYA_LOG_LEVEL=" + Environment.GetEnvironmentVariable("LAYA_LOG_LEVEL") + " LAYA_REVISION=" + Environment.GetEnvironmentVariable("LAYA_REVISION") + " HF_ENDPOINT=" + Environment.GetEnvironmentVariable("HF_ENDPOINT") + " DIGESTS=" + (String.IsNullOrEmpty(digests) ? "unset" : "set") + Environment.NewLine);
      return 0;
    }
    if (args.Length > 1 && args[0] == "-c") {
      if (command.Contains("importlib.metadata")) return File.Exists(Path.Combine(dir, "valid")) ? 0 : 1;
      // The build check (3.12 x64): a "nobuild" marker simulates an older interpreter.
      if (command.Contains("sysconfig")) return File.Exists(Path.Combine(dir, "nobuild")) ? 1 : 0;
      return 0;
    }
    if (args.Length > 2 && args[1] == "venv") {
      string target = Path.Combine(args[2], "Scripts"); Directory.CreateDirectory(target);
      File.Copy(exe, Path.Combine(target,"python.exe")); File.Copy(exe,Path.Combine(target,"laya-serve.exe")); return 0;
    }
    if (command.Contains("pip install")) {
      if (Environment.GetEnvironmentVariable("LAYA_TEST_FAIL_PIP") == "1") { Console.Error.WriteLine("ProxyError: connection failed"); return 1; }
      // Only the hash-locked install produces a valid environment; an unlocked install fails validation.
      if (command.Contains("--require-hashes") && command.Contains("requirements.lock.txt")) File.WriteAllText(Path.Combine(dir,"valid"), "1");
      return 0;
    }
    return 1;
  }
}
"@
function Assert([bool]$Value, [string]$Message) { if (!$Value) { throw $Message } }
function Fixture([string]$Name) {
    $dir = Join-Path $root $Name
    $scripts = Join-Path $dir 'venv\Scripts'
    New-Item -ItemType Directory -Path $scripts -Force | Out-Null
    Copy-Item -LiteralPath $fake -Destination (Join-Path $scripts 'python.exe')
    Copy-Item -LiteralPath $fake -Destination (Join-Path $scripts 'laya-serve.exe')
    [IO.File]::WriteAllText((Join-Path $scripts 'valid'), '1')
    [IO.File]::WriteAllText((Join-Path $dir 'environment.txt'), 'venv')
    return $dir
}
# A free port, so a developer's own service on 8000 cannot fail (or be probed by) these tests.
$portProbe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0); $portProbe.Start()
$script:testPort = $portProbe.LocalEndpoint.Port; $portProbe.Stop()
function Run([string]$Dir, [switch]$Repair, [string]$Answer = 'Y', [switch]$Managed, [switch]$NoInstall, [switch]$Mirror, [int]$Port = $script:testPort, [string]$Script = $installer) {
    $env:LAYA_TEST_TRACE = Join-Path $root 'trace.txt'
    [IO.File]::WriteAllText($env:LAYA_TEST_TRACE, '')
    $args = @('-NoProfile','-ExecutionPolicy','Bypass','-File',$Script,'-InstallDir',$Dir,'-Port',"$Port")
    if ($Repair) { $args += '-Repair' }
    if ($Managed) { $args += '-Managed'; $Answer = 'start' }
    if ($NoInstall) { $args += '-NoInstall' }
    if ($Mirror) { $args += '-ModelMirror' }
    $ErrorActionPreference = 'Continue'
    $output = $Answer | & powershell.exe @args 2>&1
    $ErrorActionPreference = 'Stop'
    $script:resultCode = $LASTEXITCODE
    $script:resultText = $output -join "`n"
    $script:trace = [IO.File]::ReadAllText($env:LAYA_TEST_TRACE)
}
# Isolate caches and never use actual network or package installation.
$env:HF_HOME = Join-Path $root 'cache'
Remove-Item Env:HF_HUB_CACHE, Env:LAYA_REVISION, Env:HF_ENDPOINT, Env:LAYA_SHA256_DIGESTS -ErrorAction SilentlyContinue
Write-Host "Fixtures: $root"
$dir = Fixture 'reuse'
Run $dir
Assert ($resultCode -eq 0) "Reuse failed: $resultText"
Assert (!$trace.Contains('pip install') -and $trace.Contains('laya-serve.exe')) 'Reuse must skip pip and launch server'
Assert (!$trace.Contains('LAYA_LOG_LEVEL=warning')) 'Standalone launch must keep the default console log'
Run $dir -Managed
Assert ($resultCode -eq 0 -and $trace.Contains('LAYA_LOG_LEVEL=warning')) "Managed launch must drop the per-request access log: $resultText"
# Model revision: the complete cached snapshot when present, otherwise the verified commit; never "ask the Hub".
$pinned = '7b928d828b7b0e022f929d9bd2e44165aa270148'
Assert ($trace.Contains("LAYA_REVISION=$pinned")) 'Without a cache the verified revision must be pinned'
Assert ($trace.Contains('DIGESTS=set') -and $trace.Contains('HF_ENDPOINT= ')) 'The pinned revision must be digest-checked, and the Hub stays the default source'
# The mirror is opt-in; it serves the same pinned revision, so the digests still apply.
Run $dir -Managed -Mirror
Assert ($trace.Contains('HF_ENDPOINT=https://hf-mirror.com') -and $trace.Contains('DIGESTS=set') -and $trace.Contains("LAYA_REVISION=$pinned")) "Mirror must keep the pinned, digest-checked revision: $trace"
$env:HF_ENDPOINT = 'https://user-own.example'
Run $dir -Managed -Mirror
Assert ($trace.Contains('HF_ENDPOINT=https://user-own.example')) 'A user-set HF_ENDPOINT must win over the mirror switch'
Remove-Item Env:HF_ENDPOINT
$cachedSha = 'a' * 40
$repoCache = Join-Path $env:HF_HOME 'hub\models--convaiinnovations--laya'
New-Item -ItemType Directory -Path (Join-Path $repoCache 'refs') -Force | Out-Null
[IO.File]::WriteAllText((Join-Path $repoCache 'refs\main'), $cachedSha)
Run $dir -Managed
Assert ($trace.Contains("LAYA_REVISION=$pinned")) 'A cached ref without its snapshot must not be trusted'
$snapshot = Join-Path $repoCache "snapshots\$cachedSha\multilingual"
New-Item -ItemType Directory -Path $snapshot -Force | Out-Null
[IO.File]::WriteAllText((Join-Path $snapshot 'model.safetensors'), 'x')
Run $dir -Managed
Assert ($trace.Contains("LAYA_REVISION=$cachedSha")) 'The cached snapshot must be reused so an installed environment never needs the Hub'
Assert ($trace.Contains('DIGESTS=unset')) 'Digests belong to the pinned revision only; another cached revision must not be refused'
$env:LAYA_REVISION = 'user-pin'
Run $dir -Managed
Assert ($trace.Contains('LAYA_REVISION=user-pin')) 'An explicit LAYA_REVISION must win'
Remove-Item Env:LAYA_REVISION
Remove-Item -LiteralPath (Join-Path $env:HF_HOME 'hub') -Recurse -Force
# -NoInstall is the desktop start without the download dialog: launch an installed environment, install nothing otherwise.
Run $dir -Managed -NoInstall
Assert ($resultCode -eq 0 -and $trace.Contains('laya-serve.exe')) "NoInstall must launch an installed environment: $resultText"
$notInstalled = Join-Path $root 'not-installed'
Run $notInstalled -Managed -NoInstall
# The trace only sees the fake interpreter; a real system Python would build a real venv unseen, so check the directory too.
Assert ($resultCode -ne 0 -and $resultText.Contains('[install]') -and !$trace.Contains('venv') -and !$trace.Contains('pip')) "NoInstall must never install: $resultText"
Assert (@(Get-ChildItem -LiteralPath $notInstalled -Directory -Filter 'env-*' -ErrorAction SilentlyContinue).Count -eq 0) 'NoInstall created an environment'
Run $dir -Managed -NoInstall -Repair
Assert ($resultCode -ne 0 -and $resultText.Contains('[install]')) 'NoInstall must refuse a repair'
# Another program already serving the port must stop the launch, not be shadowed by it.
$squatter = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0); $squatter.Start()
try {
    Run $dir -Port $squatter.LocalEndpoint.Port
    Assert ($resultCode -ne 0 -and $resultText.Contains('[port]') -and !$trace.Contains('laya-serve.exe')) "Occupied port must be refused before launch: $resultText"
} finally { $squatter.Stop() }
Run $dir -Repair -Answer 'N'
Assert ($resultCode -eq 0 -and !$trace.Contains('pip install') -and !$trace.Contains('laya-serve.exe')) 'Declining setup must not install or start'
Assert (([IO.File]::ReadAllText((Join-Path $dir 'environment.txt'))) -eq 'venv') 'Declining setup changed environment'
$env:LAYA_TEST_FAIL_PIP = '1'
Run $dir -Repair
Assert ($resultCode -ne 0) 'Failed dependency download must fail setup'
Assert (([IO.File]::ReadAllText((Join-Path $dir 'environment.txt'))) -eq 'venv') 'Failed repair changed working pointer'
Assert (Test-Path -LiteralPath (Join-Path $dir 'venv\Scripts\valid')) 'Failed repair damaged old environment'
Assert (@(Get-ChildItem -LiteralPath $dir -Directory -Filter 'env-*').Count -eq 1) 'Failed repair should leave its partial environment'
$env:LAYA_TEST_FAIL_PIP = '0'
# Without the lock file next to the script nothing may be downloaded: no unpinned fallback install.
$lockless = Join-Path $root 'lockless'
New-Item -ItemType Directory -Path $lockless | Out-Null
Copy-Item -LiteralPath $installer -Destination (Join-Path $lockless 'start.ps1')
$lockDir = Fixture 'lock-missing'
Run $lockDir -Repair -Script (Join-Path $lockless 'start.ps1')
Assert ($resultCode -ne 0 -and $resultText.Contains('[environment]') -and !$trace.Contains(' -m venv') -and !$trace.Contains('pip')) "A missing lock file must stop setup before any install: $resultText"
Assert (([IO.File]::ReadAllText((Join-Path $lockDir 'environment.txt'))) -eq 'venv') 'A missing lock file changed the environment pointer'
# Paths with non-ASCII characters and spaces (common user names and folders) must work end to end.
$unicodeDir = Fixture ('laya ' + [string][char]0x4E2D + [char]0x6587 + ' path')
Run $unicodeDir -Repair -Managed
Assert ($resultCode -eq 0 -and $trace.Contains('laya-serve.exe') -and ([IO.File]::ReadAllText((Join-Path $unicodeDir 'environment.txt'))) -match '^env-[a-f0-9]{32}$') "Non-ASCII install path failed: $resultText"
# Decoys the cleanup must never touch: a near-miss name, and a junction to an outside directory.
New-Item -ItemType Directory -Path (Join-Path $dir 'env-keep-me') | Out-Null
$outside = Join-Path $root 'outside-target'
New-Item -ItemType Directory -Path $outside | Out-Null
[IO.File]::WriteAllText((Join-Path $outside 'sentinel'), 'x')
$junction = Join-Path $dir ('env-' + ('b' * 32))
New-Item -ItemType Junction -Path $junction -Target $outside | Out-Null
Run $dir -Repair
Assert ($resultCode -eq 0) "Repair failed: $resultText"
Assert ($trace.Contains('--require-hashes') -and $trace.Contains('--only-binary :all:') -and $trace.Contains('requirements.lock.txt') -and !$trace.Contains('--upgrade')) "Setup must install only from the hash lock: $trace"
$newEnv = [IO.File]::ReadAllText((Join-Path $dir 'environment.txt'))
Assert ($newEnv -match '^env-[a-f0-9]{32}$') 'Repair must atomically publish a new environment'
$real = @(Get-ChildItem -LiteralPath $dir -Directory -Filter 'env-*' | Where-Object { $_.Name -match '^env-[a-f0-9]{32}$' -and !($_.Attributes -band [IO.FileAttributes]::ReparsePoint) })
Assert ($real.Count -eq 1 -and $real[0].Name -eq $newEnv) 'Stale environments from failed attempts must be removed'
Assert ((Test-Path -LiteralPath (Join-Path $dir 'venv')) -and (Test-Path -LiteralPath (Join-Path $dir 'env-keep-me'))) 'Cleanup removed a directory it does not own'
Assert ((Test-Path -LiteralPath $junction) -and (Test-Path -LiteralPath (Join-Path $outside 'sentinel'))) 'Cleanup must not follow or remove links'
Run $dir
Assert ($resultCode -eq 0 -and !$trace.Contains('pip install')) 'Validated repair must be reused'
$lock = [IO.File]::Open((Join-Path $dir 'install.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
try { Run $dir; Assert ($resultCode -ne 0 -and $resultText.Contains('[locked]')) 'Concurrent setup did not reject lock' }
finally { $lock.Dispose() }
# Test real filesystem probes without installing or filling a disk.
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($installer, [ref]$tokens, [ref]$errors)
Assert ($errors.Count -eq 0) 'Installer parse failure'
function Source([string]$Name) { return $ast.Find({param($a) $a -is [Management.Automation.Language.FunctionDefinitionAst] -and $a.Name -eq $Name}.GetNewClosure(), $true).Extent.Text }
foreach ($name in 'Test-Writable', 'Get-FreeBytes', 'Test-Space', 'Test-Python', 'Get-ManagedProxy', 'Assert-Architecture') { Invoke-Expression (Source $name) }
$lowSpace = $false
try { Test-Space @(@{ Path = $root; Bytes = [long]::MaxValue }) } catch { $lowSpace = $_.Exception.Message.Contains('[disk]') }
Assert $lowSpace 'Insufficient disk was not detected'
# Two needs on one drive that each fit but not together: only a per-drive sum catches this.
$free = Get-FreeBytes ([IO.Path]::GetPathRoot($root))
Test-Space @(@{ Path = $root; Bytes = [long]($free * 0.6) })
$summed = $false
try { Test-Space @(@{ Path = $root; Bytes = [long]($free * 0.6) }, @{ Path = (Join-Path $root 'cache'); Bytes = [long]($free * 0.6) }) } catch { $summed = $_.Exception.Message.Contains('[disk]') }
Assert $summed 'Needs on the same drive must be summed'
Assert ($null -eq (Get-FreeBytes '\\unreachable-host.invalid\share\')) 'A UNC root must skip the space check rather than fail'
$blocked = Join-Path $root 'not-a-directory'
[IO.File]::WriteAllText($blocked, 'file')
$denied = $false
try { Test-Writable $blocked } catch { $denied = $_.Exception.Message.Contains('[permission]') }
Assert $denied 'Unwritable destination was not rejected'
$brokenExe = Join-Path $root 'broken.exe'
[IO.File]::WriteAllText($brokenExe, 'not an executable')
$global:LASTEXITCODE = 0
Assert (!(Test-Python $brokenExe)) 'Corrupt interpreter inherited stale successful exit code'
# A new environment needs the lock's 3.12 x64 interpreter; an existing one keeps running on 3.10-3.13.
$older = Join-Path (Fixture 'older-python') 'venv\Scripts\python.exe'
Assert ((Test-Python $older) -and (Test-Python $older -Build)) 'A matching interpreter must pass both checks'
[IO.File]::WriteAllText((Join-Path (Split-Path $older) 'nobuild'), '1')
Assert ((Test-Python $older) -and !(Test-Python $older -Build)) 'An older interpreter may run an environment but must not build one'
# pip reads only a manual proxy; the managed script resolves PAC itself.
$manual = [pscustomobject]@{ ProxyEnable = 1; ProxyServer = '127.0.0.1:7890'; AutoConfigURL = $null }
Assert ((Get-ManagedProxy $manual) -eq 'http://127.0.0.1:7890') 'Manual proxy'
Assert ((Get-ManagedProxy ([pscustomobject]@{ ProxyEnable = 1; ProxyServer = 'http=a:1;https=b:2' })) -eq 'http://b:2') 'Per-protocol proxy must use the https entry'
Assert ($null -eq (Get-ManagedProxy ([pscustomobject]@{ ProxyEnable = 0; ProxyServer = 'a:1' }))) 'Disabled proxy'
$pacAndManual = [pscustomobject]@{ ProxyEnable = 1; ProxyServer = '127.0.0.1:7890'; AutoConfigURL = 'http://wpad.example/proxy.pac' }
Assert ((Get-ManagedProxy $pacAndManual { param($t) [Uri]'http://corp-proxy:3128/' }) -eq 'http://corp-proxy:3128') 'A PAC answer wins over the manual entry'
Assert ($null -eq (Get-ManagedProxy $pacAndManual { param($t) $t })) 'PAC DIRECT means no proxy'
Assert ((Get-ManagedProxy $pacAndManual { param($t) throw 'pac failed' }) -eq 'http://127.0.0.1:7890') 'A failing PAC falls back to the manual entry'
# Locked wheels are x64: Arm needs Windows 11 x64 emulation; 32-bit Windows is refused.
$savedArch = $env:PROCESSOR_ARCHITECTURE; $savedWow = $env:PROCESSOR_ARCHITEW6432
function Get-ItemProperty { param($LiteralPath, $ErrorAction) [pscustomobject]@{ CurrentBuildNumber = $script:osBuild } }
try {
    $env:PROCESSOR_ARCHITEW6432 = $null
    $env:PROCESSOR_ARCHITECTURE = 'AMD64'; Assert-Architecture
    $env:PROCESSOR_ARCHITECTURE = 'ARM64'; $script:osBuild = '22631'; Assert-Architecture
    foreach ($case in @(@('ARM64', '19045'), @('x86', '19045'))) {
        $env:PROCESSOR_ARCHITECTURE = $case[0]; $script:osBuild = $case[1]
        $refused = $false
        try { Assert-Architecture } catch { $refused = $_.Exception.Message.Contains('[platform]') }
        Assert $refused "Architecture $($case[0]) build $($case[1]) must be refused"
    }
} finally { $env:PROCESSOR_ARCHITECTURE = $savedArch; $env:PROCESSOR_ARCHITEW6432 = $savedWow; Remove-Item Function:Get-ItemProperty }
# Exercise the real bootstrap function with mocked transport, signature and process APIs.
# No network or Python installation is performed by these fault tests.
$bootstrap = $ast.Find({param($a) $a -is [Management.Automation.Language.FunctionDefinitionAst] -and $a.Name -eq 'Install-Python'}, $true)
Invoke-Expression $bootstrap.Extent.Text
$InstallDir = Join-Path $root 'bootstrap'
$Managed = $false
$PythonSha256 = [regex]::Match([IO.File]::ReadAllText($installer), "PythonSha256 = '([0-9a-f]{64})'").Groups[1].Value
Assert ($PythonSha256.Length -eq 64) 'Installer must pin the Python installer digest'
$script:downloads = 0; $script:launched = 0; $script:downloadFails = $false; $script:events = @(); $script:setupThrows = $false
$script:signatureStatus = 'Valid'; $script:publisher = 'CN=Python Software Foundation, O=Python Software Foundation, C=US'
$script:setupExit = 0; $script:foundPython = 'fake-python.exe'; $script:fileHash = $PythonSha256.ToUpperInvariant()
function Get-FileHash { param($LiteralPath, $Algorithm) [pscustomobject]@{ Hash = $script:fileHash } }
function Invoke-WebRequest { param($Uri, $OutFile, [switch]$UseBasicParsing, $TimeoutSec, $ErrorAction, $Proxy)
    $script:downloads++
    if ($script:downloadFails) { throw 'offline' }
    Assert ($Uri -eq 'https://www.python.org/ftp/python/3.12.10/python-3.12.10-amd64.exe') 'Unexpected download source'
}
function Get-AuthenticodeSignature { param($LiteralPath)
    return [pscustomobject]@{ Status = $script:signatureStatus; SignerCertificate = [pscustomobject]@{ Subject = $script:publisher } }
}
function Start-Process { param($FilePath, $ArgumentList, $WindowStyle, [switch]$Wait, [switch]$PassThru)
    $script:launched++; $script:events += 'launch'
    Assert ($ArgumentList -contains 'InstallAllUsers=0') 'Python install must be per-user'
    Assert ($ArgumentList -contains '/norestart') 'Installer must not restart the machine'
    if ($script:setupThrows) { throw 'installer crashed' }
    return [pscustomobject]@{ ExitCode = $script:setupExit }
}
function Find-Python { return $script:foundPython }
Assert ((Install-Python) -eq 'fake-python.exe') 'Bootstrap did not return validated Python'
Assert ($script:launched -eq 1) 'Bootstrap did not run installer'
# A file that is not the published one is deleted and never run, before the signature is consulted.
$script:fileHash = '0' * 64
$rejected = $false
try { Install-Python } catch { $rejected = $_.Exception.Message.Contains('SHA-256') }
Assert ($rejected -and $script:launched -eq 1) 'A digest mismatch must never run the installer'
$script:fileHash = $PythonSha256.ToUpperInvariant()
# The desktop defers a stop while the installer runs; the end marker must follow even a crashed installer.
$Managed = $true; $script:setupThrows = $true; $script:events = @()
function Write-Host { param([Parameter(Position = 0)]$Object) $script:events += "$Object" }
try { Install-Python } catch {}
Remove-Item Function:Write-Host
$Managed = $false; $script:setupThrows = $false; $script:launched = 1
$begin = [array]::IndexOf($script:events, 'SYNAROUTE_CRITICAL:begin'); $launch = [array]::IndexOf($script:events, 'launch'); $end = [array]::IndexOf($script:events, 'SYNAROUTE_CRITICAL:end')
Assert ($begin -ge 0 -and $begin -lt $launch -and $launch -lt $end) "Critical markers must bracket the installer: $($script:events -join ' | ')"
$script:signatureStatus = 'HashMismatch'
$rejected = $false
try { Install-Python } catch { $rejected = $_.Exception.Message.Contains('[python]') }
Assert ($rejected -and $script:launched -eq 1) 'Invalid signature must never run'
$script:signatureStatus = 'Valid'; $script:publisher = 'CN=Unrelated Publisher, O=Unrelated Publisher'
$rejected = $false
try { Install-Python } catch { $rejected = $_.Exception.Message.Contains('[python]') }
Assert ($rejected -and $script:launched -eq 1) 'Unexpected publisher must never run'
$script:downloadFails = $true; $before = $script:downloads
$rejected = $false
try { Install-Python } catch { $rejected = $_.Exception.Message.Contains('[network]') }
Assert ($rejected -and ($script:downloads - $before) -eq 3 -and $script:launched -eq 1) 'Download retries must be bounded and never launch on failure'
$script:downloadFails = $false; $script:publisher = 'O=Python Software Foundation'; $script:setupExit = 1603
$rejected = $false
try { Install-Python } catch { $rejected = $_.Exception.Message.Contains('1603') }
Assert $rejected 'Installer failure code must be reported'
$script:setupExit = 3010
Assert ((Install-Python) -eq 'fake-python.exe') 'Successful reboot-required exit must validate interpreter'
$script:foundPython = $null
$rejected = $false
try { Install-Python } catch { $rejected = $_.Exception.Message.Contains('[python]') }
Assert $rejected 'Missing interpreter after installation must fail'
Write-Host 'PASS: Python bootstrap, digest and signature and publisher rejection, critical-section markers, bounded download retries, installer exit codes, post-install validation.'
Write-Host 'PASS: corrupt interpreter, build-interpreter check, reuse, managed log level, model revision pin, pinned digests, opt-in mirror, no-install start, occupied port, failed repair rollback, missing lock, hash-locked install, non-ASCII path, stale environment cleanup, successful repair, repaired reuse, concurrent lock, per-drive space, UNC skip, blocked directory, PAC and manual proxy, platform check.'
# Fixtures stay on disk only when an assertion failed (the script stops there); remove the link first.
[IO.Directory]::Delete($junction)
Remove-Item -LiteralPath $root -Recurse -Force
Write-Host 'Fixtures removed.'
