# Generate a WiX v4 .wxs fragment that lists every file under the publish
# directory as a Component. Replaces heat.exe — heat fails with HEAT5151
# on .NET 8 self-contained satellite resource DLLs ("Operation is not
# supported on this platform" trying to load them as assemblies on a
# different ARCH context). Hand-walking is deterministic + arch-agnostic.
#
# Output: PublishedFiles.wxs in the project obj/ folder.

param(
    [Parameter(Mandatory)] [string] $PublishRoot,
    [Parameter(Mandatory)] [string] $OutputFile,
    [string] $ComponentGroupName = "PublishedFiles",
    [string] $DirectoryRefId = "INSTALLFOLDER"
)

if (-not (Test-Path $PublishRoot)) {
    Write-Error "PublishRoot not found: $PublishRoot"
    exit 1
}

$publishRootResolved = (Resolve-Path $PublishRoot).Path
$files = Get-ChildItem -Path $publishRootResolved -Recurse -File | Sort-Object FullName

$sb = New-Object System.Text.StringBuilder
[void]$sb.AppendLine('<?xml version="1.0" encoding="UTF-8"?>')
[void]$sb.AppendLine('<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs">')
[void]$sb.AppendLine('  <Fragment>')

# Build a directory tree. Components must live inside a Directory chain
# rooted at INSTALLFOLDER (or another DirectoryRef the main .wxs declares).
# We walk subdirs once + emit a nested Directory tree.
$dirs = @{}    # relative-path -> Directory Id
$dirs[""] = $DirectoryRefId

# Build the dir set by visiting parent of every file.
$relDirSet = New-Object 'System.Collections.Generic.HashSet[string]'
foreach ($f in $files) {
    $rel = $f.FullName.Substring($publishRootResolved.Length).TrimStart('\','/')
    $relDir = Split-Path $rel -Parent
    if ($relDir) {
        # Add this dir + every ancestor.
        $cur = $relDir
        while ($cur) {
            [void]$relDirSet.Add($cur.Replace('/','\'))
            $cur = Split-Path $cur -Parent
        }
    }
}

# Stable order so the Component Ids stay reproducible across builds.
$sortedDirs = $relDirSet | Sort-Object

# Emit the nested DirectoryRef tree.
[void]$sb.AppendLine("    <DirectoryRef Id=`"$DirectoryRefId`">")

# Walk the sorted dirs and emit the nested <Directory> tree inline.
# Stack tracks currently-open dirs; every time we open a new dir we
# record its WiX Id in $dirs so the Component emit step can reference it.
$stack = New-Object 'System.Collections.Generic.Stack[string]'
$indent = "      "
foreach ($d in $sortedDirs) {
    while ($stack.Count -gt 0 -and -not $d.StartsWith($stack.Peek() + '\') -and $d -ne $stack.Peek()) {
        $stack.Pop() | Out-Null
        $indent = $indent.Substring(2)
        [void]$sb.AppendLine("$indent</Directory>")
    }
    $name = Split-Path $d -Leaf
    $clean = ($d -replace '[^A-Za-z0-9_]', '_')
    $id = "dir_$clean"
    $dirs[$d] = $id
    [void]$sb.AppendLine("$indent<Directory Id=`"$id`" Name=`"$name`">")
    $stack.Push($d) | Out-Null
    $indent += "  "
}
while ($stack.Count -gt 0) {
    $stack.Pop() | Out-Null
    $indent = $indent.Substring(2)
    [void]$sb.AppendLine("$indent</Directory>")
}

[void]$sb.AppendLine("    </DirectoryRef>")

# Now emit the ComponentGroup with one Component per file.
[void]$sb.AppendLine("    <ComponentGroup Id=`"$ComponentGroupName`">")

$compCounter = 0
foreach ($f in $files) {
    $rel = $f.FullName.Substring($publishRootResolved.Length).TrimStart('\','/')
    $relDir = Split-Path $rel -Parent
    $relDir = if ($relDir) { $relDir.Replace('/','\') } else { "" }
    $dirId = if ($relDir -eq "") { $DirectoryRefId } else { $dirs[$relDir] }

    # Stable Component Id based on the relative path. WiX needs Ids to be
    # stable across builds for upgrade behavior to work right.
    $hash = [System.Security.Cryptography.SHA1]::Create().ComputeHash(
        [System.Text.Encoding]::UTF8.GetBytes($rel.ToLowerInvariant())
    )
    $hashHex = [System.BitConverter]::ToString($hash).Replace('-','').Substring(0, 16)
    $compId = "cmp_$hashHex"
    $fileId = "fil_$hashHex"

    # Stable component GUID per relative path. Required for proper upgrade
    # behavior — same path → same GUID across versions so the installer
    # knows it's the same file slot.
    $guidHash = [System.Security.Cryptography.MD5]::Create().ComputeHash(
        [System.Text.Encoding]::UTF8.GetBytes($rel.ToLowerInvariant())
    )
    $guidHex = [System.BitConverter]::ToString($guidHash).Replace('-','')
    $componentGuid = "{0}-{1}-{2}-{3}-{4}" -f `
        $guidHex.Substring(0, 8),
        $guidHex.Substring(8, 4),
        $guidHex.Substring(12, 4),
        $guidHex.Substring(16, 4),
        $guidHex.Substring(20, 12)

    [void]$sb.AppendLine("      <Component Id=`"$compId`" Directory=`"$dirId`" Guid=`"$componentGuid`">")
    $sourcePath = '$(var.PublishRoot)\' + $rel
    [void]$sb.AppendLine("        <File Id=`"$fileId`" Source=`"$sourcePath`" KeyPath=`"yes`" />")
    [void]$sb.AppendLine("      </Component>")
    $compCounter++
}

[void]$sb.AppendLine("    </ComponentGroup>")
[void]$sb.AppendLine("  </Fragment>")
[void]$sb.AppendLine("</Wix>")

$outDir = Split-Path $OutputFile -Parent
if ($outDir -and -not (Test-Path $outDir)) {
    New-Item -ItemType Directory -Path $outDir -Force | Out-Null
}
[System.IO.File]::WriteAllText($OutputFile, $sb.ToString(), [System.Text.UTF8Encoding]::new($false))

Write-Host "Generated $compCounter components from $publishRootResolved -> $OutputFile"
