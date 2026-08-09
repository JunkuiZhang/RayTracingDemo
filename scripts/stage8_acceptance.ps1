[CmdletBinding()]
param(
    [ValidateSet("Debug", "Release")]
    [string]$Build = "Release",
    [string]$Exe,
    [string]$OutputRoot = "output/stage8g",
    [ValidateRange(1, 10)]
    [int]$Runs = 3,
    [ValidateRange(1, 3600)]
    [int]$Seconds = 30,
    [ValidateRange(1, 7200)]
    [int]$LongRunSeconds = 1800,
    [string]$LargeModel,
    [string]$PbrModel,
    [switch]$SkipLongRun,
    [switch]$SkipCaptures,
    [switch]$SmokeOnly
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
    $middle = [int]($numbers.Count / 2)
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
    try {
        $gpu = Get-CimInstance Win32_VideoController -ErrorAction Stop |
            Where-Object { $_.Name -match 'NVIDIA' } |
            Select-Object -First 1
    } catch { }
    $power = $null
    try { $power = (powercfg /getactivescheme 2>$null | Out-String).Trim() } catch { }
    $battery = $null
    try { $battery = Get-CimInstance Win32_Battery -ErrorAction Stop | Select-Object -First 1 } catch { }
    [pscustomobject]@{
        gpu_name = if ($null -ne $gpu) { $gpu.Name } else { "N/A" }
        driver_version = if ($null -ne $gpu) { $gpu.DriverVersion } else { "N/A" }
        active_power_scheme = if ([string]::IsNullOrWhiteSpace($power)) { "N/A" } else { $power }
        power_source = if ($null -ne $battery) {
            if ($battery.BatteryStatus -eq 2) { "AC" } else { "battery" }
        } else { "N/A" }
    }
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
    $started = Get-Date
    $process = Start-Process -FilePath $FilePath -ArgumentList $argumentString `
        -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath `
        -PassThru -WindowStyle Hidden
    $process.WaitForExit()
    $finished = Get-Date
    $stdout = if (Test-Path -LiteralPath $stdoutPath) { Get-Content -LiteralPath $stdoutPath -Raw } else { "" }
    $stderr = if (Test-Path -LiteralPath $stderrPath) { Get-Content -LiteralPath $stderrPath -Raw } else { "" }
    $lines = @($stdout -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $json = $null
    $jsonError = $null
    if ($lines.Count -gt 0) {
        try { $json = $lines[-1] | ConvertFrom-Json -Depth 40 } catch { $jsonError = $_.Exception.Message }
    } else {
        $jsonError = "stdout has no non-empty JSON line"
    }
    [pscustomobject]@{
        label = $Label
        command = $commandLine
        args = $Arguments
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        exit_code = $process.ExitCode
        elapsed_seconds = ($finished - $started).TotalSeconds
        stdout_nonempty_lines = $lines.Count
        json = $json
        json_error = $jsonError
        stderr_nonempty = -not [string]::IsNullOrWhiteSpace($stderr)
    }
}

function Test-BenchmarkResult($Run) {
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
    }
    if ([string]$Run.json.resolution_mode -eq "dynamic" -and $null -ne $Run.json.dynamic_resolution) {
        $dynamic = $Run.json.dynamic_resolution.measurement
        if ($null -ne $dynamic) {
            $expected = [int64]$dynamic.valid_samples + [int64]$dynamic.stale_generation_samples_ignored
            if ([int64]$Run.json.valid_samples -ne $expected) {
                $failures.Add("dynamic epoch mismatch total=$($Run.json.valid_samples) controller=$expected")
            }
        }
    }
    [pscustomobject]@{ passed = ($failures.Count -eq 0); failures = $failures.ToArray() }
}

function Get-CaseSummary($Name, [object[]]$RunsInCase) {
    $validRuns = @($RunsInCase | Where-Object { $_.validation.passed -and $null -ne $_.json })
    $passNames = @("total", "acceleration_structure", "path_trace", "temporal", "atrous", "atrous_0", "atrous_1", "atrous_2", "atrous_3", "tone_map")
    $medians = [ordered]@{}
    foreach ($pass in $passNames) {
        $medians[$pass] = [ordered]@{
            p50_ms = Get-Median @($validRuns | ForEach-Object { $_.json.passes.$pass.p50_ms })
            p95_ms = Get-Median @($validRuns | ForEach-Object { $_.json.passes.$pass.p95_ms })
        }
    }
    [pscustomobject]@{
        name = $Name
        run_count = $RunsInCase.Count
        valid_run_count = $validRuns.Count
        passed = ($RunsInCase.Count -gt 0 -and $validRuns.Count -eq $RunsInCase.Count)
        medians = $medians
        runs = $RunsInCase
    }
}

function Invoke-BenchmarkCase(
    [string]$Name,
    [string[]]$ExtraArguments,
    [int]$Count,
    [int]$Duration,
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
        $run | Add-Member -NotePropertyName validation -NotePropertyValue (Test-BenchmarkResult $run)
        $caseRuns.Add($run)
        $AllRuns.Add($run)
    }
    return Get-CaseSummary $Name $caseRuns.ToArray()
}

function Save-Summary($Path, $Summary) {
    $Summary | ConvertTo-Json -Depth 40 | Set-Content -LiteralPath $Path -Encoding UTF8
}

function Invoke-CaptureCase(
    [string]$Name,
    [string[]]$ExtraArguments,
    [string]$Root,
    [string]$Executable
) {
    $captureDirectory = Join-Path $Root "captures"
    New-Item -ItemType Directory -Path $captureDirectory -Force | Out-Null
    $pngPath = Join-Path $captureDirectory "$Name.png"
    $arguments = @("--capture-output", $pngPath, "--capture-after-spp", "128", "--output-size", "1280x720") + $ExtraArguments
    $run = Invoke-RecordedProcess $Executable $arguments $captureDirectory $Name
    $valid = $run.exit_code -eq 0 -and $null -ne $run.json -and (Test-Path -LiteralPath $pngPath)
    $error = $null
    if (-not $valid) {
        $error = if ($null -eq $run.json) { $run.json_error } else { "capture exit=$($run.exit_code) png_exists=$(Test-Path -LiteralPath $pngPath)" }
    } elseif ([int]$run.json.actual_spp -lt 128) {
        $error = "capture SPP is below requested 128"
        $valid = $false
    }
    [pscustomobject]@{
        name = $Name
        passed = $valid
        png_path = $pngPath
        json = $run.json
        run = $run
        error = $error
    }
}

function Invoke-DiffCase(
    [string]$Name,
    [string]$Left,
    [string]$Right,
    [string]$Root,
    [string]$ImageDiffExecutable
) {
    $diffDirectory = Join-Path $Root "diffs"
    New-Item -ItemType Directory -Path $diffDirectory -Force | Out-Null
    $run = Invoke-RecordedProcess $ImageDiffExecutable @($Left, $Right) $diffDirectory $Name
    $passed = $run.exit_code -eq 0 -and $null -ne $run.json
    [pscustomobject]@{
        name = $Name
        passed = $passed
        json = $run.json
        run = $run
    }
}

$repoRoot = (Get-Location).Path
$resolvedOutputRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot $OutputRoot))
$runId = "{0}-{1}" -f (Get-Date -Format "yyyyMMdd-HHmmss"), ([guid]::NewGuid().ToString("N").Substring(0, 8))
$runRoot = Join-Path $resolvedOutputRoot $runId
New-Item -ItemType Directory -Path $runRoot -Force | Out-Null

if ([string]::IsNullOrWhiteSpace($Exe)) {
    $configuration = $Build.ToLowerInvariant()
    $Exe = Join-Path $repoRoot "target/$configuration/ray_tracing_demo.exe"
}
$Executable = [IO.Path]::GetFullPath($Exe)
if (-not (Test-Path -LiteralPath $Executable)) { throw "executable not found: $Executable" }

$environment = Get-EnvironmentSnapshot
$head = (& git rev-parse HEAD).Trim()
$allRuns = [System.Collections.Generic.List[object]]::new()
$caseSummaries = [System.Collections.Generic.List[object]]::new()
$startedAt = Get-Date

$smoke = Invoke-BenchmarkCase "smoke-forced-dynamic" @("--output-size", "1280x720", "--dynamic-resolution", "--target-gpu-ms", "4") 1 $Seconds $runRoot $Executable $allRuns
$caseSummaries.Add($smoke)
$smokePassed = $smoke.passed
if ($environment.gpu_name -eq "N/A" -and $smoke.runs.Count -gt 0 -and $null -ne $smoke.runs[0].json) {
    $environment.gpu_name = [string]$smoke.runs[0].json.gpu_name
}

$summary = [ordered]@{
    schema_version = 1
    stage = "8G"
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

$fixedSizes = @(@("1280x720", "1280", "720"), @("1600x900", "1600", "900"), @("1920x1080", "1920", "1080"))
foreach ($size in $fixedSizes) {
    $caseSummaries.Add((Invoke-BenchmarkCase "fixed-$($size[0])" @("--output-size", $size[0], "--render-scale", "1.0") $Runs $Seconds $runRoot $Executable $allRuns))
}
foreach ($scale in @("0.83", "0.75", "0.67")) {
    $caseSummaries.Add((Invoke-BenchmarkCase "fixed-scale-$scale" @("--output-size", "1280x720", "--render-scale", $scale) 1 $Seconds $runRoot $Executable $allRuns))
}
$caseSummaries.Add((Invoke-BenchmarkCase "dynamic-default" @("--output-size", "1280x720", "--dynamic-resolution") $Runs $Seconds $runRoot $Executable $allRuns))

if (-not [string]::IsNullOrWhiteSpace($PbrModel)) {
    $modelPath = [IO.Path]::GetFullPath($PbrModel)
    if (-not (Test-Path -LiteralPath $modelPath)) { throw "PbrModel not found: $modelPath" }
    $caseSummaries.Add((Invoke-BenchmarkCase "pbr-static" @("--output-size", "1280x720", "--model", $modelPath) 1 $Seconds $runRoot $Executable $allRuns))
    $caseSummaries.Add((Invoke-BenchmarkCase "pbr-animated" @("--output-size", "1280x720", "--model", $modelPath, "--animate-model") 1 $Seconds $runRoot $Executable $allRuns))
}
if (-not [string]::IsNullOrWhiteSpace($LargeModel)) {
    $largePath = [IO.Path]::GetFullPath($LargeModel)
    if (-not (Test-Path -LiteralPath $largePath)) { throw "LargeModel not found: $largePath" }
    $caseSummaries.Add((Invoke-BenchmarkCase "large-static" @("--output-size", "1280x720", "--model", $largePath) 1 $Seconds $runRoot $Executable $allRuns))
}

if (-not $SkipLongRun) {
    $longCase = Invoke-BenchmarkCase "forced-dynamic-long" @("--output-size", "1280x720", "--dynamic-resolution", "--target-gpu-ms", "4") 1 $LongRunSeconds $runRoot $Executable $allRuns
    $summary.long_run = $longCase
    $caseSummaries.Add($longCase)
}

$captureRecords = [System.Collections.Generic.List[object]]::new()
$diffRecords = [System.Collections.Generic.List[object]]::new()
if (-not $SkipCaptures) {
    $views = @("final", "raw", "albedo", "normal-roughness", "depth", "motion", "variance", "history-rejection", "history-length", "object-material-id", "specular-hit-distance")
    foreach ($recordingMode in @("baseline", "optimized")) {
        foreach ($view in $views) {
            $captureRecords.Add((Invoke-CaptureCase "command-$recordingMode-$view" @("--command-recording-mode", $recordingMode) $runRoot $Executable))
        }
    }
    foreach ($accelerationMode in @("baseline", "optimized")) {
        foreach ($view in $views) {
            $captureRecords.Add((Invoke-CaptureCase "as-$accelerationMode-$view" @("--acceleration-structure-mode", $accelerationMode) $runRoot $Executable))
        }
    }
    foreach ($atrousMode in @("baseline", "shared")) {
        $captureRecords.Add((Invoke-CaptureCase "atrous-$atrousMode-final" @("--atrous-mode", $atrousMode) $runRoot $Executable))
    }
    foreach ($scale in @("0.83", "0.75", "0.67")) {
        $captureRecords.Add((Invoke-CaptureCase "scale-$scale-final" @("--render-scale", $scale) $runRoot $Executable))
    }
    foreach ($resolutionArguments in @(
        @("dynamic-default", "--dynamic-resolution"),
        @("dynamic-forced", "--dynamic-resolution", "--target-gpu-ms", "4")
    )) {
        $captureRecords.Add((Invoke-CaptureCase "$($resolutionArguments[0])-final" @($resolutionArguments[1..($resolutionArguments.Count - 1)]) $runRoot $Executable))
        $captureRecords.Add((Invoke-CaptureCase "$($resolutionArguments[0])-history-length" (@($resolutionArguments[1..($resolutionArguments.Count - 1)]) + @("--debug-view", "history-length")) $runRoot $Executable))
    }
    $imageDiffExecutable = Join-Path $repoRoot "target/release/image_diff.exe"
    if (Test-Path -LiteralPath $imageDiffExecutable) {
        $commandBaseline = ($captureRecords | Where-Object { $_.name -eq "command-baseline-final" }).png_path
        $commandOptimized = ($captureRecords | Where-Object { $_.name -eq "command-optimized-final" }).png_path
        $atrousBaseline = ($captureRecords | Where-Object { $_.name -eq "atrous-baseline-final" }).png_path
        $atrousShared = ($captureRecords | Where-Object { $_.name -eq "atrous-shared-final" }).png_path
        if ($null -ne $commandBaseline -and $null -ne $commandOptimized) {
            $diffRecords.Add((Invoke-DiffCase "command-baseline-vs-optimized" $commandBaseline $commandOptimized $runRoot $imageDiffExecutable))
        }
        if ($null -ne $atrousBaseline -and $null -ne $atrousShared) {
            $diff = Invoke-DiffCase "atrous-baseline-vs-shared" $atrousBaseline $atrousShared $runRoot $imageDiffExecutable
            if ($diff.passed -and ($diff.json.max_channel_abs_diff -gt 1 -or $diff.json.rmse -gt 0.10)) { $diff.passed = $false }
            $diffRecords.Add($diff)
        }
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
