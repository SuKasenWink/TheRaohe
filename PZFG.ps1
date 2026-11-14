# Package zip following gitignore
$ErrorActionPreference = "Stop"

function Get-EscapedClassChar {
    param([char]$Char)
    switch ($Char) {
        '\' { return '\\' }
        '[' { return '\x5B' }
        ']' { return '\x5D' }
        '^' { return '\x5E' }
        '-' { return '\x2D' }
        default { return [string]$Char }
    }
}

function ConvertTo-GitignoreRegex {
    param([string]$Pattern)
    $sb = [System.Text.StringBuilder]::new("^")
    $i = 0
    $n = $Pattern.Length
    while ($i -lt $n) {
        $c = $Pattern[$i]
        if ($c -eq '*') {
            $run = 0
            while ($i -lt $n -and $Pattern[$i] -eq '*') { $run++; $i++ }
            if ($run -ge 2) {
                $next = if ($i -lt $n) { $Pattern[$i] } else { $null }
                if ($next -eq '/') {
                    [void]$sb.Append('(?:[^/]+/)*')
                    $i++
                } elseif ($null -eq $next) {
                    [void]$sb.Append('.*')
                } else {
                    [void]$sb.Append('[^/]*')
                }
            } else {
                [void]$sb.Append('[^/]*')
            }
            continue
        }
        if ($c -eq '?') {
            [void]$sb.Append('[^/]')
            $i++
            continue
        }
        if ($c -eq '[') {
            $j = $i + 1
            $negate = $false
            if ($j -lt $n -and ($Pattern[$j] -eq '!' -or $Pattern[$j] -eq '^')) { $negate = $true; $j++ }
            $body = [System.Text.StringBuilder]::new()
            $first = $true
            $closed = $false
            while ($j -lt $n) {
                $cj = $Pattern[$j]
                if ($cj -eq ']' -and -not $first) { $closed = $true; break }
                $first = $false
                if ($cj -eq '\' -and $j + 1 -lt $n) {
                    $body.Append((Get-EscapedClassChar $Pattern[$j + 1]))
                    $j += 2
                    continue
                }
                $body.Append((Get-EscapedClassChar $cj))
                $j++
            }
            if (-not $closed) {
                [void]$sb.Append('\[')
                $i++
                continue
            }
            if ($body.Length -eq 0) {
                [void]$sb.Append('\[\]')
                $i = $j + 1
                continue
            }
            $cls = '[' + $(if ($negate) { '^' } else { '' }) + $body.ToString() + ']'
            [void]$sb.Append($cls)
            $i = $j + 1
            continue
        }
        if ($c -eq '\') {
            if ($i + 1 -lt $n) {
                [void]$sb.Append([regex]::Escape([string]$Pattern[$i + 1]))
                $i += 2
            } else {
                [void]$sb.Append('\\')
                $i++
            }
            continue
        }
        [void]$sb.Append([regex]::Escape([string]$c))
        $i++
    }
    [void]$sb.Append('$')
    return $sb.ToString()
}

function Get-GitignoreRules {
    param([string]$GitignorePath)
    $rules = New-Object System.Collections.Generic.List[object]
    foreach ($raw in [System.IO.File]::ReadAllLines($GitignorePath)) {
        $line = $raw
        if ($line.EndsWith('\ ')) {
            $line = $line.Substring(0, $line.Length - 2) + ' '
        } else {
            $line = $line.TrimEnd()
        }
        if ($line.Length -eq 0) { continue }
        if ($line[0] -eq '#') { continue }
        $negate = $false
        if ($line[0] -eq '!') { $negate = $true; $line = $line.Substring(1) }
        $dirOnly = $line.EndsWith('/')
        while ($line.EndsWith('/')) { $line = $line.Substring(0, $line.Length - 1) }
        if ($line.Length -eq 0) { continue }
        $anchored = $false
        if ($line[0] -eq '/') { $anchored = $true; $line = $line.Substring(1) }
        if ($line.Contains('/')) { $anchored = $true }
        $regexText = ConvertTo-GitignoreRegex -Pattern $line
        $rules.Add([pscustomobject]@{
            Negate   = $negate
            DirOnly  = $dirOnly
            Anchored = $anchored
            Regex    = [System.Text.RegularExpressions.Regex]::new($regexText, [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)
        })
    }
    return $rules.ToArray()
}

function Test-Ignored {
    param(
        [object[]]$Rules,
        [string]$Rel,
        [bool]$IsDir
    )
    $ignored = $false
    $base = $Rel.Substring($Rel.LastIndexOf('/') + 1)
    foreach ($rule in $Rules) {
        if ($rule.DirOnly -and -not $IsDir) { continue }
        $target = if ($rule.Anchored) { $Rel } else { $base }
        if ($rule.Regex.IsMatch($target)) { $ignored = -not $rule.Negate }
    }
    return $ignored
}

$root = (Get-Location).Path.TrimEnd('\', '/')
$leaf = Split-Path $root -Leaf
if (-not $leaf) { $leaf = "package" }
$outFile = Join-Path $root "$leaf.zip"
$gitignore = Join-Path $root ".gitignore"

$rules = @()
if (Test-Path -LiteralPath $gitignore -PathType Leaf) {
    $rules = @(Get-GitignoreRules -GitignorePath $gitignore)
    Write-Output "gitignore: $gitignore ($($rules.Count) rules)"
} else {
    Write-Warning ".gitignore not found at $gitignore; packaging without exclusions"
}

$entries = New-Object System.Collections.Generic.List[object]
$stack = [System.Collections.Generic.Stack[string]]::new()
$stack.Push($root)
while ($stack.Count -gt 0) {
    $dir = $stack.Pop()
    foreach ($child in Get-ChildItem -LiteralPath $dir -Force) {
        $full = $child.FullName
        $rel = $full.Substring($root.Length).TrimStart('\', '/') -replace '\\', '/'
        $isDir = $child.PSIsContainer
        $isReparse = ($child.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0
        if ($isReparse) {
            Write-Warning "skipped link: $rel"
            continue
        }
        if (-not $isDir -and [string]::Equals($full, $outFile, [StringComparison]::OrdinalIgnoreCase)) { continue }
        if ($isDir -and $child.Name -eq '.git') { continue }
        if (Test-Ignored -Rules $rules -Rel $rel -IsDir $isDir) { continue }
        if ($isDir) {
            $stack.Push($full)
        } else {
            $entries.Add([pscustomobject]@{ Full = $full; Rel = $rel })
        }
    }
}

$sorted = @($entries | Sort-Object Rel)

Add-Type -AssemblyName System.IO.Compression.FileSystem
Add-Type -AssemblyName System.IO.Compression

$totalBytes = 0L
$fs = $null
$zip = $null
try {
    $fs = [System.IO.File]::Open($outFile, [System.IO.FileMode]::Create)
    $zip = [System.IO.Compression.ZipArchive]::new($fs, [System.IO.Compression.ZipArchiveMode]::Create)
    foreach ($e in $sorted) {
        $source = $null
        $target = $null
        try {
            $entry = $zip.CreateEntry($e.Rel, [System.IO.Compression.CompressionLevel]::Optimal)
            $entry.LastWriteTime = (Get-Item -LiteralPath $e.Full).LastWriteTime
            $target = $entry.Open()
            $source = [System.IO.File]::OpenRead($e.Full)
            $totalBytes += $source.Length
            $source.CopyTo($target)
        } finally {
            if ($source) { $source.Dispose() }
            if ($target) { $target.Dispose() }
        }
    }
} catch {
    if ($zip) { $zip.Dispose(); $zip = $null }
    if ($fs) { $fs.Dispose(); $fs = $null }
    Remove-Item -LiteralPath $outFile -Force -ErrorAction SilentlyContinue
    throw "PZFG failed: $($_.Exception.Message)"
} finally {
    if ($zip) { $zip.Dispose() }
    if ($fs) { $fs.Dispose() }
}

Write-Output "files:  $($sorted.Count)"
Write-Output "bytes:  $totalBytes"
Write-Output "zip:    $outFile"
Write-Output "size:   $((Get-Item -LiteralPath $outFile).Length) bytes"
