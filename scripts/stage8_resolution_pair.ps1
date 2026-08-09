[CmdletBinding()]
param(
    [string]$CandidateManifest,
    [string]$OutputRoot = "output/stage8g",
    [string]$RunId,
    [ValidateSet(3)]
    [int]$Runs = 3,
    [ValidateRange(1, 30)]
    [int]$Seconds = 30,
    [ValidateRange(45, 300)]
    [int]$ProcessTimeoutSeconds = 90,
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Quote-Argument([string]$Value) {
    if ($Value -notmatch '[\s"]') { return $Value }
    return '"' + $Value.Replace('"', '\"') + '"'
}

function Save-Json([string]$Path, $Value) {
    $Value | ConvertTo-Json -Depth 60 | Set-Content -LiteralPath $Path -Encoding UTF8
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

function Get-Sha256([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $null }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function Get-ResolutionSequence {
    return @(
        [pscustomobject]@{ side = "dynamic"; index = 1 },
        [pscustomobject]@{ side = "fixed"; index = 1 },
        [pscustomobject]@{ side = "fixed"; index = 2 },
        [pscustomobject]@{ side = "dynamic"; index = 2 },
        [pscustomobject]@{ side = "dynamic"; index = 3 },
        [pscustomobject]@{ side = "fixed"; index = 3 }
    )
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

    $driver = if ($null -ne $gpu) { [string]$gpu.DriverVersion } else { "N/A" }
    $driverSource = if ($null -ne $gpu) { "Win32_VideoController" } else { "unavailable" }
    $driverError = $null
    if ($driver -eq "N/A") {
        try {
            $smi = @(& nvidia-smi --query-gpu=driver_version --format=csv,noheader,nounits 2>&1 |
                ForEach-Object { [string]$_ } |
                Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
            if ($smi.Count -gt 0 -and $smi[0] -notmatch 'not recognized|failed|error') {
                $driver = $smi[0].Trim()
                $driverSource = "nvidia-smi"
            } else {
                $driverError = ($smi -join " ").Trim()
            }
        } catch {
            $driverError = $_.Exception.Message
        }
    }

    $powerScheme = "N/A"
    $powerSchemeSource = "unavailable"
    $powerSchemeError = $null
    try {
        $powerOutput = @(& powercfg /getactivescheme 2>&1 | ForEach-Object { [string]$_ })
        if ($LASTEXITCODE -eq 0 -and $powerOutput.Count -gt 0) {
            $powerScheme = ($powerOutput -join " ").Trim()
            $powerSchemeSource = "powercfg /getactivescheme"
        } else {
            $powerSchemeError = ($powerOutput -join " ").Trim()
        }
    } catch {
        $powerSchemeError = $_.Exception.Message
    }

    $powerValue = "N/A"
    $powerProbe = "unavailable"
    $powerError = $null
    try {
        if (-not ("Stage8Validation.ResolutionPairPowerStatus" -as [type])) {
            Add-Type -TypeDefinition @"
using System.Runtime.InteropServices;
namespace Stage8Validation {
    [StructLayout(LayoutKind.Sequential)]
    public struct ResolutionPairPowerStatus {
        public byte ACLineStatus;
        public byte BatteryFlag;
        public byte BatteryLifePercent;
        public byte Reserved;
        public int BatteryLifeTime;
        public int BatteryFullLifeTime;
    }
    public static class ResolutionPairPowerStatusApi {
        [DllImport("kernel32.dll")]
        public static extern bool GetSystemPowerStatus(out ResolutionPairPowerStatus status);
    }
}
"@
        }
        $status = New-Object Stage8Validation.ResolutionPairPowerStatus
        if ([Stage8Validation.ResolutionPairPowerStatusApi]::GetSystemPowerStatus([ref]$status)) {
            $powerProbe = "GetSystemPowerStatus"
            if ($status.ACLineStatus -eq 1) { $powerValue = "AC" }
            elseif ($status.ACLineStatus -eq 0) { $powerValue = "battery" }
            else { $powerError = "GetSystemPowerStatus returned unknown ACLineStatus" }
        } else {
            $powerError = "GetSystemPowerStatus returned false"
        }
    } catch {
        $powerError = $_.Exception.Message
    }
    if ($powerValue -eq "N/A") {
        try {
            $battery = Get-CimInstance Win32_Battery -ErrorAction Stop | Select-Object -First 1
            if ($null -ne $battery) {
                $powerProbe = "Win32_Battery.BatteryStatus"
                if ([int]$battery.BatteryStatus -eq 2) { $powerValue = "AC" }
                elseif ([int]$battery.BatteryStatus -in @(1, 3, 4, 5)) { $powerValue = "battery" }
            }
        } catch {
            if ($null -eq $powerError) { $powerError = $_.Exception.Message }
        }
    }

    return [pscustomobject]@{
        gpu_name = if ($null -ne $gpu) { [string]$gpu.Name } else { "N/A" }
        gpu_name_source = if ($null -ne $gpu) { "Win32_VideoController" } else { "unavailable" }
        driver_version = $driver
        driver_version_source = $driverSource
        driver_version_error = $driverError
        active_power_scheme = $powerScheme
        active_power_scheme_source = $powerSchemeSource
        active_power_scheme_error = $powerSchemeError
        power_source = $powerValue
        power_source_probe = $powerProbe
        power_source_error = $powerError
        gpu_probe_error = $gpuError
    }
}

function Test-EnvironmentContinuity($Before, $After) {
    $failures = [System.Collections.Generic.List[string]]::new()
    foreach ($field in @("gpu_name", "driver_version", "active_power_scheme", "power_source")) {
        if ([string]::IsNullOrWhiteSpace([string]$Before.$field) -or [string]$Before.$field -eq "N/A") {
            $failures.Add("environment_before.$field is unavailable")
        }
        if ([string]::IsNullOrWhiteSpace([string]$After.$field) -or [string]$After.$field -eq "N/A") {
            $failures.Add("environment_after.$field is unavailable")
        }
    }
    foreach ($field in @("gpu_name", "driver_version", "active_power_scheme")) {
        if ([string]$Before.$field -ne [string]$After.$field) { $failures.Add("environment changed: $field") }
    }
    if ([string]$Before.power_source -ne "AC" -or [string]$After.power_source -ne "AC") {
        $failures.Add("power source was not AC for both snapshots")
    }
    if ([string]$Before.gpu_name -notmatch 'RTX 4060 Laptop') { $failures.Add("GPU is not RTX 4060 Laptop") }
    return [pscustomobject]@{ passed = $failures.Count -eq 0; failures = $failures.ToArray() }
}

function Test-CandidateManifest($Manifest) {
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($null -eq $Manifest) {
        $failures.Add("candidate manifest is missing")
        return [pscustomobject]@{ passed = $false; failures = $failures.ToArray() }
    }
    foreach ($field in @("commit", "tree", "archive_path", "archive_sha256", "archive_comment", "cargo_lock_sha256", "executable", "exe_sha256")) {
        if ([string]::IsNullOrWhiteSpace([string]$Manifest.$field)) { $failures.Add("manifest.$field is missing") }
    }
    if (-not [bool]$Manifest.valid) { $failures.Add("manifest valid flag is false") }
    if ([string]$Manifest.archive_comment -ne [string]$Manifest.commit) { $failures.Add("archive comment differs from commit") }
    if (-not [string]::IsNullOrWhiteSpace([string]$Manifest.archive_path) -and (Get-Sha256 ([string]$Manifest.archive_path)) -ne [string]$Manifest.archive_sha256) {
        $failures.Add("archive SHA-256 differs")
    }
    if (-not [string]::IsNullOrWhiteSpace([string]$Manifest.executable) -and (Get-Sha256 ([string]$Manifest.executable)) -ne [string]$Manifest.exe_sha256) {
        $failures.Add("executable SHA-256 differs")
    }
    return [pscustomobject]@{ passed = $failures.Count -eq 0; failures = $failures.ToArray() }
}

function Invoke-RecordedProcess(
    [string]$FilePath,
    [string[]]$Arguments,
    [string]$CaseDirectory,
    [string]$Label,
    [string]$WorkingDirectory
) {
    New-Item -ItemType Directory -Path $CaseDirectory -Force | Out-Null
    $stdoutPath = Join-Path $CaseDirectory "$Label.stdout.txt"
    $stderrPath = Join-Path $CaseDirectory "$Label.stderr.txt"
    $argsPath = Join-Path $CaseDirectory "$Label.args.json"
    $argumentString = ($Arguments | ForEach-Object { Quote-Argument $_ }) -join ' '
    $commandLine = "$(Quote-Argument $FilePath) $argumentString"
    Save-Json $argsPath ([ordered]@{
        executable = $FilePath
        args = $Arguments
        command = $commandLine
        working_directory = $WorkingDirectory
        timeout_seconds = $ProcessTimeoutSeconds
    })

    $started = Get-Date
    $exitCode = -1
    $timedOut = $false
    $launchError = $null
    try {
        $process = Start-Process -FilePath $FilePath -ArgumentList $argumentString `
            -WorkingDirectory $WorkingDirectory `
            -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath `
            -PassThru -WindowStyle Hidden
        if (-not $process.WaitForExit($ProcessTimeoutSeconds * 1000)) {
            $timedOut = $true
            try { $process.Kill($true) } catch { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
            $process.WaitForExit(10000) | Out-Null
            Add-Content -LiteralPath $stderrPath -Value "process timed out after $ProcessTimeoutSeconds seconds" -Encoding UTF8
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
    $jsonError = if ($timedOut) { "process timed out after $ProcessTimeoutSeconds seconds" } else { $launchError }
    if ($lines.Count -gt 0) {
        try { $json = $lines[-1] | ConvertFrom-Json -Depth 50 } catch { if ($null -eq $jsonError) { $jsonError = $_.Exception.Message } }
    } elseif ($null -eq $jsonError) {
        $jsonError = "stdout has no non-empty JSON line"
    }

    return [pscustomobject]@{
        label = $Label
        command = $commandLine
        args = $Arguments
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        args_path = $argsPath
        exit_code = $exitCode
        timed_out = $timedOut
        timeout_seconds = $ProcessTimeoutSeconds
        elapsed_seconds = ($finished - $started).TotalSeconds
        stdout_nonempty_lines = $lines.Count
        json = $json
        json_error = $jsonError
        stderr_nonempty = -not [string]::IsNullOrWhiteSpace($stderr)
    }
}

function Get-RunStderr($Run) {
    if (-not [string]::IsNullOrWhiteSpace([string]$Run.stderr_path) -and (Test-Path -LiteralPath $Run.stderr_path)) {
        return Get-Content -LiteralPath $Run.stderr_path -Raw
    }
    return ""
}

function Test-ResolutionRun($Run, [string]$Side, [string]$ExpectedExeSha256) {
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($Run.timed_out) { $failures.Add("process timed out") }
    if ($Run.exit_code -ne 0) { $failures.Add("exit_code=$($Run.exit_code)") }
    if ([string]$Run.exe_sha256_before -ne $ExpectedExeSha256) { $failures.Add("executable SHA-256 changed before run") }
    if ($Run.stdout_nonempty_lines -ne 1) { $failures.Add("stdout is not exactly one non-empty JSON line") }
    if ($null -eq $Run.json) { $failures.Add("JSON parse failed: $($Run.json_error)") }
    $stderr = Get-RunStderr $Run
    foreach ($pattern in @('device removed', 'panic')) {
        if ($stderr -match $pattern) { $failures.Add("stderr contains forbidden marker: $pattern") }
    }
    if ($null -ne $Run.json) {
        if ([int]$Run.json.output_width -ne 1920 -or [int]$Run.json.output_height -ne 1080) { $failures.Add("output extent is not 1920x1080") }
        if ($null -eq $Run.json.valid_samples -or [int64]$Run.json.valid_samples -le 0) { $failures.Add("valid_samples is not positive") }
        $total = $Run.json.passes.total
        if ($null -eq $total -or -not (Test-Finite $total.p50_ms) -or -not (Test-Finite $total.p95_ms)) {
            $failures.Add("Total p50/p95 is not finite")
        } elseif ([double]$total.p95_ms -lt [double]$total.p50_ms) {
            $failures.Add("Total p95 is below p50")
        } elseif ([double]$total.p95_ms -gt 16.67) {
            $failures.Add("Total p95 exceeds 16.67 ms")
        }

        if ($Side -eq "fixed") {
            if ([string]$Run.json.resolution_mode -ne "fixed") { $failures.Add("fixed side resolution mode differs") }
            if ([int]$Run.json.render_width -ne 1920 -or [int]$Run.json.render_height -ne 1080) { $failures.Add("fixed side render extent differs") }
        } else {
            if ([string]$Run.json.resolution_mode -ne "dynamic") { $failures.Add("dynamic side resolution mode differs") }
            foreach ($prefix in @("render", "render_min", "render_max")) {
                if ([int]$Run.json."${prefix}_width" -ne 1920 -or [int]$Run.json."${prefix}_height" -ne 1080) {
                    $failures.Add("dynamic side $prefix extent differs")
                }
            }
            $measurement = $Run.json.dynamic_resolution.measurement
            if ($null -eq $measurement) {
                $failures.Add("dynamic measurement is missing")
            } else {
                if ([int64]$measurement.switch_count -ne 0) { $failures.Add("dynamic switch_count is not zero") }
                if ([int64]$measurement.downscale_count -ne 0) { $failures.Add("dynamic downscale_count is not zero") }
                if ([int64]$measurement.upscale_count -ne 0) { $failures.Add("dynamic upscale_count is not zero") }
                if ([int64]$measurement.stale_generation_samples_ignored -ne 0) { $failures.Add("dynamic stale generation samples are not zero") }
            }
        }
    }
    return [pscustomobject]@{ passed = $failures.Count -eq 0; failures = $failures.ToArray() }
}

function Invoke-SelfTest {
    $caseCount = 0
    if ((Get-Median @(3, 1, 2)) -ne 2.0 -or (Get-Median @(4, 1, 3, 2)) -ne 2.5) { throw "median self-test failed" }
    $caseCount++
    $sequenceText = @((Get-ResolutionSequence) | ForEach-Object { "$($_.side)-$($_.index)" }) -join ","
    if ($sequenceText -ne "dynamic-1,fixed-1,fixed-2,dynamic-2,dynamic-3,fixed-3") { throw "sequence self-test failed" }
    $caseCount++
    if ((Test-CandidateManifest $null).passed) { throw "missing manifest self-test failed" }
    $caseCount++
    $environment = [pscustomobject]@{ gpu_name = "NVIDIA GeForce RTX 4060 Laptop GPU"; driver_version = "1"; active_power_scheme = "balanced"; power_source = "AC" }
    $battery = [pscustomobject]@{ gpu_name = $environment.gpu_name; driver_version = "1"; active_power_scheme = "balanced"; power_source = "battery" }
    if (-not (Test-EnvironmentContinuity $environment $environment).passed -or (Test-EnvironmentContinuity $environment $battery).passed) { throw "environment self-test failed" }
    $caseCount++
    $timeoutRun = [pscustomobject]@{ timed_out = $true; exit_code = -1; exe_sha256_before = "x"; stdout_nonempty_lines = 0; json = $null; json_error = "timeout"; stderr_path = "" }
    if ((Test-ResolutionRun $timeoutRun "fixed" "x").passed) { throw "timeout self-test failed" }
    $caseCount++
    [ordered]@{ self_test = "passed"; cases = $caseCount; runs = $Runs } | ConvertTo-Json -Compress
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

if ([string]::IsNullOrWhiteSpace($CandidateManifest)) { throw "CandidateManifest is required unless -SelfTest is used" }
$repoRoot = (Get-Location).Path
$manifestPath = [IO.Path]::GetFullPath((Join-Path $repoRoot $CandidateManifest))
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw "candidate manifest not found: $manifestPath" }
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json -Depth 60
$manifestValidation = Test-CandidateManifest $manifest

$resolvedOutputRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot $OutputRoot))
if ([string]::IsNullOrWhiteSpace($RunId)) {
    $runId = "paired-resolution-1080-{0}-{1}" -f (Get-Date -Format "yyyyMMdd-HHmmss"), ([guid]::NewGuid().ToString("N").Substring(0, 8))
} else {
    $runId = $RunId
}
$runRoot = Join-Path $resolvedOutputRoot $runId
if (Test-Path -LiteralPath $runRoot) { throw "run root already exists: $runRoot" }
New-Item -ItemType Directory -Path $runRoot -Force | Out-Null

$startedAt = Get-Date
$summaryPath = Join-Path $runRoot "summary.json"
$environmentBefore = Get-EnvironmentSnapshot
Save-Json (Join-Path $runRoot "environment_before.json") $environmentBefore
$runRecords = [System.Collections.Generic.List[object]]::new()
$fatalError = $null

try {
    if (-not $manifestValidation.passed) { throw "candidate manifest validation failed: $($manifestValidation.failures -join '; ')" }
    $commonArguments = @(
        "--benchmark-seconds", "$Seconds",
        "--output-size", "1920x1080",
        "--command-recording-mode", "optimized",
        "--atrous-mode", "baseline",
        "--acceleration-structure-mode", "baseline"
    )
    foreach ($item in @(Get-ResolutionSequence)) {
        $sideArguments = if ($item.side -eq "dynamic") { @("--dynamic-resolution") } else { @("--render-scale", "1.0") }
        $arguments = $commonArguments + $sideArguments
        $caseDirectory = Join-Path $runRoot ("{0}-{1:D2}" -f $item.side, $item.index)
        $run = Invoke-RecordedProcess $manifest.executable $arguments $caseDirectory "run" ([IO.Path]::GetDirectoryName([string]$manifest.executable))
        $run | Add-Member -NotePropertyName side -NotePropertyValue $item.side
        $run | Add-Member -NotePropertyName index -NotePropertyValue $item.index
        $run | Add-Member -NotePropertyName exe_sha256_before -NotePropertyValue (Get-Sha256 ([string]$manifest.executable))
        $run | Add-Member -NotePropertyName validation -NotePropertyValue (Test-ResolutionRun $run $item.side ([string]$manifest.exe_sha256))
        $runRecords.Add($run)
        if (-not $run.validation.passed) { throw "resolution pair run failed: $($item.side)-$($item.index)" }
    }
} catch {
    $fatalError = $_.Exception.Message
}

$environmentAfter = Get-EnvironmentSnapshot
Save-Json (Join-Path $runRoot "environment_after.json") $environmentAfter
$environmentValidation = Test-EnvironmentContinuity $environmentBefore $environmentAfter
$dynamicRuns = @($runRecords | Where-Object { $_.side -eq "dynamic" })
$fixedRuns = @($runRecords | Where-Object { $_.side -eq "fixed" })
$dynamicValidRuns = @($dynamicRuns | Where-Object { $_.validation.passed })
$fixedValidRuns = @($fixedRuns | Where-Object { $_.validation.passed })
$dynamicP95 = @($dynamicValidRuns | ForEach-Object { [double]$_.json.passes.total.p95_ms })
$fixedP95 = @($fixedValidRuns | ForEach-Object { [double]$_.json.passes.total.p95_ms })
$dynamicMedian = Get-Median $dynamicP95
$fixedMedian = Get-Median $fixedP95
$delta = $null
$absoluteGate = "INCONCLUSIVE"
$modeRegression = "INCONCLUSIVE"
$decision = "INCONCLUSIVE"
$evidenceValid = $null -eq $fatalError -and $manifestValidation.passed -and $environmentValidation.passed -and
    $dynamicP95.Count -eq $Runs -and $fixedP95.Count -eq $Runs -and
    (Test-Finite $dynamicMedian) -and (Test-Finite $fixedMedian) -and [double]$fixedMedian -gt 0
if ($evidenceValid) {
    $delta = ([double]$dynamicMedian / [double]$fixedMedian - 1.0) * 100.0
    $absoluteGate = if ([double]$dynamicMedian -le 7.6632) { "PASS" } else { "FAIL" }
    $modeRegression = if ($delta -le 3.0) { "PASS" } else { "FAIL" }
    if ($absoluteGate -eq "PASS" -and $modeRegression -eq "PASS") {
        $decision = "HISTORICAL DYNAMIC GATE PASS"
    } elseif ($absoluteGate -eq "PASS") {
        $decision = "GATE PASS / DYNAMIC MODE OVERHEAD"
    } elseif ($modeRegression -eq "PASS") {
        $decision = "ABSOLUTE GATE FAIL / NO DYNAMIC-MODE REGRESSION"
    } else {
        $decision = "DYNAMIC-MODE REGRESSION"
    }
}

$summary = [ordered]@{
    schema_version = 1
    stage = "8G"
    suite = "PairedResolutionMode1080"
    run_id = $runId
    started_at = $startedAt.ToString("o")
    finished_at = (Get-Date).ToString("o")
    configured_runs_per_side = $Runs
    benchmark_seconds = $Seconds
    process_timeout_seconds = $ProcessTimeoutSeconds
    candidate_manifest_path = $manifestPath
    candidate = $manifest
    provenance_validation = $manifestValidation
    environment = [ordered]@{ before = $environmentBefore; after = $environmentAfter; validation = $environmentValidation }
    workloads = [ordered]@{
        common = @("--benchmark-seconds", "$Seconds", "--output-size", "1920x1080", "--command-recording-mode", "optimized", "--atrous-mode", "baseline", "--acceleration-structure-mode", "baseline")
        dynamic = @("--dynamic-resolution")
        fixed = @("--render-scale", "1.0")
    }
    order = @((Get-ResolutionSequence) | ForEach-Object { "$($_.side)-$($_.index)" })
    dynamic_runs = $dynamicRuns
    fixed_runs = $fixedRuns
    runs = $runRecords.ToArray()
    dynamic_p95_ms = $dynamicP95
    fixed_p95_ms = $fixedP95
    dynamic_median_p95_ms = $dynamicMedian
    fixed_median_p95_ms = $fixedMedian
    dynamic_vs_fixed_percent = $delta
    absolute_gate = $absoluteGate
    mode_regression = $modeRegression
    decision = $decision
    fatal_error = $fatalError
    passed = $absoluteGate -eq "PASS" -and $modeRegression -eq "PASS"
}
Save-Json $summaryPath $summary
Write-Output ([ordered]@{
    run_id = $runId
    summary = $summaryPath
    decision = $decision
    dynamic_median_p95_ms = $dynamicMedian
    fixed_median_p95_ms = $fixedMedian
    dynamic_vs_fixed_percent = $delta
    absolute_gate = $absoluteGate
    mode_regression = $modeRegression
    environment_valid = $environmentValidation.passed
    provenance_valid = $manifestValidation.passed
} | ConvertTo-Json -Compress)
if (-not $summary.passed) { exit 1 }
exit 0
