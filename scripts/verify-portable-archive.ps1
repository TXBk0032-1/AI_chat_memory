[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ArchivePath,
    [Parameter(Mandatory)][string]$ExpectedEntryName
)

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.IO.Compression.FileSystem

$resolvedArchivePath = (Resolve-Path -LiteralPath $ArchivePath).Path
$archive = $null
try {
    $archive = [IO.Compression.ZipFile]::OpenRead($resolvedArchivePath)
    if ($archive.Entries.Count -ne 1) {
        throw "Portable archive must contain exactly one entry; found $($archive.Entries.Count)"
    }

    $entry = $archive.Entries[0]
    if (-not [string]::Equals($entry.FullName, $ExpectedEntryName, [StringComparison]::Ordinal)) {
        throw "Portable archive entry must be '$ExpectedEntryName'; found '$($entry.FullName)'"
    }
    if ($entry.FullName.EndsWith('/') -or $entry.FullName.EndsWith('\')) {
        throw "Portable archive entry must be a file: $($entry.FullName)"
    }
    if ($entry.Length -le 0) {
        throw "Portable archive entry must not be empty: $($entry.FullName)"
    }

    [pscustomobject]@{
        entry_name = $entry.FullName
        entry_bytes = $entry.Length
    }
} finally {
    if ($null -ne $archive) {
        $archive.Dispose()
    }
}
