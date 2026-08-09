[CmdletBinding()]
param(
    [Alias("Build")]
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Release",
    [string]$Exe,
    [string]$OutputRoot = "output/stage8g",
    [ValidateRange(1, 10)]
    [int]$Runs = 3,
    [ValidateRange(1, 3600)]
    [int]$Seconds = 30,
    [ValidateRange(1, 3600)]
    [int]$LongRunSeconds = 1800,
    [ValidateSet("Full", "Recheck1080")]
    [string]$Suite = "Full",
    [string]$LargeModel,
    [string]$PbrModel,
    [switch]$SkipLongRun,
    [switch]$SkipCaptures,
    [switch]$SmokeOnly,
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Quote-Argument([string]$Value) {
    if ($Value -notmatch '[\s"]') { return $Value }
    return '"' + $Value.Replace('"', '\"') + '"'
}

function Get-Median([object[]]$Values) {
    $numbers = @($Values | Where-Object { $null -ne $_ } | ForEach-Object { [double]$_ } | Sort-Object)
    if ($numbers.Count -eq 0) { return $null }
    $middle = [int][Math]::Floor($numbers.Count / 2.0)
    if (($numbers.Count % 2) -eq 1) { return $numbers[$middle] }
    return ($numbers[$middle - 1] + $numbers[$middle]) / 2.0
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

    $driverVersion = "N/A"
    $driverSource = "unavailable"
    $driverError = $null
    if ($null -ne $gpu -and -not [string]::IsNullOrWhiteSpace([string]$gpu.DriverVersion)) {
        $driverVersion = [string]$gpu.DriverVersion
        $driverSource = "Win32_VideoController"
    } else {
        try {
            $smi = @(& nvidia-smi --query-gpu=driver_version --format=csv,noheader,nounits 2>&1 |
                ForEach-Object { [string]$_ } |
                Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
            if ($smi.Count -gt 0 -and $smi[0] -notmatch 'not recognized|failed|error') {
                $driverVersion = $smi[0].Trim()
                $driverSource = "nvidia-smi"
            } else {
                $driverError = ($smi -join " ").Trim()
            }
        } catch {
            $driverError = $_.Exception.Message
        }
    }

    $power = "N/A"
    $powerSource = "unavailable"
    $powerError = $null
    try {
        $powerOutput = @(& powercfg /getactivescheme 2>&1 | ForEach-Object { [string]$_ })
        $power = ($powerOutput -join " ").Trim()
        if ([string]::IsNullOrWhiteSpace($power)) {
            $powerError = "powercfg returned no output"
        } else {
            $powerSource = "powercfg /getactivescheme"
        }
    } catch {
        $powerError = $_.Exception.Message
    }

    $powerSourceValue = "N/A"
    $powerSourceProbe = "unavailable"
    $powerSourceError = $null
    try {
        if (-not ("Stage8Validation.Native.PowerStatus" -as [type])) {
            Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
namespace Stage8Validation.Native {
    [StructLayout(LayoutKind.Sequential)]
    public struct SystemPowerStatus {
        public byte ACLineStatus;
        public byte BatteryFlag;
        public byte BatteryLifePercent;
        public byte Reserved;
        public int BatteryLifeTime;
        public int BatteryFullLifeTime;
    }
    public static class PowerStatus {
        [DllImport("kernel32.dll")]
        public static extern bool GetSystemPowerStatus(out SystemPowerStatus status);
    }
}
"@
        }
        $status = New-Object Stage8Validation.Native.SystemPowerStatus
        if ([Stage8Validation.Native.PowerStatus]::GetSystemPowerStatus([ref]$status)) {
            $powerSourceProbe = "GetSystemPowerStatus"
            if ($status.ACLineStatus -eq 1) {
                $powerSourceValue = "AC"
            } elseif ($status.ACLineStatus -eq 0) {
                $powerSourceValue = "battery"
            } else {
                $powerSourceValue = "N/A"
            }
        } else {
            $powerSourceError = "GetSystemPowerStatus returned false"
        }
    } catch {
        $powerSourceError = $_.Exception.Message
    }

    if ($powerSourceValue -eq "N/A") {
        try {
            $battery = Get-CimInstance Win32_Battery -ErrorAction Stop | Select-Object -First 1
            if ($null -ne $battery) {
                $powerSourceProbe = "Win32_Battery.BatteryStatus"
                if ([int]$battery.BatteryStatus -eq 2) {
                    $powerSourceValue = "AC"
                } elseif ([int]$battery.BatteryStatus -in @(1, 3, 4, 5)) {
                    $powerSourceValue = "battery"
                }
            } else {
                $powerSourceError = "Win32_Battery returned no instance"
            }
        } catch {
            $powerSourceError = $_.Exception.Message
        }
    }

    [pscustomobject]@{
        gpu_name = if ($null -ne $gpu) { [string]$gpu.Name } else { "N/A" }
        gpu_name_source = if ($null -ne $gpu) { "Win32_VideoController" } else { "benchmark_json_pending" }
        driver_version = $driverVersion
        driver_version_source = $driverSource
        driver_version_error = $driverError
        active_power_scheme = if ([string]::IsNullOrWhiteSpace($power)) { "N/A" } else { $power }
        active_power_scheme_source = $powerSource
        active_power_scheme_error = $powerError
        power_source = $powerSourceValue
        power_source_probe = $powerSourceProbe
        power_source_error = $powerSourceError
        gpu_probe_error = $gpuError
    }
}

function Invoke-MedianSelfTest {
    $cases = @(
        [pscustomobject]@{ name = "empty"; values = @(); expected = $null },
        [pscustomobject]@{ name = "single"; values = @(3); expected = 3.0 },
        [pscustomobject]@{ name = "odd"; values = @(3, 1, 2); expected = 2.0 },
        [pscustomobject]@{ name = "even"; values = @(4, 1, 3, 2); expected = 2.5 },
        [pscustomobject]@{ name = "seven"; values = @(7, 1, 6, 2, 5, 3, 4); expected = 4.0 }
    )
    foreach ($case in $cases) {
        $actual = Get-Median $case.values
        $matches = if ($null -eq $case.expected) { $null -eq $actual } else {
            $null -ne $actual -and [double]$actual -eq [double]$case.expected
        }
        if (-not $matches) {
            throw "median self-test failed: $($case.name), expected=$($case.expected), actual=$actual"
        }
    }
    [ordered]@{ self_test = "passed"; cases = $cases.Count } | ConvertTo-Json -Compress
}

function Invoke-RecordedProcess(
    [string]$FilePath,
    [string[]]$Arguments,
    [string]$CaseDirectory,
    [string]$Label
) {
    $stdoutPath = Join-Path $CaseDirectory "$Label.stdout.txt"
    $stderrPath = Join-Path $CaseDirectory "$Label.stderr.txt"
    $argumentString = ($Arguments | ForEach-Object { Quote-Argument $_ }) -join ' '
    $commandLine = "$(Quote-Argument $FilePath) $argumentString"
    $timeoutSeconds = 300
    $benchmarkSecondsIndex = [Array]::IndexOf($Arguments, "--benchmark-seconds")
    if ($benchmarkSecondsIndex -ge 0 -and $benchmarkSecondsIndex + 1 -lt $Arguments.Count) {
        $benchmarkDuration = [int]$Arguments[$benchmarkSecondsIndex + 1]
        $timeoutSeconds = $benchmarkDuration + $(if ($Configuration -eq "Debug") { 180 } else { 90 })
    } elseif ([IO.Path]::GetFileNameWithoutExtension($FilePath) -eq "image_diff") {
        $timeoutSeconds = 60
    }
    $started = Get-Date
    $exitCode = -1
    $timedOut = $false
    $launchError = $null
    try {
        $process = Start-Process -FilePath $FilePath -ArgumentList $argumentString `
            -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath `
            -PassThru -WindowStyle Hidden
        if (-not $process.WaitForExit($timeoutSeconds * 1000)) {
            $timedOut = $true
            try { $process.Kill($true) } catch { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
            $process.WaitForExit(10000) | Out-Null
        } else {
            $process.WaitForExit()
            $exitCode = $process.ExitCode
        }
    } catch {
        $launchError = $_.Exception.Message
        Set-Content -LiteralPath $stderrPath -Value $launchError -Encoding UTF8
    }
    $finished = Get-Date
    $stdout = if (Test-Path -LiteralPath $stdoutPath) { Get-Content -LiteralPath $stdoutPath -Raw } else { "" }
    $stderr = if (Test-Path -LiteralPath $stderrPath) { Get-Content -LiteralPath $stderrPath -Raw } else { "" }
    $lines = @($stdout -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $json = $null
    $jsonError = if ($timedOut) { "process timed out after $timeoutSeconds seconds" } else { $launchError }
    if ($lines.Count -gt 0) {
        try { $json = $lines[-1] | ConvertFrom-Json -Depth 40 } catch { if ($null -eq $jsonError) { $jsonError = $_.Exception.Message } }
    } elseif ($null -eq $jsonError) {
        $jsonError = "stdout has no non-empty JSON line"
    }
    [pscustomobject]@{
        label = $Label
        command = $commandLine
        args = $Arguments
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        exit_code = $exitCode
        timed_out = $timedOut
        timeout_seconds = $timeoutSeconds
        elapsed_seconds = ($finished - $started).TotalSeconds
        stdout_nonempty_lines = $lines.Count
        json = $json
        json_error = $jsonError
        stderr_nonempty = -not [string]::IsNullOrWhiteSpace($stderr)
    }
}

function Test-BenchmarkResult($Run, [hashtable]$Expectations = @{}) {
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($Run.exit_code -ne 0) { $failures.Add("exit_code=$($Run.exit_code)") }
    if ($null -eq $Run.json) { $failures.Add("benchmark JSON invalid: $($Run.json_error)") }
    if ($null -eq $Run.json) {
        return [pscustomobject]@{ passed = $false; failures = $failures.ToArray() }
    }
    if ([string]::IsNullOrWhiteSpace([string]$Run.json.gpu_name)) { $failures.Add("gpu_name missing") }
    if ($Run.stdout_nonempty_lines -ne 1) { $failures.Add("stdout is not exactly one JSON line") }
    if (-not (Test-Finite $Run.json.valid_samples) -or [int64]$Run.json.valid_samples -le 0) { $failures.Add("valid_samples is not positive") }
    foreach ($pass in @("total", "acceleration_structure", "path_trace", "temporal", "atrous", "atrous_0", "atrous_1", "atrous_2", "atrous_3", "tone_map")) {
        $stats = $Run.json.passes.$pass
        if ($null -eq $stats -or -not (Test-Finite $stats.p50_ms) -or -not (Test-Finite $stats.p95_ms)) {
            $failures.Add("pass $pass has non-finite percentile")
        } else {
            if ([double]$stats.p95_ms -lt [double]$stats.p50_ms) {
                $failures.Add("pass $pass has p95 below p50")
            }
            if ([int64]$stats.valid_samples -ne [int64]$Run.json.valid_samples) {
                $failures.Add("pass $pass sample count differs from total")
            }
        }
    }
    $measurement = $Run.json.memory.measurement
    if ($null -eq $measurement) {
        $failures.Add("memory.measurement missing")
    } else {
        foreach ($field in @("valid_query_count", "failed_query_count", "checkpoint_count")) {
            if ($null -eq $measurement.$field) { $failures.Add("memory.measurement.$field missing") }
        }
        if ([int]$measurement.checkpoint_count -gt 60) { $failures.Add("memory checkpoint capacity exceeded") }
        if ([int64]$measurement.valid_query_count -le 0) { $failures.Add("memory measurement has no valid query") }
        if ($measurement.active) { $failures.Add("memory measurement is still active") }
        if (-not (Test-Finite $measurement.peak_usage_ratio) -or [double]$measurement.peak_usage_ratio -ge 0.70) {
            $failures.Add("memory peak usage ratio is unavailable or at least 70%")
        }
        $checkpoints = @($measurement.checkpoints)
        if ($checkpoints.Count -ne [int]$measurement.checkpoint_count) {
            $failures.Add("memory checkpoint count does not match the array")
        }
        for ($end = 4; $end -lt $checkpoints.Count; $end++) {
            $strictlyIncreasing = $true
            for ($index = $end - 3; $index -le $end; $index++) {
                if ([int64]$checkpoints[$index].usage_bytes -le [int64]$checkpoints[$index - 1].usage_bytes) {
                    $strictlyIncreasing = $false
                    break
                }
            }
            if ($strictlyIncreasing) {
                $failures.Add("memory usage increases across five consecutive checkpoints ending at $($checkpoints[$end].elapsed_seconds)s")
                break
            }
        }
    }

    if ($Expectations.ContainsKey("ExpectedOutput")) {
        $expectedWidth, $expectedHeight = [string]$Expectations.ExpectedOutput -split 'x'
        if ([int]$Run.json.output_width -ne [int]$expectedWidth -or [int]$Run.json.output_height -ne [int]$expectedHeight) {
            $failures.Add("output extent $($Run.json.output_width)x$($Run.json.output_height) differs from $($Expectations.ExpectedOutput)")
        }
    }
    if ($Expectations.ContainsKey("ExpectedRender")) {
        $expectedWidth, $expectedHeight = [string]$Expectations.ExpectedRender -split 'x'
        if ([int]$Run.json.render_width -ne [int]$expectedWidth -or [int]$Run.json.render_height -ne [int]$expectedHeight) {
            $failures.Add("render extent $($Run.json.render_width)x$($Run.json.render_height) differs from $($Expectations.ExpectedRender)")
        }
    }
    if ($Expectations.ContainsKey("ExpectedResolutionMode") -and [string]$Run.json.resolution_mode -ne [string]$Expectations.ExpectedResolutionMode) {
        $failures.Add("resolution mode $($Run.json.resolution_mode) differs from $($Expectations.ExpectedResolutionMode)")
    }
    if ($Expectations.ContainsKey("MaxTotalP95") -and [double]$Run.json.passes.total.p95_ms -gt [double]$Expectations.MaxTotalP95) {
        $failures.Add("Total p95 $($Run.json.passes.total.p95_ms) exceeds $($Expectations.MaxTotalP95) ms")
    }

    if ([string]$Run.json.resolution_mode -eq "fixed") {
        foreach ($field in @("render_generation_create_count", "render_generation_switch_count", "render_generation_retired_count", "render_extent_change_count", "history_reset_count", "gpu_idle_wait_count")) {
            if ([int64]$Run.json.$field -ne 0) { $failures.Add("fixed measurement has nonzero ${field}=$($Run.json.$field)") }
        }
    } elseif ([string]$Run.json.resolution_mode -eq "dynamic" -and $null -ne $Run.json.dynamic_resolution) {
        $dynamic = $Run.json.dynamic_resolution.measurement
        if ($null -ne $dynamic) {
            $expected = [int64]$dynamic.valid_samples + [int64]$dynamic.stale_generation_samples_ignored
            if ([int64]$Run.json.valid_samples -ne $expected) {
                $failures.Add("dynamic epoch mismatch total=$($Run.json.valid_samples) controller=$expected")
            }
            $switches = [int64]$dynamic.downscale_count + [int64]$dynamic.upscale_count
            if ([int64]$dynamic.switch_count -ne $switches) {
                $failures.Add("dynamic switch count does not equal downscale + upscale")
            }
            if ([int64]$Run.json.render_generation_create_count -ne $switches -or [int64]$Run.json.render_generation_switch_count -ne $switches) {
                $failures.Add("dynamic generation create/switch counts do not equal controller switches")
            }
            if ([int64]$Run.json.render_extent_change_count -ne $switches -or [int64]$Run.json.history_reset_count -ne $switches) {
                $failures.Add("dynamic extent/history reset counts do not equal controller switches")
            }
            if ([int64]$Run.json.render_generation_retired_count -ne $switches -or [int64]$Run.json.retired_generation_count -ne 0) {
                $failures.Add("dynamic generations are not fully retired at benchmark end")
            }
            if ([int64]$Run.json.retired_generation_high_watermark -gt 2) {
                $failures.Add("retired generation high-watermark exceeds 2")
            }
            if ([int64]$Run.json.gpu_idle_wait_count -ne 0) {
                $failures.Add("dynamic measurement has nonzero GPU idle waits")
            }
            if ($Expectations.ContainsKey("RequireDynamicSwitch") -and $Expectations.RequireDynamicSwitch -and $switches -le 0) {
                $failures.Add("workload did not produce a dynamic switch during measurement")
            }
        }
    } else {
        $failures.Add("resolution mode or dynamic telemetry is invalid")
    }
    [pscustomobject]@{ passed = ($failures.Count -eq 0); failures = $failures.ToArray() }
}

function Get-CaseSummary($Name, [object[]]$RunsInCase, [hashtable]$Expectations = @{}) {
    $validRuns = @($RunsInCase | Where-Object { $_.validation.passed -and $null -ne $_.json })
    $passNames = @("total", "acceleration_structure", "path_trace", "temporal", "atrous", "atrous_0", "atrous_1", "atrous_2", "atrous_3", "tone_map")
    $medians = [ordered]@{}
    foreach ($pass in $passNames) {
        $medians[$pass] = [ordered]@{
            p50_ms = Get-Median @($validRuns | ForEach-Object { $_.json.passes.$pass.p50_ms })
            p95_ms = Get-Median @($validRuns | ForEach-Object { $_.json.passes.$pass.p95_ms })
        }
    }
    $caseFailures = [System.Collections.Generic.List[string]]::new()
    if ($Expectations.ContainsKey("MaxMedianTotalP95") -and $null -ne $medians.total.p95_ms -and [double]$medians.total.p95_ms -gt [double]$Expectations.MaxMedianTotalP95) {
        $caseFailures.Add("median Total p95 $($medians.total.p95_ms) exceeds $($Expectations.MaxMedianTotalP95) ms")
    }
    [pscustomobject]@{
        name = $Name
        run_count = $RunsInCase.Count
        valid_run_count = $validRuns.Count
        passed = ($RunsInCase.Count -gt 0 -and $validRuns.Count -eq $RunsInCase.Count -and $caseFailures.Count -eq 0)
        failures = $caseFailures.ToArray()
        medians = $medians
        runs = $RunsInCase
    }
}

function Invoke-BenchmarkCase(
    [string]$Name,
    [string[]]$ExtraArguments,
    [int]$Count,
    [int]$Duration,
    [hashtable]$Expectations,
    [string]$Root,
    [string]$Executable,
    [System.Collections.Generic.List[object]]$AllRuns
) {
    $caseDirectory = Join-Path $Root ($Name -replace '[^A-Za-z0-9_.-]', '_')
    New-Item -ItemType Directory -Path $caseDirectory -Force | Out-Null
    $caseRuns = [System.Collections.Generic.List[object]]::new()
    for ($index = 1; $index -le $Count; $index++) {
        $arguments = @("--benchmark-seconds", "$Duration", "--command-recording-mode", "optimized", "--atrous-mode", "baseline", "--acceleration-structure-mode", "baseline") + $ExtraArguments
        $run = Invoke-RecordedProcess $Executable $arguments $caseDirectory ("run-{0:D2}" -f $index)
        $run | Add-Member -NotePropertyName validation -NotePropertyValue (Test-BenchmarkResult $run $Expectations)
        $caseRuns.Add($run)
        $AllRuns.Add($run)
    }
    return Get-CaseSummary $Name $caseRuns.ToArray() $Expectations
}

function Save-Summary($Path, $Summary) {
    $Summary | ConvertTo-Json -Depth 40 | Set-Content -LiteralPath $Path -Encoding UTF8
}

function Invoke-CaptureCase(
    [string]$Name,
    [string[]]$ExtraArguments,
    [string]$ExpectedView,
    [string]$Root,
    [string]$Executable
) {
    $captureDirectory = Join-Path $Root "captures"
    New-Item -ItemType Directory -Path $captureDirectory -Force | Out-Null
    $pngPath = Join-Path $captureDirectory "$Name.png"
    $expectedOutput = "1280x720"
    for ($index = 0; $index -lt $ExtraArguments.Count - 1; $index++) {
        if ($ExtraArguments[$index] -eq "--output-size") {
            $expectedOutput = $ExtraArguments[$index + 1]
        }
    }
    $arguments = @("--capture-output", $pngPath, "--capture-after-spp", "128", "--output-size", "1280x720") + $ExtraArguments
    $run = Invoke-RecordedProcess $Executable $arguments $captureDirectory $Name
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($run.exit_code -ne 0) { $failures.Add("capture exit=$($run.exit_code)") }
    if ($null -eq $run.json) { $failures.Add("capture JSON invalid: $($run.json_error)") }
    if ($run.stdout_nonempty_lines -ne 1) { $failures.Add("capture stdout is not exactly one JSON line") }
    if (-not (Test-Path -LiteralPath $pngPath)) { $failures.Add("capture PNG is missing") }
    if ($null -ne $run.json) {
        if ([int]$run.json.actual_spp -lt 128) { $failures.Add("capture SPP is below requested 128") }
        if ([string]$run.json.debug_view.name -ne $ExpectedView) { $failures.Add("capture debug view differs from $ExpectedView") }
        $expectedWidth, $expectedHeight = $expectedOutput -split 'x'
        if ([int]$run.json.output_width -ne [int]$expectedWidth -or [int]$run.json.output_height -ne [int]$expectedHeight) { $failures.Add("capture output extent differs from $expectedOutput") }
        if ([int64]$run.json.png_bytes -le 0) { $failures.Add("capture PNG byte count is not positive") }
        if ([IO.Path]::GetFullPath([string]$run.json.png_path) -ne [IO.Path]::GetFullPath($pngPath)) { $failures.Add("capture JSON path differs from requested path") }
        if ($Name -like "dynamic-forced-*" -and [int]$run.json.render_width -ge [int]$run.json.output_width -and [int]$run.json.render_height -ge [int]$run.json.output_height) {
            $failures.Add("forced dynamic capture did not reduce the render extent")
        }
    }
    $hash = if (Test-Path -LiteralPath $pngPath) { (Get-FileHash -LiteralPath $pngPath -Algorithm SHA256).Hash } else { $null }
    [pscustomobject]@{
        name = $Name
        passed = ($failures.Count -eq 0)
        png_path = $pngPath
        sha256 = $hash
        json = $run.json
        run = $run
        failures = $failures.ToArray()
    }
}

function Invoke-DiffCase(
    [string]$Name,
    [string]$Left,
    [string]$Right,
    [string]$Root,
    [string]$ImageDiffExecutable,
    [ValidateSet("Exact", "AtrousTolerance")]
    [string]$Threshold
) {
    $diffDirectory = Join-Path $Root "diffs"
    New-Item -ItemType Directory -Path $diffDirectory -Force | Out-Null
    $run = Invoke-RecordedProcess $ImageDiffExecutable @($Left, $Right) $diffDirectory $Name
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($run.exit_code -ne 0 -or $null -eq $run.json) {
        $failures.Add("image_diff failed or returned invalid JSON")
    } elseif ($Threshold -eq "Exact") {
        if ([int64]$run.json.changed_rgb_pixels -ne 0 -or [int64]$run.json.alpha_mismatch_count -ne 0 -or [int]$run.json.max_channel_abs_diff -ne 0 -or [double]$run.json.rmse -ne 0.0) {
            $failures.Add("exact image pair differs")
        }
    } elseif ([int64]$run.json.alpha_mismatch_count -ne 0 -or [int]$run.json.max_channel_abs_diff -gt 1 -or [double]$run.json.rmse -gt 0.10) {
        $failures.Add("À-Trous image pair exceeds max_abs=1 or RMSE=0.10")
    }
    [pscustomobject]@{
        name = $Name
        passed = ($failures.Count -eq 0)
        threshold = $Threshold
        json = $run.json
        run = $run
        failures = $failures.ToArray()
    }
}

if ($SelfTest) {
    Invoke-MedianSelfTest
    exit 0
}

if ($Suite -eq "Recheck1080" -and (-not $SkipLongRun -or -not $SkipCaptures)) {
    throw "Recheck1080 requires both -SkipLongRun and -SkipCaptures"
}

$repoRoot = (Get-Location).Path
$resolvedOutputRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot $OutputRoot))
$runId = "{0}-{1}" -f (Get-Date -Format "yyyyMMdd-HHmmss"), ([guid]::NewGuid().ToString("N").Substring(0, 8))
$runRoot = Join-Path $resolvedOutputRoot $runId
New-Item -ItemType Directory -Path $runRoot -Force | Out-Null

if ([string]::IsNullOrWhiteSpace($Exe)) {
    $configuration = $Configuration.ToLowerInvariant()
    $Exe = Join-Path $repoRoot "target/$configuration/ray_tracing_demo.exe"
} else {
    $configuration = $Configuration.ToLowerInvariant()
}
$Executable = [IO.Path]::GetFullPath($Exe)
if (-not (Test-Path -LiteralPath $Executable)) { throw "executable not found: $Executable" }

$environment = Get-EnvironmentSnapshot
$head = (& git rev-parse HEAD).Trim()
$allRuns = [System.Collections.Generic.List[object]]::new()
$caseSummaries = [System.Collections.Generic.List[object]]::new()
$startedAt = Get-Date

$smokeExpectations = @{
    ExpectedOutput = "1920x1080"
    ExpectedResolutionMode = "dynamic"
    RequireDynamicSwitch = $true
    MaxTotalP95 = 16.67
}
$smoke = Invoke-BenchmarkCase "smoke-forced-dynamic" @("--output-size", "1920x1080", "--dynamic-resolution", "--target-gpu-ms", "4") 1 $Seconds $smokeExpectations $runRoot $Executable $allRuns
$caseSummaries.Add($smoke)
$smokePassed = $smoke.passed
if ($environment.gpu_name -eq "N/A" -and $smoke.runs.Count -gt 0 -and $null -ne $smoke.runs[0].json) {
    $environment.gpu_name = [string]$smoke.runs[0].json.gpu_name
}

$summary = [ordered]@{
    schema_version = 1
    stage = "8G"
    suite = $Suite
    git_head = $head
    run_id = $runId
    started_at = $startedAt.ToString("o")
    environment = $environment
    executable = $Executable
    smoke_passed = $smokePassed
    smoke = $smoke
    cases = @()
    long_run = $null
    captures = @()
}

if (-not $smokePassed -or $SmokeOnly) {
    $summary.cases = $caseSummaries.ToArray()
    $summary.finished_at = (Get-Date).ToString("o")
    $summary.passed = $smokePassed
    Save-Summary (Join-Path $runRoot "summary.json") $summary
    if (-not $smokePassed) { exit 1 }
    exit 0
}

if ($Suite -eq "Recheck1080") {
    $fixed1080Expectations = @{
        ExpectedOutput = "1920x1080"
        ExpectedRender = "1920x1080"
        ExpectedResolutionMode = "fixed"
        MaxTotalP95 = 16.67
        MaxMedianTotalP95 = 7.6632
    }
    $caseSummaries.Add((Invoke-BenchmarkCase "fixed-1920x1080" @("--output-size", "1920x1080", "--render-scale", "1.0") $Runs $Seconds $fixed1080Expectations $runRoot $Executable $allRuns))
    $dynamic1080Expectations = @{
        ExpectedOutput = "1920x1080"
        ExpectedResolutionMode = "dynamic"
        MaxTotalP95 = 16.67
        MaxMedianTotalP95 = 7.6632
    }
    $caseSummaries.Add((Invoke-BenchmarkCase "dynamic-default" @("--output-size", "1920x1080", "--dynamic-resolution") $Runs $Seconds $dynamic1080Expectations $runRoot $Executable $allRuns))
    $summary.cases = $caseSummaries.ToArray()
    $summary.finished_at = (Get-Date).ToString("o")
    $summary.passed = (@($caseSummaries | Where-Object { -not $_.passed }).Count -eq 0)
    Save-Summary (Join-Path $runRoot "summary.json") $summary
    if (-not $summary.passed) { exit 1 }
    exit 0
}

$fixedSizes = @("1280x720", "1600x900", "1920x1080")
foreach ($size in $fixedSizes) {
    $expectations = @{
        ExpectedOutput = $size
        ExpectedRender = $size
        ExpectedResolutionMode = "fixed"
        MaxTotalP95 = 16.67
    }
    if ($size -eq "1920x1080") { $expectations.MaxMedianTotalP95 = 7.6632 }
    $caseSummaries.Add((Invoke-BenchmarkCase "fixed-$size" @("--output-size", $size, "--render-scale", "1.0") $Runs $Seconds $expectations $runRoot $Executable $allRuns))
}
$fixedScaleRenders = [ordered]@{ "0.83" = "1592x896"; "0.75" = "1440x808"; "0.67" = "1280x720" }
foreach ($scale in $fixedScaleRenders.Keys) {
    $expectations = @{
        ExpectedOutput = "1920x1080"
        ExpectedRender = $fixedScaleRenders[$scale]
        ExpectedResolutionMode = "fixed"
        MaxTotalP95 = 16.67
    }
    $caseSummaries.Add((Invoke-BenchmarkCase "fixed-scale-$scale" @("--output-size", "1920x1080", "--render-scale", $scale) 1 $Seconds $expectations $runRoot $Executable $allRuns))
}
$dynamicDefaultExpectations = @{
    ExpectedOutput = "1920x1080"
    ExpectedResolutionMode = "dynamic"
    MaxTotalP95 = 16.67
    MaxMedianTotalP95 = 7.6632
}
$caseSummaries.Add((Invoke-BenchmarkCase "dynamic-default" @("--output-size", "1920x1080", "--dynamic-resolution") $Runs $Seconds $dynamicDefaultExpectations $runRoot $Executable $allRuns))

$fixturePath = (Resolve-Path "assets/gltf/Triangle/NonIndexedMultiNode.gltf").Path
foreach ($accelerationMode in @("baseline", "optimized")) {
    $fixtureExpectations = @{ ExpectedOutput = "1280x720"; ExpectedResolutionMode = "fixed"; MaxTotalP95 = 16.67 }
    $caseSummaries.Add((Invoke-BenchmarkCase "fixture-static-$accelerationMode" @("--output-size", "1280x720", "--model", $fixturePath, "--acceleration-structure-mode", $accelerationMode) 1 $Seconds $fixtureExpectations $runRoot $Executable $allRuns))
    $caseSummaries.Add((Invoke-BenchmarkCase "fixture-animated-$accelerationMode" @("--output-size", "1280x720", "--model", $fixturePath, "--animate-model", "--acceleration-structure-mode", $accelerationMode) 1 $Seconds $fixtureExpectations $runRoot $Executable $allRuns))
}

if (-not [string]::IsNullOrWhiteSpace($PbrModel)) {
    $modelPath = [IO.Path]::GetFullPath($PbrModel)
    if (-not (Test-Path -LiteralPath $modelPath)) { throw "PbrModel not found: $modelPath" }
    $pbrExpectations = @{ ExpectedOutput = "1280x720"; ExpectedResolutionMode = "fixed"; MaxTotalP95 = 16.67 }
    $caseSummaries.Add((Invoke-BenchmarkCase "pbr-static" @("--output-size", "1280x720", "--model", $modelPath) 1 $Seconds $pbrExpectations $runRoot $Executable $allRuns))
    $caseSummaries.Add((Invoke-BenchmarkCase "pbr-animated" @("--output-size", "1280x720", "--model", $modelPath, "--animate-model") 1 $Seconds $pbrExpectations $runRoot $Executable $allRuns))
}
if (-not [string]::IsNullOrWhiteSpace($LargeModel)) {
    $largePath = [IO.Path]::GetFullPath($LargeModel)
    if (-not (Test-Path -LiteralPath $largePath)) { throw "LargeModel not found: $largePath" }
    foreach ($accelerationMode in @("baseline", "optimized")) {
        $largeExpectations = @{ ExpectedOutput = "1600x900"; ExpectedResolutionMode = "fixed"; MaxTotalP95 = 16.67 }
        $caseSummaries.Add((Invoke-BenchmarkCase "large-static-$accelerationMode" @("--output-size", "1600x900", "--model", $largePath, "--acceleration-structure-mode", $accelerationMode) $Runs $Seconds $largeExpectations $runRoot $Executable $allRuns))
    }
}

if (-not $SkipLongRun) {
    $longExpectations = @{
        ExpectedOutput = "1920x1080"
        ExpectedResolutionMode = "dynamic"
        RequireDynamicSwitch = $true
        MaxTotalP95 = 16.67
    }
    $longCase = Invoke-BenchmarkCase "forced-dynamic-long" @("--output-size", "1920x1080", "--dynamic-resolution", "--target-gpu-ms", "4") 1 $LongRunSeconds $longExpectations $runRoot $Executable $allRuns
    $summary.long_run = $longCase
    $caseSummaries.Add($longCase)
}

$captureRecords = [System.Collections.Generic.List[object]]::new()
$diffRecords = [System.Collections.Generic.List[object]]::new()
if (-not $SkipCaptures) {
    $views = @("final", "raw", "albedo", "normal-roughness", "depth", "motion", "variance", "history-rejection", "history-length", "object-material-id", "specular-hit-distance")
    foreach ($recordingMode in @("baseline", "optimized")) {
        foreach ($view in $views) {
            $captureRecords.Add((Invoke-CaptureCase "command-$recordingMode-$view" @("--command-recording-mode", $recordingMode, "--debug-view", $view) $view $runRoot $Executable))
        }
    }
    foreach ($accelerationMode in @("baseline", "optimized")) {
        foreach ($view in $views) {
            $captureRecords.Add((Invoke-CaptureCase "as-$accelerationMode-$view" @("--model", $fixturePath, "--acceleration-structure-mode", $accelerationMode, "--debug-view", $view) $view $runRoot $Executable))
        }
    }
    foreach ($atrousMode in @("baseline", "shared")) {
        $captureRecords.Add((Invoke-CaptureCase "atrous-$atrousMode-final" @("--atrous-mode", $atrousMode) "final" $runRoot $Executable))
    }
    foreach ($scale in @("1.0", "0.83", "0.75", "0.67")) {
        $captureRecords.Add((Invoke-CaptureCase "scale-$scale-final" @("--render-scale", $scale) "final" $runRoot $Executable))
    }
    foreach ($resolutionArguments in @(
        @("dynamic-default", "--output-size", "1920x1080", "--dynamic-resolution"),
        @("dynamic-forced", "--output-size", "1920x1080", "--dynamic-resolution", "--target-gpu-ms", "4")
    )) {
        $captureRecords.Add((Invoke-CaptureCase "$($resolutionArguments[0])-final" @($resolutionArguments[1..($resolutionArguments.Count - 1)]) "final" $runRoot $Executable))
        $captureRecords.Add((Invoke-CaptureCase "$($resolutionArguments[0])-history-rejection" (@($resolutionArguments[1..($resolutionArguments.Count - 1)]) + @("--debug-view", "history-rejection")) "history-rejection" $runRoot $Executable))
    }
    $imageDiffExecutable = Join-Path $repoRoot "target/$configuration/image_diff.exe"
    if (Test-Path -LiteralPath $imageDiffExecutable) {
        foreach ($view in $views) {
            $commandBaseline = ($captureRecords | Where-Object { $_.name -eq "command-baseline-$view" }).png_path
            $commandOptimized = ($captureRecords | Where-Object { $_.name -eq "command-optimized-$view" }).png_path
            if ($null -ne $commandBaseline -and $null -ne $commandOptimized) {
                $diffRecords.Add((Invoke-DiffCase "command-baseline-vs-optimized-$view" $commandBaseline $commandOptimized $runRoot $imageDiffExecutable "Exact"))
            }
            $asBaseline = ($captureRecords | Where-Object { $_.name -eq "as-baseline-$view" }).png_path
            $asOptimized = ($captureRecords | Where-Object { $_.name -eq "as-optimized-$view" }).png_path
            if ($null -ne $asBaseline -and $null -ne $asOptimized) {
                $diffRecords.Add((Invoke-DiffCase "as-baseline-vs-optimized-$view" $asBaseline $asOptimized $runRoot $imageDiffExecutable "Exact"))
            }
        }
        $atrousBaseline = ($captureRecords | Where-Object { $_.name -eq "atrous-baseline-final" }).png_path
        $atrousShared = ($captureRecords | Where-Object { $_.name -eq "atrous-shared-final" }).png_path
        if ($null -ne $atrousBaseline -and $null -ne $atrousShared) {
            $diffRecords.Add((Invoke-DiffCase "atrous-baseline-vs-shared-final" $atrousBaseline $atrousShared $runRoot $imageDiffExecutable "AtrousTolerance"))
        }
    } else {
        $diffRecords.Add([pscustomobject]@{
            name = "image-diff-tool"
            passed = $false
            failures = @("image_diff executable not found: $imageDiffExecutable")
        })
    }
}

$summary.cases = $caseSummaries.ToArray()
$summary.captures = [pscustomobject]@{ records = $captureRecords.ToArray(); diffs = $diffRecords.ToArray() }
$summary.finished_at = (Get-Date).ToString("o")
$summary.passed = (@($caseSummaries | Where-Object { -not $_.passed }).Count -eq 0) -and
    (@($captureRecords | Where-Object { -not $_.passed }).Count -eq 0) -and
    (@($diffRecords | Where-Object { -not $_.passed }).Count -eq 0)
Save-Summary (Join-Path $runRoot "summary.json") $summary
if (-not $summary.passed) { exit 1 }
exit 0
