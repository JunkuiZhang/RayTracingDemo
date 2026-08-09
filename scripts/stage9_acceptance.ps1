[CmdletBinding()]
param(
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Release",
    [string]$Exe,
    [ValidateSet("Smoke", "Matrix", "DebugValidation")]
    [string]$Suite = "Smoke",
    [ValidateRange(1, 3)]
    [int]$Runs = 1,
    [ValidateRange(1, 30)]
    [int]$Seconds = 3,
    [ValidateRange(10, 300)]
    [int]$TimeoutSeconds = 60,
    [string]$FixedOutputSize = "1280x720",
    [string]$DynamicOutputSize = "1920x1080",
    [string]$OutputRoot = "output/stage9"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Quote-Argument([string]$Value) {
    if ($Value -notmatch '[\s"]') { return $Value }
    return '"' + $Value.Replace('"', '\"') + '"'
}

function Test-Finite($Value) {
    if ($null -eq $Value) { return $false }
    $number = [double]$Value
    return -not [double]::IsNaN($number) -and -not [double]::IsInfinity($number)
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
        $powerOutput = @(& powercfg /getactivescheme 2>&1 | ForEach-Object { [string]$_ })
        if ($powerOutput.Count -gt 0) {
            $powerScheme = ($powerOutput -join " ").Trim()
        }
    } catch { }

    $powerSource = "N/A"
    $powerProbe = "unavailable"
    try {
        if (-not ("Stage9Acceptance.Native.PowerStatus" -as [type])) {
            Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
namespace Stage9Acceptance.Native {
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
        $status = New-Object Stage9Acceptance.Native.SYSTEM_POWER_STATUS
        if ([Stage9Acceptance.Native.PowerStatus]::GetSystemPowerStatus([ref]$status)) {
            $powerProbe = "GetSystemPowerStatus"
            if ($status.ACLineStatus -eq 1) { $powerSource = "AC" }
            elseif ($status.ACLineStatus -eq 0) { $powerSource = "battery" }
        }
    } catch { }

    [ordered]@{
        gpu_name = if ($null -ne $gpu) { [string]$gpu.Name } else { "N/A" }
        gpu_name_source = if ($null -ne $gpu) { "Win32_VideoController.Name" } else { "benchmark_json_pending" }
        driver_version = $driver
        driver_version_source = $driverSource
        active_power_scheme = $powerScheme
        active_power_scheme_source = "powercfg /getactivescheme"
        power_source = $powerSource
        power_source_probe = $powerProbe
        gpu_probe_error = $gpuError
        os = [Environment]::OSVersion.VersionString
        powershell = $PSVersionTable.PSVersion.ToString()
        machine = [Environment]::MachineName
    }
}

function Get-Median([object[]]$Values) {
    $numbers = @($Values | Where-Object { $null -ne $_ } | ForEach-Object { [double]$_ } | Sort-Object)
    if ($numbers.Count -eq 0) { return $null }
    $index = [int][Math]::Floor($numbers.Count / 2.0)
    if (($numbers.Count % 2) -eq 1) { return $numbers[$index] }
    return ($numbers[$index - 1] + $numbers[$index]) / 2.0
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
    $exitPath = Join-Path $CaseRoot "$Label.exit.txt"
    $argumentString = ($Arguments | ForEach-Object { Quote-Argument $_ }) -join ' '
    $command = "$(Quote-Argument $Executable) $argumentString"
    Set-Content -LiteralPath $argsPath -Value $command -Encoding UTF8
    $started = Get-Date
    $exitCode = -1
    $timedOut = $false
    $launchError = $null
    try {
        $process = Start-Process -FilePath $Executable -ArgumentList $argumentString `
            -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath `
            -PassThru -WindowStyle Hidden
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $timedOut = $true
            try { $process.Kill($true) } catch { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
            $process.WaitForExit(10000) | Out-Null
        } else {
            $process.Refresh()
            $exitCode = if ($process.HasExited) { [int]$process.ExitCode } else { -1 }
        }
    } catch {
        $launchError = $_.Exception.Message
        Set-Content -LiteralPath $stderrPath -Value $launchError -Encoding UTF8
    }
    $elapsed = ((Get-Date) - $started).TotalSeconds
    $stdout = if (Test-Path -LiteralPath $stdoutPath) { Get-Content -LiteralPath $stdoutPath -Raw } else { "" }
    $stderr = if (Test-Path -LiteralPath $stderrPath) { Get-Content -LiteralPath $stderrPath -Raw } else { "" }
    $lines = @($stdout -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $json = $null
    $jsonError = $launchError
    if ($timedOut) { $jsonError = "timeout after $TimeoutSeconds seconds" }
    if ($lines.Count -gt 0) {
        try { $json = $lines[-1] | ConvertFrom-Json }
        catch { if ($null -eq $jsonError) { $jsonError = $_.Exception.Message } }
    } elseif ($null -eq $jsonError) {
        $jsonError = "stdout has no JSON line"
    }
    Set-Content -LiteralPath $exitPath -Value ([ordered]@{
        exit_code = $exitCode
        timed_out = $timedOut
        elapsed_seconds = $elapsed
        json_error = $jsonError
    } | ConvertTo-Json -Compress) -Encoding UTF8
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
        json = $json
        json_error = $jsonError
        stderr_nonempty = -not [string]::IsNullOrWhiteSpace($stderr)
    }
}

function Test-Run($Run, [string]$Backend, [string]$Mode, [bool]$RequireSwitch, [bool]$EnforcePerformance) {
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($Run.exit_code -ne 0) { $failures.Add("exit_code=$($Run.exit_code)") }
    if ($Run.timed_out) { $failures.Add("timed_out") }
    if ($Run.stdout_nonempty_lines -ne 1) { $failures.Add("stdout must contain exactly one JSON line") }
    if ($null -eq $Run.json) { $failures.Add("invalid JSON: $($Run.json_error)") }
    $stderr = if (Test-Path -LiteralPath $Run.stderr_path) {
        Get-Content -LiteralPath $Run.stderr_path -Raw
    } else { "" }
    $stderr = [regex]::Replace($stderr, "\x1B\[[0-?]*[ -/]*[@-~]", "")
    $debugErrors = @(
        [regex]::Matches($stderr, '(?im)D3D12 Debug .*severity=(ERROR|CORRUPTION)') |
            ForEach-Object { $_.Value }
    )
    if ($debugErrors.Count -gt 0) {
        $failures.Add("D3D12 InfoQueue contains $($debugErrors.Count) ERROR/CORRUPTION message(s)")
    }
    if ($null -eq $Run.json) { return [pscustomobject]@{ passed = $false; failures = $failures.ToArray() } }
    $json = $Run.json
    if ([string]$json.denoiser.active -ne $Backend) { $failures.Add("active denoiser mismatch") }
    if ([string]$json.resolution_mode -ne $Mode) { $failures.Add("resolution mode mismatch") }
    if (-not (Test-Finite $json.valid_samples) -or [int64]$json.valid_samples -le 0) { $failures.Add("valid_samples invalid") }
    $activePasses = if ($Backend -eq "nrd-reblur") {
        @("total", "acceleration_structure", "path_trace", "nrd_prep", "nrd_denoise", "nrd_compose", "tone_map")
    } else {
        @("total", "acceleration_structure", "path_trace", "temporal", "atrous", "atrous_0", "atrous_1", "atrous_2", "atrous_3", "tone_map")
    }
    foreach ($name in $activePasses) {
        $pass = $json.passes.$name
        if ($null -eq $pass -or -not (Test-Finite $pass.p50_ms) -or -not (Test-Finite $pass.p95_ms)) {
            $failures.Add("active pass $name is not finite")
        } elseif ([int64]$pass.valid_samples -ne [int64]$json.valid_samples) {
            $failures.Add("active pass $name sample count differs")
        }
    }
    $inactivePasses = if ($Backend -eq "nrd-reblur") {
        @("temporal", "atrous", "atrous_0", "atrous_1", "atrous_2", "atrous_3")
    } else { @("nrd_prep", "nrd_denoise", "nrd_compose") }
    foreach ($name in $inactivePasses) {
        if ($null -ne $json.passes.$name.p50_ms -or $null -ne $json.passes.$name.p95_ms) {
            $failures.Add("inactive pass $name must be null")
        }
    }
    if ($EnforcePerformance -and [double]$json.passes.total.p95_ms -gt 16.67) {
        $failures.Add("Total p95 exceeds 16.67 ms")
    }
    if ([int64]$json.gpu_idle_wait_count -ne 0) { $failures.Add("gpu_idle_wait_count is nonzero") }
    if ($null -eq $json.memory.measurement -or [int64]$json.memory.measurement.valid_query_count -le 0) {
        $failures.Add("memory measurement is unavailable")
    } elseif ($EnforcePerformance -and (-not (Test-Finite $json.memory.measurement.peak_usage_ratio) -or [double]$json.memory.measurement.peak_usage_ratio -ge 0.70)) {
        $failures.Add("memory peak/budget is invalid or >= 70%")
    }
    if ($Mode -eq "fixed" -and [int64]$json.render_generation_switch_count -ne 0) {
        $failures.Add("fixed case unexpectedly switched generations")
    }
    if ($RequireSwitch) {
        $dynamic = $json.dynamic_resolution.measurement
        if ($null -eq $dynamic -or ([int64]$dynamic.downscale_count + [int64]$dynamic.upscale_count) -le 0) {
            $failures.Add("dynamic case did not switch")
        } elseif ([int64]$json.render_generation_switch_count -ne ([int64]$dynamic.downscale_count + [int64]$dynamic.upscale_count)) {
            $failures.Add("dynamic switch/generation count mismatch")
        } elseif ([int64]$json.render_generation_retired_count -ne [int64]$json.render_generation_switch_count -or [int64]$json.retired_generation_count -ne 0) {
            $failures.Add("dynamic generation retirement mismatch")
        }
    }
    [pscustomobject]@{ passed = $failures.Count -eq 0; failures = $failures.ToArray() }
}

function Add-Case(
    [string]$Name,
    [string[]]$Arguments,
    [string]$Backend,
    [string]$Mode,
    [bool]$RequireSwitch,
    [int]$Count,
    [System.Collections.Generic.List[object]]$Cases,
    [string]$Root,
    [string]$Executable,
    [bool]$EnforcePerformance
) {
    $caseRoot = Join-Path $Root $Name
    New-Item -ItemType Directory -Path $caseRoot -Force | Out-Null
    $runs = [System.Collections.Generic.List[object]]::new()
    for ($index = 1; $index -le $Count; $index++) {
        $run = Invoke-RecordedProcess $Executable $Arguments $caseRoot ("run-{0:D2}" -f $index)
        $run | Add-Member -NotePropertyName validation -NotePropertyValue (Test-Run $run $Backend $Mode $RequireSwitch $EnforcePerformance)
        $runs.Add($run)
    }
    $valid = @($runs | Where-Object { $_.validation.passed })
    $totalP95 = @( $valid | ForEach-Object { $_.json.passes.total.p95_ms } )
    $summary = [ordered]@{
        name = $Name
        backend = $Backend
        mode = $Mode
        run_count = $runs.Count
        valid_run_count = $valid.Count
        passed = $runs.Count -gt 0 -and $valid.Count -eq $runs.Count
        median_total_p95_ms = Get-Median $totalP95
        runs = $runs.ToArray()
    }
    $Cases.Add([pscustomobject]$summary)
}

$repoRoot = (Get-Location).Path
$configurationName = $Configuration.ToLowerInvariant()
if ([string]::IsNullOrWhiteSpace($Exe)) {
    $Exe = Join-Path $repoRoot "target/$configurationName/ray_tracing_demo.exe"
}
$Executable = [IO.Path]::GetFullPath($Exe)
if (-not (Test-Path -LiteralPath $Executable)) { throw "executable not found: $Executable" }

$runId = "{0}-{1}" -f (Get-Date -Format "yyyyMMdd-HHmmss"), ([guid]::NewGuid().ToString("N").Substring(0, 8))
$runRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot (Join-Path $OutputRoot $runId)))
New-Item -ItemType Directory -Path $runRoot -Force | Out-Null
$startedAt = (Get-Date).ToString("o")
$environment = Get-EnvironmentSnapshot
$environment.exe_sha256 = (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash
$environment.exe_path = $Executable
$environment.git_head = (& git rev-parse HEAD).Trim()
$environment.git_status = @(& git status --short)
$environment_json_path = Join-Path $runRoot "environment.json"
$environment | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $environment_json_path -Encoding UTF8

$common = @("--command-recording-mode", "optimized", "--atrous-mode", "baseline", "--acceleration-structure-mode", "baseline")
$cases = [System.Collections.Generic.List[object]]::new()
$enforcePerformance = $Configuration -eq "Release"
if ($Suite -eq "Smoke") {
    Add-Case "svgf-fixed" (@("--benchmark-seconds", "$Seconds", "--output-size", $FixedOutputSize, "--denoiser", "svgf") + $common) "svgf" "fixed" $false 1 $cases $runRoot $Executable $enforcePerformance
    Add-Case "nrd-fixed" (@("--benchmark-seconds", "$Seconds", "--output-size", $FixedOutputSize, "--denoiser", "nrd-reblur") + $common) "nrd-reblur" "fixed" $false 1 $cases $runRoot $Executable $enforcePerformance
    Add-Case "nrd-dynamic" (@("--benchmark-seconds", "$Seconds", "--output-size", $DynamicOutputSize, "--dynamic-resolution", "--target-gpu-ms", "4.0", "--denoiser", "nrd-reblur") + $common) "nrd-reblur" "dynamic" $true 1 $cases $runRoot $Executable $enforcePerformance
} elseif ($Suite -eq "Matrix") {
    Add-Case "svgf-1280x720-fixed" (@("--benchmark-seconds", "$Seconds", "--output-size", "1280x720", "--denoiser", "svgf") + $common) "svgf" "fixed" $false $Runs $cases $runRoot $Executable $enforcePerformance
    Add-Case "nrd-1280x720-fixed" (@("--benchmark-seconds", "$Seconds", "--output-size", "1280x720", "--denoiser", "nrd-reblur") + $common) "nrd-reblur" "fixed" $false $Runs $cases $runRoot $Executable $enforcePerformance
    Add-Case "svgf-1920x1080-fixed" (@("--benchmark-seconds", "$Seconds", "--output-size", "1920x1080", "--denoiser", "svgf") + $common) "svgf" "fixed" $false $Runs $cases $runRoot $Executable $enforcePerformance
    Add-Case "nrd-1920x1080-fixed" (@("--benchmark-seconds", "$Seconds", "--output-size", "1920x1080", "--denoiser", "nrd-reblur") + $common) "nrd-reblur" "fixed" $false $Runs $cases $runRoot $Executable $enforcePerformance
    Add-Case "nrd-1920x1080-dynamic" (@("--benchmark-seconds", "$Seconds", "--output-size", "1920x1080", "--dynamic-resolution", "--target-gpu-ms", "4.0", "--denoiser", "nrd-reblur") + $common) "nrd-reblur" "dynamic" $true $Runs $cases $runRoot $Executable $enforcePerformance
    $fixture = [IO.Path]::GetFullPath((Join-Path $repoRoot "assets/gltf/Triangle/NonIndexedMultiNode.gltf"))
    if (Test-Path -LiteralPath $fixture) {
        Add-Case "nrd-animated-gltf-1280x720" (@("--benchmark-seconds", "$Seconds", "--output-size", "1280x720", "--denoiser", "nrd-reblur", "--model", $fixture, "--animate-model") + $common) "nrd-reblur" "fixed" $false $Runs $cases $runRoot $Executable $enforcePerformance
    } else {
        $cases.Add([pscustomobject]@{ name = "nrd-animated-gltf-1280x720"; passed = $false; blocked = $true; reason = "fixture missing: $fixture" })
    }
} else {
    Add-Case "svgf-static" (@("--benchmark-seconds", "$Seconds", "--output-size", "1280x720", "--denoiser", "svgf") + $common) "svgf" "fixed" $false 1 $cases $runRoot $Executable $false
    Add-Case "nrd-static" (@("--benchmark-seconds", "$Seconds", "--output-size", "1280x720", "--denoiser", "nrd-reblur") + $common) "nrd-reblur" "fixed" $false 1 $cases $runRoot $Executable $false
    $fixture = [IO.Path]::GetFullPath((Join-Path $repoRoot "assets/gltf/Triangle/NonIndexedMultiNode.gltf"))
    if (Test-Path -LiteralPath $fixture) {
        Add-Case "nrd-animated-gltf" (@("--benchmark-seconds", "$Seconds", "--output-size", "1280x720", "--denoiser", "nrd-reblur", "--model", $fixture, "--animate-model") + $common) "nrd-reblur" "fixed" $false 1 $cases $runRoot $Executable $false
    } else {
        $cases.Add([pscustomobject]@{ name = "nrd-animated-gltf"; passed = $false; blocked = $true; reason = "fixture missing: $fixture" })
    }
    Add-Case "nrd-dynamic" (@("--benchmark-seconds", "$Seconds", "--output-size", "1920x1080", "--dynamic-resolution", "--target-gpu-ms", "4.0", "--denoiser", "nrd-reblur") + $common) "nrd-reblur" "dynamic" $true 1 $cases $runRoot $Executable $false
    Add-Case "nrd-validation" (@("--benchmark-seconds", "$Seconds", "--output-size", "1280x720", "--denoiser", "nrd-reblur", "--debug-view", "nrd-validation") + $common) "nrd-reblur" "fixed" $false 1 $cases $runRoot $Executable $false
}

$passed = @($cases | Where-Object { $_.passed }).Count -eq $cases.Count
$benchmarkGpu = @(
    $cases | ForEach-Object { $_.runs } | ForEach-Object { $_.json } |
        Where-Object { $null -ne $_ -and -not [string]::IsNullOrWhiteSpace([string]$_.gpu_name) } |
        Select-Object -First 1
)
if (($environment.gpu_name -eq "N/A" -or [string]::IsNullOrWhiteSpace([string]$environment.gpu_name)) -and $benchmarkGpu.Count -gt 0) {
    $environment.gpu_name = [string]$benchmarkGpu[0].gpu_name
    $environment.gpu_name_source = "benchmark_json.gpu_name"
}
$environment | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $environment_json_path -Encoding UTF8
$summary = [ordered]@{
    schema_version = 1
    stage = "9F"
    suite = $Suite
    run_id = $runId
    started_at = $startedAt
    configuration = $Configuration
    executable = $Executable
    timeout_seconds = $TimeoutSeconds
    seconds_per_process = $Seconds
    runs_per_case = if ($Suite -eq "Matrix") { $Runs } else { 1 }
    environment = $environment
    cases = $cases.ToArray()
    passed = $passed
    prohibited_long_run_seconds = @(600, 1800)
    finished_at = (Get-Date).ToString("o")
}
$summaryPath = Join-Path $runRoot "summary.json"
$summary | ConvertTo-Json -Depth 60 | Set-Content -LiteralPath $summaryPath -Encoding UTF8
$summary | ConvertTo-Json -Depth 60
if (-not $passed) { exit 1 }
