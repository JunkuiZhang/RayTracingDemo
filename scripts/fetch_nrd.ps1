[CmdletBinding()]
param(
    [string]$ExternalRoot = (Join-Path $PSScriptRoot '..\external'),
    [switch]$VerifyOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$lockPath = Join-Path $PSScriptRoot '..\third_party\nrd\version.lock.json'
$lock = Get-Content -LiteralPath $lockPath -Raw | ConvertFrom-Json
$root = [System.IO.Path]::GetFullPath($ExternalRoot)
$nrdRoot = Join-Path $root 'nrd-v4.17.3'
$dependencyRoot = Join-Path $nrdRoot '_deps'
$d3d12maRoot = Join-Path $root $lock.d3d12_memory_allocator.destination

function Invoke-Git {
    param([string[]]$Arguments, [string]$WorkingDirectory)
    $output = & git -C $WorkingDirectory @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "git $($Arguments -join ' ') failed in $WorkingDirectory`n$output"
    }
    return ($output -join "`n")
}

function Get-GitHead {
    param([string]$Path)
    return (Invoke-Git @('rev-parse', 'HEAD') $Path).Trim()
}

function Ensure-Repository {
    param(
        [string]$Repository,
        [string]$Ref,
        [string]$ExpectedCommit,
        [string]$Destination,
        [string]$Label,
        [switch]$IsTag
    )

    if (Test-Path -LiteralPath $Destination) {
        if (-not (Test-Path -LiteralPath (Join-Path $Destination '.git'))) {
            throw "$Label 目标已存在但不是 git 仓库，拒绝覆盖：$Destination"
        }
        $head = Get-GitHead $Destination
        if ($head -ne $ExpectedCommit) {
            throw "$Label 版本不匹配：期望 $ExpectedCommit，实际 $head；拒绝覆盖 $Destination"
        }
        return
    }
    if ($VerifyOnly) {
        throw "$Label 缺少已验证的本地源：$Destination"
    }

    $parent = Split-Path -Parent $Destination
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
    if ($IsTag) {
        & git clone --branch $Ref --depth 1 --no-tags $Repository $Destination
    } else {
        & git clone --filter=blob:none --no-checkout --no-tags $Repository $Destination
        if ($LASTEXITCODE -ne 0) { throw "$Label clone failed" }
        Invoke-Git @('fetch', '--depth', '1', 'origin', $Ref) $Destination | Out-Null
        Invoke-Git @('checkout', '--detach', $Ref) $Destination | Out-Null
    }
    if ($LASTEXITCODE -ne 0) { throw "$Label checkout failed" }
    $head = Get-GitHead $Destination
    if ($head -ne $ExpectedCommit) {
        throw "$Label checkout 结果不匹配：期望 $ExpectedCommit，实际 $head"
    }
}

Ensure-Repository $lock.nrd.repository $lock.nrd.tag $lock.nrd.commit $nrdRoot 'NRD v4.17.3' -IsTag
Ensure-Repository $lock.nri.repository $lock.nri.tag $lock.nri.commit (Join-Path $dependencyRoot 'NRI') 'NRI v179' -IsTag
Ensure-Repository $lock.mathlib.repository $lock.mathlib.tag $lock.mathlib.commit (Join-Path $dependencyRoot 'MathLib') 'MathLib v11' -IsTag
Ensure-Repository $lock.shadermake.repository $lock.shadermake.commit $lock.shadermake.commit (Join-Path $dependencyRoot 'ShaderMake') 'ShaderMake'
Ensure-Repository $lock.d3d12_memory_allocator.repository $lock.d3d12_memory_allocator.commit $lock.d3d12_memory_allocator.commit $d3d12maRoot 'D3D12MemoryAllocator'

$actualNrd = Get-GitHead $nrdRoot
if (-not $actualNrd.StartsWith($lock.nrd.commit_prefix)) {
    throw "NRD HEAD 不满足官方发布提交前缀：$actualNrd"
}

if ($lock.license_hashes.status -ne 'verified') {
    throw 'version.lock.json 的 license_hashes 不是 verified；禁止猜测许可证 hash。'
}
foreach ($entry in $lock.license_hashes.files.psobject.Properties) {
    $licensePath = Join-Path $nrdRoot ($entry.Name -replace '/', '\\')
    if (-not (Test-Path -LiteralPath $licensePath)) {
        throw "许可证文件缺失：$licensePath"
    }
    $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $licensePath).Hash.ToLowerInvariant()
    if ($actualHash -ne $entry.Value.ToLowerInvariant()) {
        throw "许可证 hash 不匹配：$($entry.Name)，期望 $($entry.Value)，实际 $actualHash"
    }
}
foreach ($entry in $lock.license_hashes.external_files.psobject.Properties) {
    $licensePath = Join-Path $root ($entry.Name -replace '/', '\\')
    if (-not (Test-Path -LiteralPath $licensePath)) {
        throw "许可证文件缺失：$licensePath"
    }
    $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $licensePath).Hash.ToLowerInvariant()
    if ($actualHash -ne $entry.Value.ToLowerInvariant()) {
        throw "许可证 hash 不匹配：$($entry.Name)，期望 $($entry.Value)，实际 $actualHash"
    }
}

Write-Output "NRD sources verified: $actualNrd"
Write-Output "NRI: $(Get-GitHead (Join-Path $dependencyRoot 'NRI'))"
Write-Output "MathLib: $(Get-GitHead (Join-Path $dependencyRoot 'MathLib'))"
Write-Output "ShaderMake: $(Get-GitHead (Join-Path $dependencyRoot 'ShaderMake'))"
Write-Output "D3D12MemoryAllocator: $(Get-GitHead $d3d12maRoot)"
Write-Output 'FetchContent must use local source directories and FETCHCONTENT_FULLY_DISCONNECTED=ON.'
