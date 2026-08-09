[CmdletBinding()]
param(
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Release",
    [string]$Exe,
    [ValidateSet("Smoke", "Matrix", "DebugValidation")]
    [string]$Suite = "Smoke",
    [string]$NrdExe,
    [ValidateRange(1, 3)]
    [int]$Runs = 1,
    [ValidateRange(1, 30)]
    [int]$Seconds = 1,
    [ValidateRange(10, 180)]
    [int]$TimeoutSeconds = 60,
    [string]$OutputRoot = "output/stage10"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Quote-Argument([string]$Value) {
    if ($Value -notmatch '[\s"]') { return $Value }
    return '"' + $Value.Replace('"', '\"') + '"'
}

function Get-EnvironmentSnapshot {
    $gpu = $null
    $gpuError = $null
    try {
        $gpu = Get-CimInstance Win32_VideoController -ErrorAction Stop |
            Where-Object { $_.Name -match 'NVIDIA' } |
            Select-Object -First 1
    } catch {
        $gpuError = $_.Exception.Message
    }
    $driver = "N/A"
    $driverSource = "unavailable"
    try {
        if ($null -ne $gpu -and -not [string]::IsNullOrWhiteSpace([string]$gpu.DriverVersion)) {
            $driver = [string]$gpu.DriverVersion
            $driverSource = "Win32_VideoController.DriverVersion"
        } else {
            $smi = @(& nvidia-smi --query-gpu=driver_version --format=csv,noheader,nounits 2>&1 |
                ForEach-Object { [string]$_ } |
                Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
            if ($smi.Count -gt 0 -and $smi[0] -notmatch 'not recognized|failed|error') {
                $driver = $smi[0].Trim()
                $driverSource = "nvidia-smi"
            }
        }
    } catch { }

    $powerScheme = "N/A"
    try {
        $power = @(& powercfg /getactivescheme 2>&1 | ForEach-Object { [string]$_ })
        if ($power.Count -gt 0) { $powerScheme = ($power -join " ").Trim() }
    } catch { }

    $powerSource = "N/A"
    try {
        if (-not ("Stage10Acceptance.Native.PowerStatus" -as [type])) {
            Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
namespace Stage10Acceptance.Native {
    [StructLayout(LayoutKind.Sequential)]
    public struct SYSTEM_POWER_STATUS {
        public byte ACLineStatus;
        public byte BatteryFlag;
        public byte BatteryLifePercent;
        public byte Reserved;
        public int BatteryLifeTime;
        public int BatteryFullLifeTime;
    }
    public static class PowerStatus {
        [DllImport("kernel32.dll")]
        public static extern bool GetSystemPowerStatus(out SYSTEM_POWER_STATUS status);
    }
}
"@
        }
        $status = New-Object Stage10Acceptance.Native.SYSTEM_POWER_STATUS
        if ([Stage10Acceptance.Native.PowerStatus]::GetSystemPowerStatus([ref]$status)) {
            if ($status.ACLineStatus -eq 1) { $powerSource = "AC" }
            elseif ($status.ACLineStatus -eq 0) { $powerSource = "battery" }
        }
    } catch { }

    [ordered]@{
        gpu_name = if ($null -ne $gpu) { [string]$gpu.Name } else { "N/A" }
        gpu_name_source = if ($null -ne $gpu) { "Win32_VideoController.Name" } else { "unavailable" }
        driver_version = $driver
        driver_version_source = $driverSource
        active_power_scheme = $powerScheme
        active_power_scheme_source = "powercfg /getactivescheme"
        power_source = $powerSource
        power_source_probe = "GetSystemPowerStatus"
        gpu_probe_error = $gpuError
        os = [Environment]::OSVersion.VersionString
        powershell = $PSVersionTable.PSVersion.ToString()
        machine = [Environment]::MachineName
    }
}

function Invoke-RecordedProcess(
    [string]$Executable,
    [string[]]$Arguments,
    [string]$CaseRoot,
    [string]$Label
) {
    $stdoutPath = Join-Path $CaseRoot "$Label.stdout.txt"
    $stderrPath = Join-Path $CaseRoot "$Label.stderr.txt"
    $argsPath = Join-Path $CaseRoot "$Label.args.txt"
    $exitPath = Join-Path $CaseRoot "$Label.exit.json"
    $command = "$(Quote-Argument $Executable) " + (($Arguments | ForEach-Object { Quote-Argument $_ }) -join ' ')
    Set-Content -LiteralPath $argsPath -Value $command -Encoding UTF8
    $started = Get-Date
    $exitCode = -1
    $timedOut = $false
    $launchError = $null
    try {
        $process = Start-Process -FilePath $Executable -ArgumentList ($Arguments -join ' ') `
            -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath -PassThru -WindowStyle Hidden
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $timedOut = $true
            try { $process.Kill($true) } catch { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
            $process.WaitForExit(10000) | Out-Null
        } else {
            $process.Refresh()
            if ($process.HasExited) { $exitCode = [int]$process.ExitCode }
        }
    } catch {
        $launchError = $_.Exception.Message
        Set-Content -LiteralPath $stderrPath -Value $launchError -Encoding UTF8
    }
    $elapsed = ((Get-Date) - $started).TotalSeconds
    $stdout = if (Test-Path -LiteralPath $stdoutPath) { Get-Content -LiteralPath $stdoutPath -Raw } else { "" }
    $lines = @($stdout -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $json = $null
    $jsonError = $launchError
    if ($timedOut) { $jsonError = "timeout after $TimeoutSeconds seconds" }
    $jsonCandidates = @()
    foreach ($line in $lines) {
        try { $jsonCandidates += ,($line | ConvertFrom-Json) } catch { }
    }
    if ($jsonCandidates.Count -eq 1) {
        $json = $jsonCandidates[0]
    } elseif ($null -eq $jsonError) {
        $jsonError = if ($jsonCandidates.Count -eq 0) {
            "stdout has no parseable JSON line"
        } else {
            "stdout has $($jsonCandidates.Count) parseable JSON lines; expected exactly one"
        }
    }
    [ordered]@{
        label = $Label
        command = $command
        args_path = $argsPath
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        exit_path = $exitPath
        exit_code = $exitCode
        timed_out = $timedOut
        elapsed_seconds = $elapsed
        stdout_nonempty_lines = $lines.Count
        parseable_json_lines = $jsonCandidates.Count
        json = $json
        json_error = $jsonError
    } | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $exitPath -Encoding UTF8
    [pscustomobject]@{
        label = $Label
        command = $command
        args_path = $argsPath
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        exit_path = $exitPath
        exit_code = $exitCode
        timed_out = $timedOut
        elapsed_seconds = $elapsed
        stdout_nonempty_lines = $lines.Count
        parseable_json_lines = $jsonCandidates.Count
        json = $json
        json_error = $jsonError
    }
}

function Get-Cases([string]$SelectedSuite) {
    $cases = @(
        @{ label = "native_svgf"; upscaler = "native"; denoiser = "svgf" },
        @{ label = "dlaa_svgf"; upscaler = "dlaa"; denoiser = "svgf" },
        @{ label = "quality_svgf"; upscaler = "dlss-quality"; denoiser = "svgf" },
        @{ label = "balanced_svgf"; upscaler = "dlss-balanced"; denoiser = "svgf" },
        @{ label = "performance_svgf"; upscaler = "dlss-performance"; denoiser = "svgf" }
    )
    if ($SelectedSuite -eq "Smoke") { return @($cases[0]) }
    if ($SelectedSuite -eq "DebugValidation") { return @($cases[0], $cases[2]) }
    return $cases
}

$repoRoot = (Get-Location).Path
$executable = if ([string]::IsNullOrWhiteSpace($Exe)) {
    Join-Path $repoRoot "target\$($Configuration.ToLower())\ray_tracing_demo.exe"
} else {
    (Resolve-Path -LiteralPath $Exe).Path
}
if (-not (Test-Path -LiteralPath $executable)) { throw "executable not found: $executable" }
$nrdExecutable = $null
if (-not [string]::IsNullOrWhiteSpace($NrdExe)) {
    $nrdExecutable = (Resolve-Path -LiteralPath $NrdExe).Path
    if (-not (Test-Path -LiteralPath $nrdExecutable)) { throw "NRD executable not found: $nrdExecutable" }
}

$runId = "$(Get-Date -Format yyyyMMdd-HHmmss)-$([guid]::NewGuid().ToString('N').Substring(0,8))"
$runRoot = Join-Path $OutputRoot $runId
New-Item -ItemType Directory -Path $runRoot -Force | Out-Null
$environment = Get-EnvironmentSnapshot
$environment | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $runRoot "environment.json") -Encoding UTF8

$cases = Get-Cases $Suite
if ($Suite -eq "Matrix" -and $null -ne $nrdExecutable) {
    $cases += @{ label = "quality_nrd"; upscaler = "dlss-quality"; denoiser = "nrd-reblur"; executable = $nrdExecutable }
}
$caseResults = [System.Collections.Generic.List[object]]::new()
foreach ($case in $cases) {
    $caseRoot = Join-Path $runRoot $case.label
    New-Item -ItemType Directory -Path $caseRoot -Force | Out-Null
    for ($run = 1; $run -le $Runs; $run++) {
        $label = "$($case.label)-$run"
        $caseExecutable = if ($case.ContainsKey("executable")) { $case.executable } else { $executable }
        $arguments = @(
            "--benchmark-seconds", [string]$Seconds,
            "--output-size", "1920x1080",
            "--denoiser", $case.denoiser,
            "--upscaler", $case.upscaler
        )
        $result = Invoke-RecordedProcess $caseExecutable $arguments $caseRoot $label
        $caseResults.Add($result)
    }
}

$failures = @($caseResults | Where-Object {
    $_.exit_code -ne 0 -or $_.timed_out -or $_.parseable_json_lines -ne 1 -or $null -eq $_.json
})
$summary = [ordered]@{
    schema_version = 1
    stage = "10H"
    run_id = $runId
    configuration = $Configuration
    suite = $Suite
    seconds = $Seconds
    runs = $Runs
    timeout_seconds = $TimeoutSeconds
    executable = $executable
    executable_sha256 = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash
    nrd_executable = $nrdExecutable
    nrd_executable_sha256 = if ($null -ne $nrdExecutable) {
        (Get-FileHash -LiteralPath $nrdExecutable -Algorithm SHA256).Hash
    } else { $null }
    git_head = (& git rev-parse HEAD).Trim()
    environment = $environment
    cases = $caseResults
    pass = ($failures.Count -eq 0)
    failures = @($failures | ForEach-Object { "$($_.label): $($_.json_error)" })
    note = "No long-duration workload is implemented; this runner is bounded to 1..30 seconds."
}
$summaryPath = Join-Path $runRoot "summary.json"
$summary | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath $summaryPath -Encoding UTF8
Write-Output $summaryPath
if ($failures.Count -gt 0) { exit 1 }
