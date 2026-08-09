[CmdletBinding()]
param(
    [string]$Exe = "target/debug/ray_tracing_demo.exe",
    [string]$OutputRoot = "output/stage8g",
    [ValidateRange(1, 5)]
    [int]$Seconds = 1,
    [ValidateRange(120, 300)]
    [int]$ProcessTimeoutSeconds = 180,
    [switch]$SelfTest
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

    $powerSourceValue = "N/A"
    $powerProbe = "unavailable"
    try {
        if (-not ("Stage8Validation.DebugPowerStatus" -as [type])) {
            Add-Type -TypeDefinition @"
using System.Runtime.InteropServices;
namespace Stage8Validation {
    [StructLayout(LayoutKind.Sequential)]
    public struct DebugPowerStatus {
        public byte ACLineStatus;
        public byte BatteryFlag;
        public byte BatteryLifePercent;
        public byte Reserved;
        public int BatteryLifeTime;
        public int BatteryFullLifeTime;
    }
    public static class DebugPowerStatusApi {
        [DllImport("kernel32.dll")]
        public static extern bool GetSystemPowerStatus(out DebugPowerStatus status);
    }
}
"@
        }
        $status = New-Object Stage8Validation.DebugPowerStatus
        if ([Stage8Validation.DebugPowerStatusApi]::GetSystemPowerStatus([ref]$status)) {
            $powerProbe = "GetSystemPowerStatus"
            if ($status.ACLineStatus -eq 1) { $powerSourceValue = "AC" }
            elseif ($status.ACLineStatus -eq 0) { $powerSourceValue = "battery" }
        }
    } catch { }
    if ($powerSourceValue -eq "N/A") {
        try {
            $battery = Get-CimInstance Win32_Battery -ErrorAction Stop | Select-Object -First 1
            if ($null -ne $battery) {
                $powerProbe = "Win32_Battery.BatteryStatus"
                if ([int]$battery.BatteryStatus -eq 2) { $powerSourceValue = "AC" }
                elseif ([int]$battery.BatteryStatus -in @(1, 3, 4, 5)) { $powerSourceValue = "battery" }
            }
        } catch { }
    }

    [pscustomobject]@{
        gpu_name = if ($null -ne $gpu) { [string]$gpu.Name } else { "N/A" }
        gpu_name_source = if ($null -ne $gpu) { "Win32_VideoController" } else { "benchmark_json_pending" }
        driver_version = $driver
        driver_version_source = $driverSource
        active_power_scheme = $power
        active_power_scheme_source = $powerSource
        power_source = $powerSourceValue
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
    $timedOut = $false
    $launchError = $null
    try {
        $process = Start-Process -FilePath $FilePath -ArgumentList $argumentString `
            -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath `
            -PassThru -WindowStyle Hidden
        if (-not $process.WaitForExit($ProcessTimeoutSeconds * 1000)) {
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
    $jsonError = if ($timedOut) { "process timed out after $ProcessTimeoutSeconds seconds" } else { $launchError }
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
        timed_out = $timedOut
        timeout_seconds = $ProcessTimeoutSeconds
        elapsed_seconds = ($finished - $started).TotalSeconds
        stdout_nonempty_lines = $lines.Count
        json = $json
        json_error = $jsonError
        stderr_nonempty = -not [string]::IsNullOrWhiteSpace($stderr)
    }
}

function Test-DebugStderr($Run) {
    $failures = [System.Collections.Generic.List[string]]::new()
    $stderr = if (-not [string]::IsNullOrWhiteSpace([string]$Run.stderr_path) -and (Test-Path -LiteralPath $Run.stderr_path)) { Get-Content -LiteralPath $Run.stderr_path -Raw } else { "" }
    if ($stderr -notmatch 'D3D12 Debug InfoQueue：0 条消息') {
        $failures.Add("stderr does not contain a zero-message InfoQueue conclusion")
    }
    foreach ($pattern in @('device removed', 'panic', 'resource state', 'as lifetime', 'readback (error|footprint|failed)', 'descriptor (error|failed|invalid)', 'fence (error|failed|invalid)')) {
        if ($stderr -match $pattern) { $failures.Add("stderr contains forbidden validation marker: $pattern") }
    }
    return $failures
}

function Test-Benchmark($Run) {
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($Run.timed_out) { $failures.Add("process timed out") }
    if ($Run.exit_code -ne 0) { $failures.Add("exit_code=$($Run.exit_code)") }
    if ($Run.stdout_nonempty_lines -ne 1) { $failures.Add("stdout is not exactly one non-empty JSON line") }
    if ($null -eq $Run.json) { $failures.Add("JSON parse failed: $($Run.json_error)") }
    if ($null -ne $Run.json) {
        if ([string]::IsNullOrWhiteSpace([string]$Run.json.gpu_name)) { $failures.Add("gpu_name missing") }
        if ($null -eq $Run.json.valid_samples -or [int64]$Run.json.valid_samples -le 0) { $failures.Add("valid_samples is not positive") }
    }
    foreach ($failure in @(Test-DebugStderr $Run)) { $failures.Add($failure) }
    return [pscustomobject]@{ passed = $failures.Count -eq 0; failures = $failures.ToArray() }
}

function Get-PngExtent([string]$Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 24 -or ([BitConverter]::ToString($bytes[0..7]) -ne "89-50-4E-47-0D-0A-1A-0A")) {
        throw "invalid PNG signature or too-short PNG"
    }
    [pscustomobject]@{
        width = ([int]$bytes[16] * 16777216) + ([int]$bytes[17] * 65536) + ([int]$bytes[18] * 256) + [int]$bytes[19]
        height = ([int]$bytes[20] * 16777216) + ([int]$bytes[21] * 65536) + ([int]$bytes[22] * 256) + [int]$bytes[23]
    }
}

function Test-Capture($Run, [string]$PngPath) {
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($Run.timed_out) { $failures.Add("capture process timed out") }
    if ($Run.exit_code -ne 0) { $failures.Add("capture exit_code=$($Run.exit_code)") }
    if ($Run.stdout_nonempty_lines -ne 1) { $failures.Add("capture stdout is not exactly one JSON line") }
    if ($null -eq $Run.json) { $failures.Add("capture JSON parse failed: $($Run.json_error)") }
    if (-not (Test-Path -LiteralPath $PngPath)) { $failures.Add("capture PNG is missing") }
    if ($null -ne $Run.json) {
        if ([int]$Run.json.actual_spp -ne 8) { $failures.Add("capture actual_spp is not 8") }
        if ([string]$Run.json.debug_view.name -ne "object-material-id") { $failures.Add("capture debug view differs") }
        if ([int]$Run.json.output_width -ne 320 -or [int]$Run.json.output_height -ne 180) { $failures.Add("capture output extent differs") }
        if ([string]$Run.json.modes.acceleration_structure -ne "optimized" -or [string]$Run.json.modes.command_recording -ne "optimized" -or [string]$Run.json.modes.atrous -ne "baseline") { $failures.Add("capture modes differ") }
        if ([int64]$Run.json.png_bytes -le 0) { $failures.Add("capture png_bytes is not positive") }
    }
    if (Test-Path -LiteralPath $PngPath) {
        try {
            $extent = Get-PngExtent $PngPath
            if ($extent.width -ne 320 -or $extent.height -ne 180) { $failures.Add("PNG IHDR extent differs") }
        } catch { $failures.Add("PNG validation failed: $($_.Exception.Message)") }
    }
    foreach ($failure in @(Test-DebugStderr $Run)) { $failures.Add($failure) }
    return [pscustomobject]@{
        passed = $failures.Count -eq 0
        failures = $failures.ToArray()
        png_path = $PngPath
        sha256 = if (Test-Path -LiteralPath $PngPath) { (Get-FileHash -LiteralPath $PngPath -Algorithm SHA256).Hash } else { $null }
    }
}

if ($SelfTest) {
    $fakeRun = [pscustomobject]@{
        timed_out = $true
        exit_code = -1
        stdout_nonempty_lines = 0
        json = $null
        json_error = "timeout"
        stderr_path = ""
    }
    $validation = Test-Benchmark $fakeRun
    if ($validation.passed -or $validation.failures -notcontains "process timed out") {
        throw "Debug timeout self-test failed"
    }
    [ordered]@{ self_test = "passed"; cases = 1 } | ConvertTo-Json -Compress
    exit 0
}

$repoRoot = (Get-Location).Path
$resolvedExe = [IO.Path]::GetFullPath((Join-Path $repoRoot $Exe))
if (-not (Test-Path -LiteralPath $resolvedExe)) { throw "Debug executable not found: $resolvedExe" }
$resolvedOutputRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot $OutputRoot))
$runId = "debug-{0}-{1}" -f (Get-Date -Format "yyyyMMdd-HHmmss"), ([guid]::NewGuid().ToString("N").Substring(0, 8))
$runRoot = Join-Path $resolvedOutputRoot $runId
New-Item -ItemType Directory -Path $runRoot -Force | Out-Null
$startedAt = Get-Date
$environment = Get-EnvironmentSnapshot
$head = (& git rev-parse HEAD).Trim()
Save-Json (Join-Path $runRoot "environment.json") $environment

$commands = [System.Collections.Generic.List[object]]::new()
$benchmarkCases = @(
    [pscustomobject]@{ name = "cornell-baseline"; args = @("--benchmark-seconds", "$Seconds", "--output-size", "320x180", "--command-recording-mode", "baseline", "--atrous-mode", "baseline", "--acceleration-structure-mode", "baseline") },
    [pscustomobject]@{ name = "cornell-forced-dynamic"; args = @("--benchmark-seconds", "$Seconds", "--output-size", "320x180", "--dynamic-resolution", "--target-gpu-ms", "4", "--command-recording-mode", "optimized", "--atrous-mode", "baseline", "--acceleration-structure-mode", "baseline") },
    [pscustomobject]@{ name = "gltf-static-optimized"; args = @("--model", (Resolve-Path "assets/gltf/Triangle/NonIndexedMultiNode.gltf").Path, "--benchmark-seconds", "$Seconds", "--output-size", "320x180", "--acceleration-structure-mode", "optimized") },
    [pscustomobject]@{ name = "gltf-animated-optimized"; args = @("--model", (Resolve-Path "assets/gltf/Triangle/NonIndexedMultiNode.gltf").Path, "--animate-model", "--benchmark-seconds", "$Seconds", "--output-size", "320x180", "--acceleration-structure-mode", "optimized") }
)
foreach ($case in $benchmarkCases) {
    $caseDirectory = Join-Path $runRoot $case.name
    New-Item -ItemType Directory -Path $caseDirectory -Force | Out-Null
    $run = Invoke-RecordedProcess $resolvedExe $case.args $caseDirectory "run"
    $run | Add-Member -NotePropertyName validation -NotePropertyValue (Test-Benchmark $run)
    $commands.Add([pscustomobject]@{ name = $case.name; kind = "benchmark"; run = $run })
}

$captureDirectory = Join-Path $runRoot "gltf-capture"
New-Item -ItemType Directory -Path $captureDirectory -Force | Out-Null
$capturePath = Join-Path $captureDirectory "debug-gltf-as-capture.png"
$captureArgs = @(
    "--model", (Resolve-Path "assets/gltf/Triangle/NonIndexedMultiNode.gltf").Path,
    "--acceleration-structure-mode", "optimized",
    "--capture-output", $capturePath,
    "--capture-after-spp", "8",
    "--output-size", "320x180",
    "--debug-view", "object-material-id"
)
$captureRun = Invoke-RecordedProcess $resolvedExe $captureArgs $captureDirectory "capture"
$captureRun | Add-Member -NotePropertyName validation -NotePropertyValue (Test-Capture $captureRun $capturePath)
$commands.Add([pscustomobject]@{ name = "gltf-capture"; kind = "capture"; run = $captureRun })

$summary = [ordered]@{
    schema_version = 1
    stage = "8G"
    suite = "DebugValidation"
    git_head = $head
    run_id = $runId
    started_at = $startedAt.ToString("o")
    finished_at = (Get-Date).ToString("o")
    environment = $environment
    executable = $resolvedExe
    benchmark_seconds = $Seconds
    commands = $commands.ToArray()
    passed = (@($commands | Where-Object { -not $_.run.validation.passed }).Count -eq 0)
}
Save-Json (Join-Path $runRoot "summary.json") $summary
Write-Output ([ordered]@{ run_id = $runId; summary = (Join-Path $runRoot "summary.json"); passed = $summary.passed } | ConvertTo-Json -Compress)
if (-not $summary.passed) { exit 1 }
exit 0
