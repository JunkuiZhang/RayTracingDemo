[CmdletBinding()]
param(
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Release",
    [string]$Exe,
    [ValidateSet("Smoke", "Matrix", "DebugValidation")]
    [string]$Suite = "Smoke",
    [ValidateSet("", "native_svgf", "dlaa_svgf", "quality_svgf", "balanced_svgf", "performance_svgf", "quality_nrd")]
    [string]$CaseName = "",
    [string]$NrdExe,
    [ValidateRange(1, 3)]
    [int]$Runs = 1,
    [ValidateRange(1, 30)]
    [int]$Seconds = 1,
    [ValidateRange(10, 180)]
    [int]$TimeoutSeconds = 60,
    [uint32]$StreamlineApplicationId = 0,
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

function New-RecordedProcessContext(
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
    [pscustomobject]@{
        label = $Label
        executable = $Executable
        arguments = $Arguments
        command = $command
        args_path = $argsPath
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        exit_path = $exitPath
    }
}

function Complete-RecordedProcess(
    [object]$Context,
    [int]$ExitCode,
    [bool]$TimedOut,
    [double]$ElapsedSeconds,
    [AllowNull()][string]$LaunchError
) {
    $stdout = if (Test-Path -LiteralPath $Context.stdout_path) {
        Get-Content -LiteralPath $Context.stdout_path -Raw
    } else { "" }
    $lines = @($stdout -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $json = $null
    $jsonError = $LaunchError
    if ($TimedOut) { $jsonError = "timeout after $TimeoutSeconds seconds" }
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
        label = $Context.label
        command = $Context.command
        args_path = $Context.args_path
        stdout_path = $Context.stdout_path
        stderr_path = $Context.stderr_path
        exit_path = $Context.exit_path
        exit_code = $ExitCode
        timed_out = $TimedOut
        elapsed_seconds = $ElapsedSeconds
        stdout_nonempty_lines = $lines.Count
        parseable_json_lines = $jsonCandidates.Count
        json = $json
        json_error = $jsonError
    } | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $Context.exit_path -Encoding UTF8
    [pscustomobject]@{
        label = $Context.label
        command = $Context.command
        args_path = $Context.args_path
        stdout_path = $Context.stdout_path
        stderr_path = $Context.stderr_path
        exit_path = $Context.exit_path
        exit_code = $ExitCode
        timed_out = $TimedOut
        elapsed_seconds = $ElapsedSeconds
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

function Get-Stage10GateFailures(
    [object]$Result,
    [hashtable]$Case,
    [string]$BuildConfiguration
) {
    $gateFailures = [System.Collections.Generic.List[string]]::new()
    if ($Result.exit_code -ne 0 -or $Result.timed_out -or
        $Result.parseable_json_lines -ne 1 -or $null -eq $Result.json) {
        return @($gateFailures)
    }

    try {
        $json = $Result.json
        $expectedUpscaler = [string]$Case.upscaler
        if ([string]$json.upscaler.mode -ne $expectedUpscaler) {
            $gateFailures.Add("upscaler mode mismatch: expected=$expectedUpscaler actual=$($json.upscaler.mode)")
        }
        if ([string]$json.gpu_name -notmatch 'RTX 4060 Laptop') {
            $gateFailures.Add("stage 10 target GPU mismatch: actual=$($json.gpu_name)")
        }
        if ([uint32]$json.output_width -ne 1920 -or [uint32]$json.output_height -ne 1080) {
            $gateFailures.Add("output extent must be 1920x1080")
        }
        if ([uint64]$json.gpu_idle_wait_count -ne 0) {
            $gateFailures.Add("gpu_idle_wait_count must be zero, actual=$($json.gpu_idle_wait_count)")
        }
        if ([uint64]$json.valid_samples -eq 0 -or $null -eq $json.passes.total.p95_ms) {
            $gateFailures.Add("GPU Total must contain valid timing samples")
        }

        $usageRatio = if ($null -ne $json.memory.measurement.peak_usage_ratio) {
            $json.memory.measurement.peak_usage_ratio
        } else { $json.memory.usage_ratio }
        if ($null -eq $usageRatio) {
            $gateFailures.Add("local VRAM usage_ratio is unavailable")
        } elseif ([double]$usageRatio -ge 0.70) {
            $gateFailures.Add("local VRAM usage_ratio must be below 0.70, actual=$usageRatio")
        }

        $usesDlss = $expectedUpscaler -ne "native"
        if ($usesDlss) {
            if ($null -eq $json.upscaler.dlss_optimal) {
                $gateFailures.Add("active DLSS mode must report optimal settings")
            } else {
                if ([uint32]$json.render_width -ne [uint32]$json.upscaler.dlss_optimal.optimal_render_width -or
                    [uint32]$json.render_height -ne [uint32]$json.upscaler.dlss_optimal.optimal_render_height) {
                    $gateFailures.Add("render extent does not match Streamline optimal settings")
                }
            }
            foreach ($passName in @("dlss_compose", "dlss_evaluate")) {
                $pass = $json.passes.$passName
                if ($null -eq $pass.p95_ms -or [uint64]$pass.valid_samples -eq 0) {
                    $gateFailures.Add("$passName must contain non-null GPU timing samples")
                }
            }
            if ($BuildConfiguration -eq "Release" -and [double]$json.passes.total.p95_ms -gt 16.67) {
                $gateFailures.Add("DLSS GPU Total p95 exceeds 16.67 ms: $($json.passes.total.p95_ms)")
            }
        } else {
            foreach ($passName in @("dlss_compose", "dlss_evaluate")) {
                $pass = $json.passes.$passName
                if ($null -ne $pass.p95_ms -or [uint64]$pass.valid_samples -ne 0) {
                    $gateFailures.Add("Native mode must keep $passName inactive")
                }
            }
        }

        if ([bool]$json.reflex.compiled) {
            $tokenCount = [uint64]$json.reflex.token_count
            if ($tokenCount -eq 0) {
                $gateFailures.Add("Streamline frame token count must be nonzero")
            }
            if ([uint64]$json.reflex.present_common_count -ne $tokenCount) {
                $gateFailures.Add("presentCommon count must equal frame token count")
            }
            if ([uint64]$json.reflex.order_errors -ne 0) {
                $gateFailures.Add("Reflex/PCL marker order_errors must be zero")
            }
            if ([bool]$json.reflex.support.reflex -and [string]$json.reflex.active_mode -ne "off" -and
                [uint64]$json.reflex.sleep_count -ne $tokenCount) {
                $gateFailures.Add("Reflex sleep count must equal frame token count when enabled")
            }
            if ([bool]$json.reflex.support.pcl) {
                foreach ($markerName in @(
                    "simulation_start", "simulation_end", "render_submit_start",
                    "render_submit_end", "present_start", "present_end")) {
                    if ([uint64]$json.reflex.marker_counts.$markerName -ne $tokenCount) {
                        $gateFailures.Add("PCL marker $markerName must equal frame token count")
                    }
                }
            }
        }

        if ($BuildConfiguration -eq "Debug") {
            $stderr = if (Test-Path -LiteralPath $Result.stderr_path) {
                Get-Content -LiteralPath $Result.stderr_path -Raw
            } else { "" }
            $emptyInfoQueue = $stderr -match 'D3D12 Debug InfoQueue[^\r\n]*0\s*条消息'
            $zeroSeverities = $stderr -match 'CORRUPTION\s+0.*ERROR\s+0'
            if (-not ($emptyInfoQueue -or $zeroSeverities)) {
                $gateFailures.Add("Debug InfoQueue must report CORRUPTION 0 and ERROR 0")
            }
        }
    } catch {
        $gateFailures.Add("benchmark JSON schema/gate evaluation failed: $($_.Exception.Message)")
    }
    return @($gateFailures)
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

$caseSuite = if ([string]::IsNullOrWhiteSpace($CaseName)) { $Suite } else { "Matrix" }
$cases = Get-Cases $caseSuite
if (($caseSuite -eq "Matrix") -and $null -ne $nrdExecutable) {
    $cases += @{ label = "quality_nrd"; upscaler = "dlss-quality"; denoiser = "nrd-reblur"; executable = $nrdExecutable }
}
if (-not [string]::IsNullOrWhiteSpace($CaseName)) {
    $selectedCases = @($cases | Where-Object { $_.label -eq $CaseName })
    if ($selectedCases.Count -ne 1) {
        throw "case '$CaseName' is unavailable; quality_nrd requires -NrdExe"
    }
    $cases = $selectedCases[0]
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
        # Keep the DLSS quality/performance matrix isolated from latency pacing.
        # Reflex-on is covered separately by Smoke and DebugValidation.
        if ($Suite -eq "Matrix") {
            $arguments += @("--reflex-mode", "off")
        }
        if ($StreamlineApplicationId -ne 0) {
            $arguments += @("--streamline-application-id", [string]$StreamlineApplicationId)
        }
        $context = New-RecordedProcessContext $caseExecutable $arguments $caseRoot $label
        $started = Get-Date
        $exitCode = -1
        $timedOut = $false
        $launchError = $null
        try {
            # Keep GUI/D3D process creation at script scope. Launching it from a
            # helper function or nested pwsh causes Windows to background-throttle
            # the hidden render window on some laptop configurations.
            $process = Start-Process -FilePath $context.executable `
                -ArgumentList ($context.arguments -join ' ') `
                -RedirectStandardOutput $context.stdout_path `
                -RedirectStandardError $context.stderr_path `
                -PassThru -WindowStyle Hidden
            if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
                $timedOut = $true
                try { $process.Kill($true) } catch {
                    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
                }
                $process.WaitForExit(10000) | Out-Null
            } else {
                $process.Refresh()
                if ($process.HasExited) { $exitCode = [int]$process.ExitCode }
            }
            $process.Dispose()
        } catch {
            $launchError = $_.Exception.Message
            Set-Content -LiteralPath $context.stderr_path -Value $launchError -Encoding UTF8
        }
        $elapsedSeconds = ((Get-Date) - $started).TotalSeconds
        $result = Complete-RecordedProcess `
            $context $exitCode $timedOut $elapsedSeconds $launchError
        $gateFailures = Get-Stage10GateFailures $result $case $Configuration
        $result | Add-Member -NotePropertyName gate_failures -NotePropertyValue @($gateFailures)
        $caseResults.Add($result)
    }
}

$failures = @($caseResults | Where-Object {
    $_.exit_code -ne 0 -or $_.timed_out -or $_.parseable_json_lines -ne 1 -or
    $null -eq $_.json -or $_.gate_failures.Count -ne 0
})
$failureMessages = [System.Collections.Generic.List[string]]::new()
foreach ($failure in $failures) {
    if ($failure.exit_code -ne 0 -or $failure.timed_out -or
        $failure.parseable_json_lines -ne 1 -or $null -eq $failure.json) {
        $detail = if ([string]::IsNullOrWhiteSpace([string]$failure.json_error)) {
            "process/JSON validation failed"
        } else { [string]$failure.json_error }
        $failureMessages.Add("$($failure.label): $detail")
    }
    foreach ($gateFailure in @($failure.gate_failures)) {
        $failureMessages.Add("$($failure.label): $gateFailure")
    }
}
$environment = Get-EnvironmentSnapshot
$environment | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $runRoot "environment.json") -Encoding UTF8
$summary = [ordered]@{
    schema_version = 1
    stage = "10H"
    run_id = $runId
    configuration = $Configuration
    suite = $Suite
    case_name = if ([string]::IsNullOrWhiteSpace($CaseName)) { $null } else { $CaseName }
    seconds = $Seconds
    runs = $Runs
    timeout_seconds = $TimeoutSeconds
    streamline_identity = if ($StreamlineApplicationId -eq 0) { "built-in-project-id" } else { "nvidia-application-id" }
    streamline_application_id = if ($StreamlineApplicationId -eq 0) { $null } else { $StreamlineApplicationId }
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
    failures = @($failureMessages)
    note = "No long-duration workload is implemented; this runner is bounded to 1..30 seconds."
}
$summaryPath = Join-Path $runRoot "summary.json"
$summary | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath $summaryPath -Encoding UTF8
Write-Output $summaryPath
if ($failures.Count -gt 0) { exit 1 }
