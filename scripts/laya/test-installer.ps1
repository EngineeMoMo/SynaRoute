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
    if (Path.GetFileName(exe) == "laya-serve.exe") return 0;
    if (args.Length > 1 && args[0] == "-c") {
      if (command.Contains("importlib.metadata")) return File.Exists(Path.Combine(dir, "valid")) ? 0 : 1;
      return 0;
    }
    if (args.Length > 2 && args[1] == "venv") {
      string target = Path.Combine(args[2], "Scripts"); Directory.CreateDirectory(target);
      File.Copy(exe, Path.Combine(target,"python.exe")); File.Copy(exe,Path.Combine(target,"laya-serve.exe")); return 0;
    }
    if (command.Contains("pip install")) {
      if (Environment.GetEnvironmentVariable("LAYA_TEST_FAIL_PIP") == "1") { Console.Error.WriteLine("ProxyError: connection failed"); return 1; }
      if (command.Contains("laya[serve]")) File.WriteAllText(Path.Combine(dir,"valid"), "1");
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
function Run([string]$Dir, [switch]$Repair) {
    $env:LAYA_TEST_TRACE = Join-Path $root 'trace.txt'
    [IO.File]::WriteAllText($env:LAYA_TEST_TRACE, '')
    $args = @('-NoProfile','-ExecutionPolicy','Bypass','-File',$installer,'-InstallDir',$Dir)
    if ($Repair) { $args += '-Repair' }
    $ErrorActionPreference = 'Continue'
    $output = & powershell.exe @args 2>&1
    $ErrorActionPreference = 'Stop'
    $script:resultCode = $LASTEXITCODE
    $script:resultText = $output -join "`n"
    $script:trace = [IO.File]::ReadAllText($env:LAYA_TEST_TRACE)
}
# Isolate caches and never use actual network or package installation.
$env:HF_HOME = Join-Path $root 'cache'
$dir = Fixture 'reuse'
Run $dir
Assert ($resultCode -eq 0) "Reuse failed: $resultText"
Assert (!$trace.Contains('pip install') -and $trace.Contains('laya-serve.exe')) 'Reuse must skip pip and launch server'
$env:LAYA_TEST_FAIL_PIP = '1'
Run $dir -Repair
Assert ($resultCode -ne 0) 'Failed dependency download must fail setup'
Assert (([IO.File]::ReadAllText((Join-Path $dir 'environment.txt'))) -eq 'venv') 'Failed repair changed working pointer'
Assert (Test-Path -LiteralPath (Join-Path $dir 'venv\Scripts\valid')) 'Failed repair damaged old environment'
$env:LAYA_TEST_FAIL_PIP = '0'
Run $dir -Repair
Assert ($resultCode -eq 0) "Repair failed: $resultText"
$newEnv = [IO.File]::ReadAllText((Join-Path $dir 'environment.txt'))
Assert ($newEnv -match '^env-[a-f0-9]{32}$') 'Repair must atomically publish a new environment'
Run $dir
Assert ($resultCode -eq 0 -and !$trace.Contains('pip install')) 'Validated repair must be reused'
$lock = [IO.File]::Open((Join-Path $dir 'install.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
try { Run $dir; Assert ($resultCode -ne 0 -and $resultText.Contains('[locked]')) 'Concurrent setup did not reject lock' }
finally { $lock.Dispose() }
# Test real filesystem probes without installing or filling a disk.
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($installer, [ref]$tokens, [ref]$errors)
Assert ($errors.Count -eq 0) 'Installer parse failure'
$function = $ast.Find({param($a) $a -is [Management.Automation.Language.FunctionDefinitionAst] -and $a.Name -eq 'Test-Storage'}, $true)
Invoke-Expression $function.Extent.Text
$lowSpace = $false
try { Test-Storage $root ([long]::MaxValue) } catch { $lowSpace = $_.Exception.Message.Contains('[disk]') }
Assert $lowSpace 'Insufficient disk was not detected'
$blocked = Join-Path $root 'not-a-directory'
[IO.File]::WriteAllText($blocked, 'file')
$denied = $false
try { Test-Storage $blocked 0 } catch { $denied = $true }
Assert $denied 'Unwritable destination was not rejected'
$pythonCheck = $ast.Find({param($a) $a -is [Management.Automation.Language.FunctionDefinitionAst] -and $a.Name -eq 'Test-Python'}, $true)
Invoke-Expression $pythonCheck.Extent.Text
$brokenExe = Join-Path $root 'broken.exe'
[IO.File]::WriteAllText($brokenExe, 'not an executable')
$global:LASTEXITCODE = 0
Assert (!(Test-Python $brokenExe)) 'Corrupt interpreter inherited stale successful exit code'
Write-Host 'PASS: corrupt interpreter, reuse, failed repair rollback, successful repair, repaired reuse, concurrent lock, low disk, blocked directory.'
Write-Host "Fixtures retained: $root"
