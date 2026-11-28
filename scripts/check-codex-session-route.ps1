[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)] [guid] $ThreadId,
    [string] $CodexHome = $env:CODEX_HOME,
    [string] $SqliteHome = $env:CODEX_SQLITE_HOME
)

$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($CodexHome)) {
    $CodexHome = Join-Path $HOME '.codex'
}
if ([string]::IsNullOrWhiteSpace($SqliteHome)) {
    $SqliteHome = $CodexHome
}
$CodexHome = [IO.Path]::GetFullPath($CodexHome)
$SqliteHome = [IO.Path]::GetFullPath($SqliteHome)
$id = $ThreadId.ToString('D')

function Get-ProviderValue($Value) {
    if ($Value -isnot [string] -or [string]::IsNullOrWhiteSpace($Value)) {
        return '(unset)'
    }
    return $Value.Trim()
}

function Join-UniqueValues($Values) {
    $items = @($Values | Where-Object { $_ -and $_ -ne '(no row)' } | Sort-Object -Unique)
    if ($items.Count -eq 0) { return '(none)' }
    return $items -join ', '
}

function Get-TomlAssignment {
    param(
        [string] $Content,
        [string] $Section,
        [string] $Key
    )
    if ($null -eq $Content) { return '(missing)' }
    $currentSection = ''
    $pattern = '^\s*' + [regex]::Escape($Key) + '\s*=\s*(?:"([^"]*)"|''([^'']*)''|([^#\s]+))'
    foreach ($line in ($Content -split "`r?`n")) {
        if ($line -match '^\s*\[([^\]]+)\]\s*$') {
            $currentSection = $Matches[1].Trim()
            continue
        }
        if ($currentSection -ne $Section) { continue }
        if ($line -match $pattern) {
            foreach ($index in 1..3) {
                if ($null -ne $Matches[$index] -and $Matches[$index] -ne '') {
                    return $Matches[$index]
                }
            }
        }
    }
    return '(unset)'
}

function Get-RolloutFiles {
    $escapedId = [regex]::Escape($id)
    $pattern = '^rollout-.*-' + $escapedId + '\.jsonl(?:\.zst)?$'
    $found = @()
    foreach ($directory in 'sessions', 'archived_sessions') {
        $root = Join-Path $CodexHome $directory
        if (-not (Test-Path -LiteralPath $root -PathType Container)) { continue }
        $found += @(Get-ChildItem -LiteralPath $root -File -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -imatch $pattern })
    }
    return @($found | Sort-Object FullName -Unique)
}

function Read-RolloutMetadata($Files) {
    $metadata = [System.Collections.Generic.List[string]]::new()
    $settings = [System.Collections.Generic.List[string]]::new()
    $first = '(none)'
    $unparsed = 0
    $share = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
    foreach ($rollout in @($Files)) {
        if ($null -eq $rollout) { continue }
        if ($rollout.Extension -ieq '.zst') { continue }
        $file = $null
        $reader = $null
        try {
            $file = [IO.File]::Open($rollout.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, $share)
            $reader = [IO.StreamReader]::new($file, [Text.Encoding]::UTF8)
            while ($null -ne ($line = $reader.ReadLine())) {
                if (-not ($line.Contains('session_meta') -or $line.Contains('thread_settings_applied'))) { continue }
                try { $row = ConvertFrom-Json -InputObject $line -ErrorAction Stop } catch { $unparsed++; continue }
                if ($row.type -ceq 'session_meta') {
                    $provider = Get-ProviderValue $row.payload.model_provider
                    if ($first -eq '(none)') { $first = $provider }
                    if (-not $metadata.Contains($provider)) { $metadata.Add($provider) }
                    continue
                }
                if ($row.type -ne 'event_msg' -or $row.payload.type -ne 'thread_settings_applied') { continue }
                $provider = $row.payload.thread_settings.model_provider_id
                if ($provider -isnot [string]) { continue }
                $owner = $row.payload.thread_id
                if ([string]::IsNullOrWhiteSpace([string]$owner) -or [string]$owner -eq $id) {
                    $value = Get-ProviderValue $provider
                    if (-not $settings.Contains($value)) { $settings.Add($value) }
                }
            }
        } catch {
            $unparsed++
        } finally {
            if ($null -ne $reader) { $reader.Dispose() }
            if ($null -ne $file) { $file.Dispose() }
        }
    }
    [pscustomobject]@{
        FirstMetadataProvider = $first
        MetadataProviders = Join-UniqueValues $metadata
        ResumeSettingsProvider = Join-UniqueValues $settings
        UnparsedCandidates = $unparsed
    }
}

if (-not ('SynaRouteReadOnlySessionIndex' -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class SynaRouteReadOnlySessionIndex {
    private const string Library = "winsqlite3.dll";
    private const int ReadOnly = 1;
    private const int Row = 100;
    private const int Done = 101;

    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    private static extern int sqlite3_open_v2(byte[] path, out IntPtr db, int flags, IntPtr vfs);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    private static extern int sqlite3_close(IntPtr db);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern int sqlite3_prepare16_v2(IntPtr db, string sql, int bytes, out IntPtr statement, IntPtr tail);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    private static extern int sqlite3_step(IntPtr statement);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr sqlite3_column_text16(IntPtr statement, int column);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    private static extern int sqlite3_column_int(IntPtr statement, int column);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    private static extern int sqlite3_finalize(IntPtr statement);

    private static string Quote(string value) {
        return "'" + value.Replace("'", "''") + "'";
    }

    private static string Text(IntPtr statement, int column) {
        IntPtr value = sqlite3_column_text16(statement, column);
        return value == IntPtr.Zero ? null : Marshal.PtrToStringUni(value);
    }

    private static string QueryText(IntPtr db, string sql) {
        IntPtr statement = IntPtr.Zero;
        int prepared = sqlite3_prepare16_v2(db, sql, -1, out statement, IntPtr.Zero);
        try {
            if (prepared != 0) return "(unreadable)";
            int step = sqlite3_step(statement);
            if (step == Row) return Text(statement, 0) ?? "(unset)";
            if (step == Done) return null;
            return "(unreadable)";
        } finally {
            if (statement != IntPtr.Zero) sqlite3_finalize(statement);
        }
    }

    private static int QueryInt(IntPtr db, string sql) {
        IntPtr statement = IntPtr.Zero;
        int prepared = sqlite3_prepare16_v2(db, sql, -1, out statement, IntPtr.Zero);
        try {
            if (prepared != 0) return -1;
            int step = sqlite3_step(statement);
            if (step == Row) return sqlite3_column_int(statement, 0);
            if (step == Done) return 0;
            return -1;
        } finally {
            if (statement != IntPtr.Zero) sqlite3_finalize(statement);
        }
    }

    private static IntPtr Open(string path, out int result) {
        IntPtr db;
        result = sqlite3_open_v2(Encoding.UTF8.GetBytes(path + "\0"), out db, ReadOnly, IntPtr.Zero);
        return db;
    }

    private static string Host(IntPtr db) {
        int hasHosts = QueryInt(db, "SELECT 1 FROM sqlite_master WHERE type='table' AND name='local_thread_catalog_hosts' LIMIT 1");
        if (hasHosts != 1) return "local";
        int hasKind = QueryInt(db, "SELECT 1 FROM pragma_table_info('local_thread_catalog_hosts') WHERE name='host_kind' LIMIT 1");
        string sql = hasKind == 1
            ? "SELECT host_id FROM local_thread_catalog_hosts WHERE LOWER(COALESCE(host_kind, ''))='local' ORDER BY host_id LIMIT 1"
            : "SELECT host_id FROM local_thread_catalog_hosts WHERE host_id='local' LIMIT 1";
        return QueryText(db, sql);
    }

    public static string ThreadsProvider(string path, Guid thread) {
        int opened;
        IntPtr db = Open(path, out opened);
        try {
            if (opened != 0) return "(unreadable)";
            if (QueryInt(db, "SELECT 1 FROM sqlite_master WHERE type='table' AND name='threads' LIMIT 1") != 1) return null;
            if (QueryInt(db, "SELECT 1 FROM pragma_table_info('threads') WHERE name='model_provider' LIMIT 1") != 1) return "(no model_provider column)";
            return QueryText(db, "SELECT model_provider FROM threads WHERE id=" + Quote(thread.ToString("D")) + " LIMIT 1") ?? "(no row)";
        } finally {
            if (db != IntPtr.Zero) sqlite3_close(db);
        }
    }

    public static string LocalCatalogHost(string path) {
        int opened;
        IntPtr db = Open(path, out opened);
        try {
            if (opened != 0) return "(unreadable)";
            if (QueryInt(db, "SELECT 1 FROM sqlite_master WHERE type='table' AND name='local_thread_catalog' LIMIT 1") != 1) return null;
            return Host(db) ?? "(no local host)";
        } finally {
            if (db != IntPtr.Zero) sqlite3_close(db);
        }
    }

    public static string LocalCatalogProvider(string path, Guid thread) {
        int opened;
        IntPtr db = Open(path, out opened);
        try {
            if (opened != 0) return "(unreadable)";
            if (QueryInt(db, "SELECT 1 FROM sqlite_master WHERE type='table' AND name='local_thread_catalog' LIMIT 1") != 1) return null;
            if (QueryInt(db, "SELECT 1 FROM pragma_table_info('local_thread_catalog') WHERE name='model_provider' LIMIT 1") != 1) return "(no model_provider column)";
            int hasHost = QueryInt(db, "SELECT 1 FROM pragma_table_info('local_thread_catalog') WHERE name='host_id' LIMIT 1");
            string sql = hasHost == 1
                ? "SELECT model_provider FROM local_thread_catalog WHERE host_id=" + Quote(Host(db) ?? "") + " AND thread_id=" + Quote(thread.ToString("D")) + " LIMIT 1"
                : "SELECT model_provider FROM local_thread_catalog WHERE thread_id=" + Quote(thread.ToString("D")) + " LIMIT 1";
            return QueryText(db, sql) ?? "(no row)";
        } finally {
            if (db != IntPtr.Zero) sqlite3_close(db);
        }
    }
}
"@
}

$databases = @()
if (Test-Path -LiteralPath $SqliteHome -PathType Container) {
    $databases += @(Get-ChildItem -LiteralPath $SqliteHome -Filter 'state_*.sqlite' -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^state_\d+\.sqlite$' })
    $modern = Join-Path $SqliteHome 'sqlite'
    if (Test-Path -LiteralPath $modern -PathType Container) {
        $databases += @(Get-ChildItem -LiteralPath $modern -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Extension -in '.db', '.sqlite', '.sqlite3' })
    }
}
$databases = @($databases | Sort-Object FullName -Unique)

$databaseDiagnostics = @($databases | ForEach-Object {
    [pscustomobject]@{
        Database = $_.FullName
        ThreadsProvider = [SynaRouteReadOnlySessionIndex]::ThreadsProvider($_.FullName, $ThreadId)
        LocalCatalogHost = [SynaRouteReadOnlySessionIndex]::LocalCatalogHost($_.FullName)
        LocalCatalogProvider = [SynaRouteReadOnlySessionIndex]::LocalCatalogProvider($_.FullName, $ThreadId)
    }
})

$rollouts = Get-RolloutFiles
$rolloutMetadata = Read-RolloutMetadata $rollouts
$configPath = Join-Path $CodexHome 'config.toml'
$configText = if (Test-Path -LiteralPath $configPath -PathType Leaf) {
    Get-Content -LiteralPath $configPath -Raw
} else {
    $null
}
$threadsProviders = @($databaseDiagnostics.ThreadsProvider | Where-Object { $_ })
$catalogProviders = @($databaseDiagnostics.LocalCatalogProvider | Where-Object { $_ })

$version = '(unavailable)'
$codex = Get-Command codex -ErrorAction SilentlyContinue
if ($null -ne $codex) {
    try { $version = ((& $codex.Source --version 2>$null | Select-Object -First 1).ToString()).Trim() } catch { }
}

[pscustomobject]@{
    CodexVersion = $version
    CodexHome = $CodexHome
    SqliteHome = $SqliteHome
    ConfigPath = $configPath
    ConfigModelProvider = Get-TomlAssignment $configText '' 'model_provider'
    RemoteCompactionV2 = Get-TomlAssignment $configText 'features' 'remote_compaction_v2'
    RolloutFiles = @($rollouts.FullName)
    CompressedRolloutFiles = @($rollouts | Where-Object { $_.Extension -ieq '.zst' } | Select-Object -ExpandProperty FullName)
    FirstMetadataProvider = $rolloutMetadata.FirstMetadataProvider
    MetadataProviders = $rolloutMetadata.MetadataProviders
    ResumeSettingsProvider = $rolloutMetadata.ResumeSettingsProvider
    UnparsedCandidates = $rolloutMetadata.UnparsedCandidates
    ThreadIndexProviders = Join-UniqueValues $threadsProviders
    LocalCatalogProviders = Join-UniqueValues $catalogProviders
    DatabaseDiagnostics = $databaseDiagnostics
}
