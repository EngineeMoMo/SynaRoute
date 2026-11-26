[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)] [guid] $ThreadId,
    [string] $CodexHome = $env:CODEX_HOME,
    [string] $SqliteHome = $env:CODEX_SQLITE_HOME
)

$ErrorActionPreference = 'Stop'
if (-not $CodexHome) { $CodexHome = Join-Path $HOME '.codex' }
$id = $ThreadId.ToString()
$files = @(foreach ($directory in 'sessions', 'archived_sessions') {
    $root = Join-Path $CodexHome $directory
    if (Test-Path -LiteralPath $root -PathType Container) {
        Get-ChildItem -LiteralPath $root -Filter "rollout-*-$id.jsonl" -File -Recurse
    }
})
if ($files.Count -ne 1) { throw "Expected one rollout for this ID; found $($files.Count). Check the thread ID and -CodexHome." }

function Get-ProviderLabel($Value) {
    if ($Value -isnot [string] -or -not $Value) { return '(unset)' }
    if ($Value -ceq 'openai' -or $Value -ceq 'synaroute') { return $Value }
    return '(other)'
}

$metadata = [System.Collections.Generic.List[string]]::new()
$first = '(none)'
$owned = '(none)'
$latest = '(none)'
$unparsed = 0
$share = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
$file = [IO.File]::Open($files[0].FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, $share)
try {
    $reader = [IO.StreamReader]::new($file, [Text.Encoding]::UTF8)
    try {
        while ($null -ne ($line = $reader.ReadLine())) {
            if (-not ($line.Contains('session_meta') -or $line.Contains('thread_settings_applied') -or $line.Contains('\u'))) { continue }
            try { $row = ConvertFrom-Json -InputObject $line -ErrorAction Stop } catch { $unparsed++; continue }
            if ($row.type -ceq 'session_meta') {
                $label = Get-ProviderLabel $row.payload.model_provider
                if ($first -eq '(none)') { $first = $label }
                if (-not $metadata.Contains($label)) { $metadata.Add($label) }
            } elseif ($row.type -ceq 'event_msg' -and $row.payload.type -ceq 'thread_settings_applied' -and
                $row.payload.thread_settings.model_provider_id -is [string]) {
                $owner = $row.payload.thread_id
                $label = Get-ProviderLabel $row.payload.thread_settings.model_provider_id
                if ($null -eq $owner) { $latest = $label }
                elseif ($owner -is [string] -and $owner -eq $id) { $owned = $label; $latest = $label }
                elseif ($owner -is [string]) { $latest = $owned }
            }
        }
    } finally { $reader.Dispose() }
} finally { $file.Dispose() }

function Get-IndexProviders {
    if (-not $SqliteHome) { $SqliteHome = $CodexHome }
    $databases = @(Get-ChildItem -LiteralPath $SqliteHome -Filter 'state_*.sqlite' -File | Where-Object Name -Match '^state_\d+\.sqlite$'
        $modern = Join-Path $SqliteHome 'sqlite'
        if (Test-Path -LiteralPath $modern -PathType Container) {
            Get-ChildItem -LiteralPath $modern -File | Where-Object Extension -In '.db','.sqlite','.sqlite3'
        }
    )
    if (-not $databases) { return '(no database)' }
    if (-not ('SynaRouteReadOnlySessionIndex' -as [type])) {
        Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class SynaRouteReadOnlySessionIndex {
    private const string Library = "winsqlite3.dll";
    [DllImport(Library, CallingConvention=CallingConvention.Cdecl)] static extern int sqlite3_open_v2(byte[] path, out IntPtr db, int flags, IntPtr vfs);
    [DllImport(Library, CallingConvention=CallingConvention.Cdecl)] static extern int sqlite3_close(IntPtr db);
    [DllImport(Library, CallingConvention=CallingConvention.Cdecl, CharSet=CharSet.Unicode, ExactSpelling=true)] static extern int sqlite3_prepare16_v2(IntPtr db, string sql, int bytes, out IntPtr statement, IntPtr tail);
    [DllImport(Library, CallingConvention=CallingConvention.Cdecl)] static extern int sqlite3_step(IntPtr statement);
    [DllImport(Library, CallingConvention=CallingConvention.Cdecl)] static extern int sqlite3_column_int(IntPtr statement, int column);
    [DllImport(Library, CallingConvention=CallingConvention.Cdecl)] static extern int sqlite3_finalize(IntPtr statement);

    static int Scalar(IntPtr db, string sql) {
        IntPtr statement;
        int prepared = sqlite3_prepare16_v2(db, sql, -1, out statement, IntPtr.Zero);
        try {
            if (prepared != 0) return -1;
            int step = sqlite3_step(statement);
            return step == 100 ? sqlite3_column_int(statement, 0) : step == 101 ? 0 : -1;
        } finally { if (statement != IntPtr.Zero) sqlite3_finalize(statement); }
    }

    public static string Provider(string path, Guid thread) {
        IntPtr db;
        int opened = sqlite3_open_v2(Encoding.UTF8.GetBytes(path + "\0"), out db, 1, IntPtr.Zero);
        try {
            if (opened != 0) return "(unreadable)";
            int schema = Scalar(db, "SELECT 1 FROM pragma_table_info('threads') WHERE name='model_provider' LIMIT 1");
            if (schema != 1) return schema == 0 ? null : "(unreadable)";
            int label = Scalar(db, "SELECT CASE WHEN model_provider='openai' THEN 1 WHEN model_provider='synaroute' THEN 2 WHEN COALESCE(model_provider,'')='' THEN 3 ELSE 4 END FROM threads WHERE id='" + thread.ToString("D") + "' LIMIT 1");
            switch (label) {
                case 0: return null; case 1: return "openai"; case 2: return "synaroute";
                case 3: return "(unset)"; case 4: return "(other)"; default: return "(unreadable)";
            }
        } finally { if (db != IntPtr.Zero) sqlite3_close(db); }
    }
}
"@
    }
    $labels = @($databases | ForEach-Object { [SynaRouteReadOnlySessionIndex]::Provider($_.FullName, $ThreadId) } |
        Where-Object { $_ } | Sort-Object -Unique)
    if (-not $labels) { return '(no row)' }
    return $labels -join ', '
}

$index = try { Get-IndexProviders } catch { '(unavailable)' }
[pscustomobject]@{
    FirstMetadataProvider = $first
    MetadataProviders = $metadata -join ', '
    ResumeSettingsProvider = $latest
    ThreadIndexProviders = $index
    UnparsedCandidates = $unparsed
}
