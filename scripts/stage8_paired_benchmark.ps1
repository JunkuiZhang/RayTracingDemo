[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$CandidateExe,
    [Parameter(Mandatory = $true)]
    [string]$ReferenceExe,
    [string]$OutputRoot = "output/stage8g",
    [string]$RunId,
    [ValidateRange(1, 10)]
    [int]$Runs = 3,
    [ValidateRange(1, 3600)]
    [int]$Seconds = 30
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Quote-Argument([string]$Value) {
    if ($Value -notmatch '[\s"]') { return $Value }
    return '"' + $Value.Replace('"', '\"') + '"'
}

function Save-Json([string]$Path, $Value) {
    $Value | ConvertTo-Json -Depth 50 | Set-Content -LiteralPath $Path -Encoding UTF8
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
    $driver = if ($null -ne $gpu) { [string]$gpu.DriverVersion } else { "N/A" }
    $driverSource = if ($null -ne $gpu) { "Win32_VideoController" } else { "unavailable" }
    if ($driver -eq "N/A") {
        try {
            $smi = @(& nvidia-smi --query-gpu=driver_version --format=csv,noheader,nounits 2>&1 |
                ForEach-Object { [string]$_ } |
                Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
            if ($smi.Count -gt 0 -and $smi[0] -notmatch 'not recognized|failed|error') {
                $driver = $smi[0].Trim()
                $driverSource = "nvidia-smi"
            }
        } catch { }
    }
    $power = "N/A"
    $powerSource = "unavailable"
    try {
        $powerOutput = @(& powercfg /getactivescheme 2>&1 | ForEach-Object { [string]$_ })
        if ($powerOutput.Count -gt 0) {
            $power = ($powerOutput -join " ").Trim()
            $powerSource = "powercfg /getactivescheme"
        }
    } catch { }
    $powerValue = "N/A"
    $powerProbe = "unavailable"
    try {
        $battery = Get-CimInstance Win32_Battery -ErrorAction Stop | Select-Object -First 1
        if ($null -ne $battery) {
            $powerProbe = "Win32_Battery.BatteryStatus"
            if ([int]$battery.BatteryStatus -eq 2) { $powerValue = "AC" }
            elseif ([int]$battery.BatteryStatus -in @(1, 3, 4, 5)) { $powerValue = "battery" }
        }
    } catch { }
    [pscustomobject]@{
        gpu_name = if ($null -ne $gpu) { [string]$gpu.Name } else { "N/A" }
        gpu_name_source = if ($null -ne $gpu) { "Win32_VideoController" } else { "benchmark_json_pending" }
        driver_version = $driver
        driver_version_source = $driverSource
        active_power_scheme = $power
        active_power_scheme_source = $powerSource
        power_source = $powerValue
        power_source_probe = $powerProbe
        gpu_probe_error = $gpuError
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
    $argsPath = Join-Path $CaseDirectory "$Label.args.json"
    $argumentString = ($Arguments | ForEach-Object { Quote-Argument $_ }) -join ' '
    $commandLine = "$(Quote-Argument $FilePath) $argumentString"
    Save-Json $argsPath ([ordered]@{ executable = $FilePath; args = $Arguments; command = $commandLine })
    $started = Get-Date
    $exitCode = -1
    $launchError = $null
    try {
        $process = Start-Process -FilePath $FilePath -ArgumentList $argumentString `
            -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath `
            -PassThru -WindowStyle Hidden
        $process.WaitForExit()
        $exitCode = $process.ExitCode
    } catch {
        $launchError = $_.Exception.Message
        Set-Content -LiteralPath $stderrPath -Value $launchError -Encoding UTF8
    }
    $finished = Get-Date
    $stdout = if (Test-Path -LiteralPath $stdoutPath) { Get-Content -LiteralPath $stdoutPath -Raw } else { "" }
    $stderr = if (Test-Path -LiteralPath $stderrPath) { Get-Content -LiteralPath $stderrPath -Raw } else { "" }
    $lines = @($stdout -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $json = $null
    $jsonError = $launchError
    if ($lines.Count -gt 0) {
        try { $json = $lines[-1] | ConvertFrom-Json -Depth 50; $jsonError = $null } catch { $jsonError = $_.Exception.Message }
    } elseif ($null -eq $jsonError) {
        $jsonError = "stdout has no non-empty JSON line"
    }
    [pscustomobject]@{
        label = $Label
        command = $commandLine
        args = $Arguments
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        args_path = $argsPath
        exit_code = $exitCode
        elapsed_seconds = ($finished - $started).TotalSeconds
        stdout_nonempty_lines = $lines.Count
        json = $json
        json_error = $jsonError
    }
}

function Test-PairedRun($Run) {
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($Run.exit_code -ne 0) { $failures.Add("exit_code=$($Run.exit_code)") }
    if ($Run.stdout_nonempty_lines -ne 1) { $failures.Add("stdout is not exactly one non-empty JSON line") }
    if ($null -eq $Run.json) { $failures.Add("JSON parse failed: $($Run.json_error)") }
    if ($null -ne $Run.json) {
        if ([string]$Run.json.resolution_mode -ne "fixed") { $failures.Add("resolution mode is not fixed") }
        if ([int]$Run.json.output_width -ne 1920 -or [int]$Run.json.output_height -ne 1080) { $failures.Add("output extent is not 1920x1080") }
        if ([int]$Run.json.render_width -ne 1920 -or [int]$Run.json.render_height -ne 1080) { $failures.Add("render extent is not 1920x1080") }
        if ($null -eq $Run.json.valid_samples -or [int64]$Run.json.valid_samples -le 0) { $failures.Add("valid_samples is not positive") }
        $total = $Run.json.passes.total
        if ($null -eq $total -or -not (Test-Finite $total.p50_ms) -or -not (Test-Finite $total.p95_ms)) {
            $failures.Add("Total p50/p95 is not finite")
        } elseif ([double]$total.p95_ms -lt [double]$total.p50_ms) {
            $failures.Add("Total p95 is below p50")
        } elseif ([double]$total.p95_ms -gt 16.67) {
            $failures.Add("Total p95 exceeds 16.67 ms")
        }
    }
    [pscustomobject]@{ passed = $failures.Count -eq 0; failures = $failures.ToArray() }
}

$repoRoot = (Get-Location).Path
$candidate = [IO.Path]::GetFullPath($CandidateExe)
$reference = [IO.Path]::GetFullPath($ReferenceExe)
if (-not (Test-Path -LiteralPath $candidate)) { throw "candidate executable not found: $candidate" }
if (-not (Test-Path -LiteralPath $reference)) { throw "reference executable not found: $reference" }
$resolvedOutputRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot $OutputRoot))
if ([string]::IsNullOrWhiteSpace($RunId)) {
    $runId = "paired-1080-{0}-{1}" -f (Get-Date -Format "yyyyMMdd-HHmmss"), ([guid]::NewGuid().ToString("N").Substring(0, 8))
} else {
    $runId = $RunId
}
$runRoot = Join-Path $resolvedOutputRoot $runId
New-Item -ItemType Directory -Path $runRoot -Force | Out-Null
$startedAt = Get-Date
$environment = Get-EnvironmentSnapshot
$head = (& git rev-parse HEAD).Trim()
Save-Json (Join-Path $runRoot "environment.json") $environment

$workload = @(
    "--benchmark-seconds", "$Seconds",
    "--output-size", "1920x1080",
    "--render-scale", "1.0",
    "--command-recording-mode", "optimized",
    "--atrous-mode", "baseline",
    "--acceleration-structure-mode", "baseline"
)
$sequence = @(
    [pscustomobject]@{ side = "candidate"; index = 1 },
    [pscustomobject]@{ side = "reference"; index = 1 },
    [pscustomobject]@{ side = "reference"; index = 2 },
    [pscustomobject]@{ side = "candidate"; index = 2 },
    [pscustomobject]@{ side = "candidate"; index = 3 },
    [pscustomobject]@{ side = "reference"; index = 3 }
)
$runs = [System.Collections.Generic.List[object]]::new()
$invalid = $false
foreach ($item in $sequence) {
    if ($invalid) { break }
    $exe = if ($item.side -eq "candidate") { $candidate } else { $reference }
    $caseDirectory = Join-Path $runRoot ("{0}-{1:D2}" -f $item.side, $item.index)
    New-Item -ItemType Directory -Path $caseDirectory -Force | Out-Null
    $run = Invoke-RecordedProcess $exe $workload $caseDirectory "run"
    $run | Add-Member -NotePropertyName side -NotePropertyValue $item.side
    $run | Add-Member -NotePropertyName index -NotePropertyValue $item.index
    $run | Add-Member -NotePropertyName validation -NotePropertyValue (Test-PairedRun $run)
    $runs.Add($run)
    if (-not $run.validation.passed) { $invalid = $true }
}

$candidateRuns = @($runs | Where-Object { $_.side -eq "candidate" -and $_.validation.passed })
$referenceRuns = @($runs | Where-Object { $_.side -eq "reference" -and $_.validation.passed })
$candidateP95 = @($candidateRuns | ForEach-Object { [double]$_.json.passes.total.p95_ms })
$referenceP95 = @($referenceRuns | ForEach-Object { [double]$_.json.passes.total.p95_ms })
$candidateMedian = Get-Median $candidateP95
$referenceMedian = Get-Median $referenceP95
$regression = $null
$absoluteGate = "INCONCLUSIVE"
$decision = "INCONCLUSIVE"
if (-not $invalid -and $candidateP95.Count -eq $Runs -and $referenceP95.Count -eq $Runs -and (Test-Finite $candidateMedian) -and (Test-Finite $referenceMedian) -and [double]$referenceMedian -gt 0) {
    $regression = ([double]$candidateMedian / [double]$referenceMedian - 1.0) * 100.0
    if ([double]$candidateMedian -le 7.6632) {
        $absoluteGate = "PASS"
        $decision = "HISTORICAL GATE PASS"
    } else {
        $absoluteGate = "FAIL"
        if ($regression -le 3.0) { $decision = "ABSOLUTE GATE FAIL / NO PAIRED REGRESSION" }
        else { $decision = "REAL REGRESSION" }
    }
}

$summary = [ordered]@{
    schema_version = 1
    stage = "8G"
    suite = "Paired1080"
    git_head = $head
    run_id = $runId
    started_at = $startedAt.ToString("o")
    finished_at = (Get-Date).ToString("o")
    environment = $environment
    candidate_exe = $candidate
    reference_exe = $reference
    workload = $workload
    order = @($sequence | ForEach-Object { "{0}-{1}" -f $_.side, $_.index })
    candidate_runs = $candidateRuns
    reference_runs = $referenceRuns
    runs = $runs.ToArray()
    candidate_p95_ms = $candidateP95
    reference_p95_ms = $referenceP95
    candidate_median_p95_ms = $candidateMedian
    reference_median_p95_ms = $referenceMedian
    candidate_vs_reference_percent = $regression
    absolute_gate = $absoluteGate
    decision = $decision
    passed = $decision -ne "REAL REGRESSION" -and $decision -ne "INCONCLUSIVE"
}
Save-Json (Join-Path $runRoot "summary.json") $summary
Write-Output ([ordered]@{ run_id = $runId; summary = (Join-Path $runRoot "summary.json"); decision = $decision; candidate_median_p95_ms = $candidateMedian; reference_median_p95_ms = $referenceMedian; candidate_vs_reference_percent = $regression } | ConvertTo-Json -Compress)
if ($decision -eq "REAL REGRESSION" -or $decision -eq "INCONCLUSIVE") { exit 1 }
exit 0
