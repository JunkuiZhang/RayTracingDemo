[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$lockPath = Join-Path $root 'third_party\streamline\version.lock.json'
$lock = Get-Content -LiteralPath $lockPath -Raw | ConvertFrom-Json
$target = Join-Path $root 'external\streamline-v2.14.1'

function Assert-RelativePath([string]$path) {
    if ([string]::IsNullOrWhiteSpace($path) -or [IO.Path]::IsPathRooted($path) -or $path.Replace('\','/') -match '(^|/)\.\.(/|$)') {
        throw "lock 中包含不安全路径：$path"
    }
}

function Get-ExpectedEntries {
    @($lock.files) + @($lock.licenses) | ForEach-Object {
        Assert-RelativePath $_.path
        $_
    }
}

function Test-VerifiedTree([string]$directory) {
    if (-not (Test-Path -LiteralPath $directory -PathType Container)) { return $false }
    foreach ($entry in Get-ExpectedEntries) {
        $path = Join-Path $directory ($entry.path -replace '/','\')
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $false }
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
        if ($actual -ne $entry.sha256.ToLowerInvariant()) { return $false }
    }
    foreach ($entry in @($lock.files | Where-Object { $_.path -match '^bin/x64/[^/]+\.dll$' })) {
        $path = Join-Path $directory ($entry.path -replace '/','\')
        $signature = Get-AuthenticodeSignature -LiteralPath $path
        if ($signature.Status -ne 'Valid' -or
            $null -eq $signature.SignerCertificate -or
            $signature.SignerCertificate.Subject -notmatch 'O=NVIDIA Corporation') {
            return $false
        }
    }
    return $true
}

function Assert-ReleaseUrl {
    $uri = [Uri]$lock.asset_url
    if ($uri.Scheme -ne 'https' -or $uri.Host -ne 'github.com' -or
        $uri.AbsolutePath -notmatch '/NVIDIA-RTX/Streamline/releases/download/v2\.14\.1/') {
        throw "asset_url 必须是官方 GitHub v2.14.1 release URL：$($lock.asset_url)"
    }
    if ([IO.Path]::GetFileName($uri.AbsolutePath) -ne $lock.asset_name) {
        throw "asset_name 与 asset_url 不一致"
    }
}

Assert-ReleaseUrl
if (Test-VerifiedTree $target) {
    Write-Output "Streamline SDK verified: $target"
    exit 0
}
if (Test-Path -LiteralPath $target) {
    throw "已有 Streamline SDK 目录但 hash 不匹配；拒绝覆盖：$target"
}

$temp = Join-Path ([IO.Path]::GetTempPath()) ("raytracingdemo-streamline-" + [Guid]::NewGuid().ToString('N'))
$archive = Join-Path $temp $lock.asset_name
$extract = Join-Path $temp 'extract'
New-Item -ItemType Directory -Force -Path $temp,$extract | Out-Null
try {
    Invoke-WebRequest -Uri $lock.asset_url -OutFile $archive -UseBasicParsing
    $archiveHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
    if ($archiveHash -ne $lock.archive_sha256.ToLowerInvariant()) {
        throw "Streamline release archive hash 不匹配：$archiveHash"
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName.Replace('\','/')
            if ([IO.Path]::IsPathRooted($name) -or $name -match '(^|/)\.\.(/|$)') {
                throw "ZIP 包含目录逃逸路径：$name"
            }
        }
    } finally { $zip.Dispose() }
    Expand-Archive -LiteralPath $archive -DestinationPath $extract

    $candidate = $extract
    $children = @(Get-ChildItem -LiteralPath $extract -Force)
    if ($children.Count -eq 1 -and $children[0].PSIsContainer -and
        (Test-Path (Join-Path $children[0].FullName 'include')) -and
        (Test-Path (Join-Path $children[0].FullName 'bin'))) {
        $candidate = $children[0].FullName
    }
    if (-not (Test-VerifiedTree $candidate)) {
        throw '解包后的 Streamline SDK 缺少必需文件或 hash 不匹配'
    }
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $target) | Out-Null
    Move-Item -LiteralPath $candidate -Destination $target
    Write-Output "Streamline SDK fetched and verified: $target"
} finally {
    if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp -Recurse -Force }
}
