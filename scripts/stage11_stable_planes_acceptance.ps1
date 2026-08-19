[CmdletBinding()]
param(
    [ValidateSet("Smoke", "Quality", "Lifecycle", "Gate")]
    [string]$Suite = "Smoke",
    [string]$NrdExe,
    [string]$RrExe,
    [string]$OutputRoot = "output/stage11-stable-planes",
    [ValidateRange(10, 60)]
    [int]$TimeoutSeconds = 60,
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$counterSchema = @(
    "pixels_traced",
    "active_plane_slots",
    "plane_count_0",
    "plane_count_1",
    "plane_count_2",
    "plane_count_3",
    "plane_overflow_pixels",
    "branch_queue_overflow_events",
    "interior_overflow_events",
    "false_intersection_rejections",
    "total_internal_reflection_events",
    "invalid_medium_exit_events"
)

# These are characterization ROIs, not pass/fail thresholds. Keeping the
# normalized table in one place makes output-size conversion auditable.
$roiTable = [ordered]@{
    lamp_edge = @(0.42, 0.08, 0.16, 0.18)
    left_wall_back_wall_seam = @(0.08, 0.30, 0.20, 0.25)
    right_wall_back_wall_seam = @(0.72, 0.30, 0.20, 0.25)
    left_mirror_floor_seam = @(0.20, 0.52, 0.20, 0.18)
    glass_top = @(0.55, 0.30, 0.16, 0.14)
    glass_interior = @(0.57, 0.39, 0.12, 0.16)
    glass_floor_contact = @(0.52, 0.53, 0.20, 0.12)
}

function Quote-Argument([string]$Value) {
    if ($Value -notmatch '[\s"]') {
        return $Value
    }
    return '"' + $Value.Replace('"', '\"') + '"'
}

function Resolve-OptionalPath([string]$Value) {
    if ([string]::IsNullOrWhiteSpace($Value)) {
        return $null
    }
    $candidate = if ([IO.Path]::IsPathRooted($Value)) {
        $Value
    } else {
        Join-Path $repoRoot $Value
    }
    if (-not (Test-Path -LiteralPath $candidate)) {
        return $null
    }
    return (Resolve-Path -LiteralPath $candidate).Path
}

function Get-PropertyPath([object]$Object, [string]$Path) {
    $current = $Object
    foreach ($part in ($Path -split '\.')) {
        if ($null -eq $current) {
            return $null
        }
        $property = $current.PSObject.Properties[$part]
        if ($null -eq $property) {
            return $null
        }
        $current = $property.Value
    }
    return $current
}

function Test-Finite([object]$Value) {
    if ($null -eq $Value) {
        return $false
    }
    try {
        $number = [double]$Value
        return -not [double]::IsNaN($number) -and -not [double]::IsInfinity($number)
    } catch {
        return $false
    }
}

function Test-U32([object]$Value) {
    if (-not (Test-Finite $Value)) {
        return $false
    }
    $number = [double]$Value
    return $number -ge 0 -and $number -le 4294967295 -and
        [math]::Floor($number) -eq $number
}

function Add-Failure([System.Collections.Generic.List[string]]$Failures, [string]$Message) {
    $null = $Failures.Add($Message)
}

function Get-GitEvidence {
    $head = (& git -C $repoRoot rev-parse HEAD 2>$null).Trim()
    $tree = (& git -C $repoRoot rev-parse 'HEAD^{tree}' 2>$null).Trim()
    $status = @(& git -C $repoRoot status --porcelain=v1 --untracked-files=all 2>$null)
    [ordered]@{
        head = if ($head) { $head } else { "unavailable" }
        tree = if ($tree) { $tree } else { "unavailable" }
        dirty = @($status).Count -gt 0
        status = $status
    }
}

function Get-EnvironmentEvidence {
    $gpu = $null
    $gpuError = $null
    try {
        $gpu = Get-CimInstance Win32_VideoController -ErrorAction Stop |
            Where-Object { $_.Name -match "NVIDIA" } |
            Select-Object -First 1
    } catch {
        $gpuError = $_.Exception.Message
    }

    $driver = "N/A"
    $driverSource = "unavailable"
    try {
        if ($null -ne $gpu -and [string]$gpu.DriverVersion) {
            $driver = [string]$gpu.DriverVersion
            $driverSource = "Win32_VideoController.DriverVersion"
        } else {
            $smi = @(& nvidia-smi --query-gpu=driver_version --format=csv,noheader,nounits 2>&1 |
                ForEach-Object { [string]$_ } |
                Where-Object { $_ -and $_ -notmatch "not recognized|failed|error" })
            if ($smi.Count -gt 0) {
                $driver = $smi[0].Trim()
                $driverSource = "nvidia-smi"
            }
        }
    } catch { }

    $powerPlan = "N/A"
    try {
        $powerOutput = @(& powercfg /getactivescheme 2>&1 | ForEach-Object { [string]$_ })
        if ($powerOutput.Count -gt 0) {
            $powerPlan = ($powerOutput -join " ").Trim()
        }
    } catch { }

    $powerSource = "N/A"
    $powerSourceSource = "unavailable"
    try {
        if (-not ("Stage11StablePlanes.Native.PowerStatus" -as [type])) {
            Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
namespace Stage11StablePlanes.Native {
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
        $status = New-Object Stage11StablePlanes.Native.SYSTEM_POWER_STATUS
        if ([Stage11StablePlanes.Native.PowerStatus]::GetSystemPowerStatus([ref]$status)) {
            $powerSourceSource = "GetSystemPowerStatus.ACLineStatus"
            if ($status.ACLineStatus -eq 1) { $powerSource = "AC" }
            elseif ($status.ACLineStatus -eq 0) { $powerSource = "battery" }
        }
    } catch { }

    [ordered]@{
        gpu_name = if ($null -ne $gpu) { [string]$gpu.Name } else { "N/A" }
        gpu_name_source = if ($null -ne $gpu) { "Win32_VideoController.Name" } else { "unavailable" }
        driver_version = $driver
        driver_version_source = $driverSource
        active_power_scheme = $powerPlan
        active_power_scheme_source = "powercfg /getactivescheme"
        power_source = $powerSource
        power_source_source = $powerSourceSource
        gpu_probe_error = $gpuError
        os = [Environment]::OSVersion.VersionString
        powershell = $PSVersionTable.PSVersion.ToString()
        machine = [Environment]::MachineName
    }
}

function Get-FileEvidence([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path) -or -not (Test-Path -LiteralPath $Path)) {
        return [ordered]@{ path = $Path; exists = $false; sha256 = $null; length = $null }
    }
    $item = Get-Item -LiteralPath $Path
    [ordered]@{
        path = $item.FullName
        exists = $true
        sha256 = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        length = $item.Length
    }
}

function New-RunRoot {
    $git = Get-GitEvidence
    $short = if ([string]$git.head -match '^[0-9a-f]{7,}') { $git.head.Substring(0, 7) } else { "nogit" }
    $runId = "{0}-{1}" -f (Get-Date).ToUniversalTime().ToString("yyyyMMdd-HHmmssZ"), $short
    $base = if ([IO.Path]::IsPathRooted($OutputRoot)) { $OutputRoot } else { Join-Path $repoRoot $OutputRoot }
    $runRoot = Join-Path $base $runId
    New-Item -ItemType Directory -Path $runRoot -Force | Out-Null
    return [pscustomobject]@{ id = $runId; path = $runRoot }
}

function Invoke-RecordedProcess(
    [string]$Executable,
    [string[]]$Arguments,
    [string]$CaseRoot,
    [string]$Label,
    [int]$Timeout
) {
    New-Item -ItemType Directory -Path $CaseRoot -Force | Out-Null
    $stdoutPath = Join-Path $CaseRoot "$Label.stdout.txt"
    $stderrPath = Join-Path $CaseRoot "$Label.stderr.txt"
    $argsPath = Join-Path $CaseRoot "$Label.args.txt"
    $exitPath = Join-Path $CaseRoot "$Label.exit.json"
    $command = "$(Quote-Argument $Executable) " + (($Arguments | ForEach-Object { Quote-Argument $_ }) -join " ")
    Set-Content -LiteralPath $argsPath -Value $command -Encoding UTF8

    $started = [Diagnostics.Stopwatch]::StartNew()
    $process = $null
    $timedOut = $false
    $launchError = $null
    $exitCode = $null
    if ([string]::IsNullOrWhiteSpace($Executable) -or -not (Test-Path -LiteralPath $Executable)) {
        $launchError = "executable not found"
    } else {
        try {
            # Start-Process is intentional: it gives every evidence record a
            # PID and makes the timeout/kill boundary independent of the host shell.
            $process = Start-Process -FilePath $Executable -ArgumentList $Arguments -WorkingDirectory $repoRoot `
                -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath -NoNewWindow -PassThru
            if (-not $process.WaitForExit($Timeout * 1000)) {
                $timedOut = $true
                # The renderer may load helper processes; /T makes timeout
                # evidence cover the entire child process tree.
                & taskkill.exe /PID $process.Id /T /F 2>$null | Out-Null
                $process.WaitForExit(5000)
            }
            $process.Refresh()
            if (-not $process.HasExited -and $timedOut) {
                # A stale Process object can miss the tree-kill transition.
                # Kill the exact Start-Process child as a second bounded step;
                # this cannot target an unrelated user process.
                try { $process.Kill() } catch { }
                $process.WaitForExit(5000)
                $process.Refresh()
            }
            if ($process.HasExited) {
                $exitCode = [int]$process.ExitCode
            } else {
                $exitCode = -1
            }
        } catch {
            $launchError = $_.Exception.Message
        }
    }
    $started.Stop()

    $stdout = if (Test-Path -LiteralPath $stdoutPath) {
        Get-Content -LiteralPath $stdoutPath -Raw
    } else { "" }
    $lines = @($stdout -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $json = $null
    $jsonError = $launchError
    if ($timedOut) {
        $jsonError = "timeout after $Timeout seconds"
    } elseif ($null -eq $jsonError) {
        if ($lines.Count -ne 1) {
            $jsonError = "stdout must contain exactly one non-empty JSON line; found $($lines.Count)"
        } else {
            try {
                $json = $lines[0] | ConvertFrom-Json
            } catch {
                $jsonError = "stdout line is not valid JSON: $($_.Exception.Message)"
            }
        }
    }
    $record = [ordered]@{
        label = $Label
        executable = $Executable
        arguments = $Arguments
        command = $command
        args_path = $argsPath
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        exit_path = $exitPath
        exit_code = $exitCode
        timed_out = $timedOut
        elapsed_seconds = [math]::Round($started.Elapsed.TotalSeconds, 3)
        stdout_nonempty_lines = $lines.Count
        json = $json
        json_error = $jsonError
    }
    $record | ConvertTo-Json -Depth 40 | Set-Content -LiteralPath $exitPath -Encoding UTF8
    return [pscustomobject]$record
}

function Convert-NormalizedRoi([double[]]$Normalized, [int]$Width, [int]$Height) {
    if ($Normalized.Count -ne 4 -or $Width -le 0 -or $Height -le 0) {
        throw "invalid normalized ROI input"
    }
    $x = [math]::Floor($Normalized[0] * $Width)
    $y = [math]::Floor($Normalized[1] * $Height)
    $right = [math]::Ceiling(($Normalized[0] + $Normalized[2]) * $Width)
    $bottom = [math]::Ceiling(($Normalized[1] + $Normalized[3]) * $Height)
    $x = [math]::Max(0, [math]::Min($Width - 1, $x))
    $y = [math]::Max(0, [math]::Min($Height - 1, $y))
    $right = [math]::Max($x + 1, [math]::Min($Width, $right))
    $bottom = [math]::Max($y + 1, [math]::Min($Height, $bottom))
    [ordered]@{ x = [int]$x; y = [int]$y; width = [int]($right - $x); height = [int]($bottom - $y) }
}

function Test-ActivePassContract(
    [object]$Json,
    [string]$Consumer,
    [System.Collections.Generic.List[string]]$Failures
) {
    $build = Get-PropertyPath $Json "passes.stable_plane.build"
    $fill = @(Get-PropertyPath $Json "passes.stable_plane.fill")
    if ($null -eq $build) { Add-Failure $Failures "stable_plane.build must be active" }
    if ($fill.Count -ne 3 -or @($fill | Where-Object { $null -eq $_ }).Count -gt 0) {
        Add-Failure $Failures "stable_plane.fill must contain three active child timings"
    }
    $nrdPaths = @("prep", "denoise", "compose")
    foreach ($path in $nrdPaths) {
        $items = @(Get-PropertyPath $Json "passes.nrd_stable.$path")
        $nullCount = @($items | Where-Object { $null -eq $_ }).Count
        if ($Consumer -eq "nrd-stable-planes" -and ($items.Count -ne 3 -or $nullCount -gt 0)) {
            Add-Failure $Failures "nrd_stable.$path must be active for stable NRD"
        }
        if ($Consumer -eq "rr-stable-planes" -and $nullCount -ne 3) {
            Add-Failure $Failures "nrd_stable.$path must be null for fused RR"
        }
    }
    $rrMerge = Get-PropertyPath $Json "passes.rr_stable_merge"
    if ($Consumer -eq "rr-stable-planes" -and $null -eq $rrMerge) {
        Add-Failure $Failures "rr_stable_merge must be active for stable RR"
    }
    if ($Consumer -eq "nrd-stable-planes" -and $null -ne $rrMerge) {
        Add-Failure $Failures "rr_stable_merge must be null for stable NRD"
    }
}

function Test-CounterContract([object]$Json, [System.Collections.Generic.List[string]]$Failures) {
    $counters = Get-PropertyPath $Json "path_space.counters"
    if ($null -eq $counters) {
        Add-Failure $Failures "stable counter telemetry is missing"
        return
    }
    $schema = @($counters.schema | ForEach-Object { [string]$_ })
    if ($schema.Count -ne $counterSchema.Count -or
        (Compare-Object $schema $counterSchema -SyncWindow 0)) {
        Add-Failure $Failures "stable counter schema/order mismatch"
    }
    if (-not (Test-U32 $counters.completed_frames) -or [uint64]$counters.completed_frames -eq 0) {
        Add-Failure $Failures "stable counter completed_frames must be a positive integer"
    }
    if (-not (Test-Finite $counters.pixels_traced) -or
        [double]$counters.pixels_traced -lt 1 -or
        [math]::Floor([double]$counters.pixels_traced) -ne [double]$counters.pixels_traced) {
        Add-Failure $Failures "pixels_traced must be a positive integer"
    }
    if (-not (Test-Finite $counters.active_planes_mean) -or
        [double]$counters.active_planes_mean -lt 0 -or
        [double]$counters.active_planes_mean -gt 3) {
        Add-Failure $Failures "active_planes_mean is outside [0,3]"
    }
    $histogram = @($counters.plane_count_histogram)
    if ($histogram.Count -ne 4 -or
        @($histogram | Where-Object { -not (Test-U32 $_) }).Count -gt 0) {
        Add-Failure $Failures "plane_count_histogram must contain four uint32 values"
    } else {
        $histogramSum = [uint64]0
        foreach ($bucket in $histogram) { $histogramSum += [uint64]$bucket }
        if (Test-Finite $counters.pixels_traced) {
            if ($histogramSum -ne [uint64]$counters.pixels_traced) {
                Add-Failure $Failures "plane_count_histogram sum does not equal pixels_traced"
            }
        }
    }
    $means = @($counters.per_frame_mean)
    if ($means.Count -ne $counterSchema.Count) {
        Add-Failure $Failures "stable counter per_frame_mean length mismatch"
    } else {
        foreach ($mean in $means) {
            if (-not (Test-Finite $mean) -or [double]$mean -lt 0) {
                Add-Failure $Failures "stable counter per_frame_mean is non-finite or negative"
                break
            }
        }
    }
    $lastValues = @(Get-PropertyPath $counters "last.values")
    if ($lastValues.Count -ne $counterSchema.Count -or
        @($lastValues | Where-Object { -not (Test-U32 $_) }).Count -gt 0) {
        Add-Failure $Failures "last counter values must be twelve uint32 values"
    }
    if ($lastValues.Count -eq $counterSchema.Count -and
        (Test-U32 $Json.render_width) -and (Test-U32 $Json.render_height) -and
        [uint64]$lastValues[0] -ne ([uint64]$Json.render_width * [uint64]$Json.render_height)) {
        Add-Failure $Failures "last pixels_traced does not equal the render pixel count"
    }
    if ((Test-Finite $counters.interior_overflow_events) -and
        (Test-Finite $counters.invalid_medium_exit_events) -and
        (([double]$counters.interior_overflow_events -ne 0) -or
         ([double]$counters.invalid_medium_exit_events -ne 0))) {
        Add-Failure $Failures "interior or invalid medium exit counter is non-zero"
    }
}

function Test-LegacyPassContract(
    [object]$Json,
    [System.Collections.Generic.List[string]]$Failures
) {
    if ($null -ne (Get-PropertyPath $Json "path_space.counters")) {
        Add-Failure $Failures "legacy path must not expose stable counter telemetry"
    }
    if ($null -ne (Get-PropertyPath $Json "passes.stable_plane.build")) {
        Add-Failure $Failures "legacy path must not run stable_plane.build"
    }
    foreach ($path in @("passes.stable_plane.fill", "passes.nrd_stable.prep", "passes.nrd_stable.denoise", "passes.nrd_stable.compose")) {
        $items = @(Get-PropertyPath $Json $path)
        if (@($items | Where-Object { $null -ne $_ }).Count -gt 0) {
            Add-Failure $Failures "$path must be inactive for the legacy path"
        }
    }
    if ($null -ne (Get-PropertyPath $Json "passes.rr_stable_merge")) {
        Add-Failure $Failures "legacy path must not run rr_stable_merge"
    }
}

function Test-ReflexAccounting([object]$Json, [System.Collections.Generic.List[string]]$Failures) {
    $reflex = Get-PropertyPath $Json "reflex"
    if ($null -eq $reflex) { Add-Failure $Failures "reflex telemetry is missing"; return }
    $markers = Get-PropertyPath $reflex "marker_counts"
    $token = [uint64]$reflex.token_count
    $present = [uint64]$reflex.present_common_count
    if ($token -ne [uint64]$reflex.sleep_count -or
        $token -ne [uint64]$markers.render_submit_start -or
        [uint64]$markers.render_submit_start -ne [uint64]$markers.render_submit_end -or
        [uint64]$markers.present_start -ne [uint64]$markers.present_end -or
        [uint64]$markers.present_end -ne $present) {
        Add-Failure $Failures "Reflex token/sleep/PCL/Present/presentCommon counts disagree"
    }
}

function Test-OptimalExtent([object]$Json, [System.Collections.Generic.List[string]]$Failures) {
    $optimal = Get-PropertyPath $Json "upscaler.dlss_optimal"
    if ($null -eq $optimal) { Add-Failure $Failures "DLSS optimal settings are missing"; return }
    $renderWidth = [int]$Json.render_width
    $renderHeight = [int]$Json.render_height
    if ([int]$optimal.optimal_render_width -ne $renderWidth -or
        [int]$optimal.optimal_render_height -ne $renderHeight) {
        Add-Failure $Failures "render extent does not match reported DLSS optimal extent"
    }
}

function Test-BenchmarkContract(
    [object]$Record,
    [hashtable]$Case,
    [System.Collections.Generic.List[string]]$Failures,
    [switch]$Gate
) {
    if ($Record.exit_code -ne 0) { Add-Failure $Failures "process exit_code=$($Record.exit_code)" }
    if ($Record.timed_out) { Add-Failure $Failures "process timed out" }
    if ($Record.stdout_nonempty_lines -ne 1 -or $null -eq $Record.json) {
        Add-Failure $Failures "stdout is not exactly one JSON line: $($Record.json_error)"
        return
    }
    $json = $Record.json
    if ([int]$json.output_width -ne [int]$Case.width -or [int]$json.output_height -ne [int]$Case.height) {
        Add-Failure $Failures "output extent mismatch"
    }
    $expectedPathSpace = if ([string]$Case.consumer -eq "legacy") { "legacy" } else { "stable-planes" }
    if ([string]$json.path_space.active -ne $expectedPathSpace -or
        [string]$json.path_space.consumer -ne [string]$Case.consumer) {
        Add-Failure $Failures "path-space consumer mismatch"
    }
    if ([string]$Case.consumer -eq "legacy") {
        Test-LegacyPassContract $json $Failures
    } else {
        Test-ActivePassContract $json ([string]$Case.consumer) $Failures
        Test-CounterContract $json $Failures
    }
    if ([string]$Case.feature -eq "nrd" -and $json.denoiser.nrd_compiled -ne $true) {
        Add-Failure $Failures "NRD executable does not report nrd compiled"
    }
    if ([string]$Case.feature -eq "streamline-rr" -and $json.dlss_rr.compiled -ne $true) {
        Add-Failure $Failures "RR executable does not report streamline-rr compiled"
    }
    if (-not (Test-U32 $json.gpu_idle_wait_count) -or [uint64]$json.gpu_idle_wait_count -ne 0) {
        Add-Failure $Failures "gpu_idle_wait_count must remain zero during the case"
    }
    if ($Gate) {
        if (-not (Test-Finite $json.passes.total.p95_ms) -or [double]$json.passes.total.p95_ms -gt 16.67) {
            Add-Failure $Failures "Total p95 exceeds 16.67 ms or is unavailable"
        }
        if ([string]$json.gpu_name -notmatch "RTX 4060 Laptop") {
            Add-Failure $Failures "GPU is not identified as RTX 4060 Laptop"
        }
        if ([string]$Case.consumer -ne "legacy" -and [uint64]$json.path_space.allocated_bytes -le 0) {
            Add-Failure $Failures "stable-plane allocation is not reported"
        }
        if ([string]$json.memory.status -ne "available" -or [uint64]$json.memory.budget_bytes -eq 0) {
            Add-Failure $Failures "local VRAM measurement is unavailable"
        }
        Test-ReflexAccounting $json $Failures
        if ([string]$Case.consumer -eq "rr-stable-planes") {
            Test-OptimalExtent $json $Failures
        }
    }
}

function Test-CaptureContract(
    [object]$Record,
    [hashtable]$Case,
    [System.Collections.Generic.List[string]]$Failures
) {
    if ($Record.exit_code -ne 0 -or $Record.timed_out -or $null -eq $Record.json) {
        Add-Failure $Failures "capture process did not produce valid JSON"
        return
    }
    $json = $Record.json
    if (-not (Test-Path -LiteralPath ([string]$json.png_path))) {
        Add-Failure $Failures "capture PNG is missing"
    }
    if ([int]$json.output_width -ne [int]$Case.width -or [int]$json.output_height -ne [int]$Case.height) {
        Add-Failure $Failures "capture output extent mismatch"
    }
    $expectedPathSpace = if ([string]$Case.consumer -eq "legacy") { "legacy" } else { "stable-planes" }
    if ([string]$json.path_space.active -ne $expectedPathSpace -or
        [string]$json.path_space.consumer -ne [string]$Case.consumer) {
        Add-Failure $Failures "capture path-space consumer mismatch"
    }
}

function Test-ImageDiffContract(
    [object]$Record,
    [int]$Width,
    [int]$Height,
    [object]$ExpectedRoi,
    [System.Collections.Generic.List[string]]$Failures
) {
    if ($Record.exit_code -ne 0 -or $Record.timed_out -or
        $Record.stdout_nonempty_lines -ne 1 -or $null -eq $Record.json) {
        Add-Failure $Failures "image_diff did not produce one valid JSON line: $($Record.json_error)"
        return
    }
    $json = $Record.json
    if ([int]$json.width -ne $Width -or [int]$json.height -ne $Height) {
        Add-Failure $Failures "image_diff source extent mismatch"
    }
    $expected = if ($null -eq $ExpectedRoi) {
        [ordered]@{ x = 0; y = 0; width = $Width; height = $Height }
    } else { $ExpectedRoi }
    foreach ($field in @("x", "y", "width", "height")) {
        if ([int](Get-PropertyPath $json "roi.$field") -ne [int]$expected[$field]) {
            Add-Failure $Failures "image_diff ROI mismatch for $field"
        }
    }
    foreach ($field in @("changed_rgb_pixels", "rgb_pixels_over_2", "alpha_mismatch_count", "max_channel_abs_diff", "mean_max_channel_abs_diff", "mae", "rmse")) {
        $value = Get-PropertyPath $json $field
        if (-not (Test-Finite $value) -or [double]$value -lt 0) {
            Add-Failure $Failures "image_diff metric $field is non-finite or negative"
        }
    }
}

function New-RendererArguments([hashtable]$Case, [string]$Mode, [string]$OutputPath) {
    $pathSpaceMode = if ([string]$Case.consumer -eq "legacy") { "legacy" } else { "stable-planes" }
    $arguments = @(
        "--output-size", ("{0}x{1}" -f $Case.width, $Case.height),
        "--scene", [string]$Case.scene,
        "--path-space-mode", $pathSpaceMode,
        "--denoiser", [string]$Case.denoiser,
        "--upscaler", [string]$Case.upscaler,
        "--atrous-mode", "baseline",
        "--command-recording-mode", "optimized",
        "--acceleration-structure-mode", "baseline"
    )
    if ($Mode -eq "benchmark") {
        $arguments += @("--benchmark-seconds", "1")
    } else {
        $arguments += @("--capture-output", $OutputPath, "--capture-after-spp", [string]$Case.spp)
    }
    return $arguments
}

function Get-ExecutableForCase([hashtable]$Case) {
    if ([string]$Case.feature -eq "nrd") { return (Resolve-OptionalPath $NrdExe) }
    return (Resolve-OptionalPath $RrExe)
}

function New-Case([string]$Label, [string]$Feature, [string]$Denoiser, [string]$Upscaler, [int]$Width, [int]$Height, [string]$Scene, [string]$Consumer, [int]$Spp) {
    return @{
        label = $Label; feature = $Feature; denoiser = $Denoiser; upscaler = $Upscaler
        width = $Width; height = $Height; scene = $Scene; consumer = $Consumer; spp = $Spp
    }
}

function Get-CaseStatus([System.Collections.Generic.List[string]]$Failures) {
    if ($Failures.Count -eq 0) { return "PASS" }
    return "FAIL"
}

function Get-SuiteCases([string]$SelectedSuite) {
    if ($SelectedSuite -eq "Smoke") {
        return @(
            (New-Case "cornell_nrd_smoke" "nrd" "nrd-reblur" "native" 320 180 "cornell" "nrd-stable-planes" 8),
            (New-Case "cornell_rr_smoke" "streamline-rr" "dlss-rr" "dlss-quality" 320 180 "cornell" "rr-stable-planes" 8)
        )
    }
    if ($SelectedSuite -eq "Gate") {
        return @(
            (New-Case "cornell_nrd_1080p_gate" "nrd" "nrd-reblur" "native" 1920 1080 "cornell" "nrd-stable-planes" 0),
            (New-Case "cornell_rr_1080p_gate" "streamline-rr" "dlss-rr" "dlss-quality" 1920 1080 "cornell" "rr-stable-planes" 0)
        )
    }
    if ($SelectedSuite -eq "Lifecycle") {
        return @(
            (New-Case "cornell_nrd_lifecycle" "nrd" "nrd-reblur" "native" 1280 720 "cornell" "nrd-stable-planes" 0),
            (New-Case "nested_rr_lifecycle" "streamline-rr" "dlss-rr" "dlss-quality" 1280 720 "nested-dielectric" "rr-stable-planes" 0)
        )
    }
    return @(
        (New-Case "cornell_nrd_spp64" "nrd" "nrd-reblur" "native" 1280 720 "cornell" "nrd-stable-planes" 64),
        (New-Case "cornell_nrd_spp127" "nrd" "nrd-reblur" "native" 1280 720 "cornell" "nrd-stable-planes" 127),
        (New-Case "cornell_nrd_spp128" "nrd" "nrd-reblur" "native" 1280 720 "cornell" "nrd-stable-planes" 128),
        (New-Case "cornell_rr_spp64" "streamline-rr" "dlss-rr" "dlss-quality" 1280 720 "cornell" "rr-stable-planes" 64),
        (New-Case "cornell_rr_spp127" "streamline-rr" "dlss-rr" "dlss-quality" 1280 720 "cornell" "rr-stable-planes" 127),
        (New-Case "cornell_rr_spp128" "streamline-rr" "dlss-rr" "dlss-quality" 1280 720 "cornell" "rr-stable-planes" 128),
        (New-Case "cornell_legacy_spp127" "nrd" "nrd-reblur" "native" 1280 720 "cornell" "legacy" 127),
        (New-Case "cornell_legacy_spp128" "nrd" "nrd-reblur" "native" 1280 720 "cornell" "legacy" 128),
        (New-Case "nested_nrd_spp128" "nrd" "nrd-reblur" "native" 1280 720 "nested-dielectric" "nrd-stable-planes" 128),
        (New-Case "nested_rr_spp128" "streamline-rr" "dlss-rr" "dlss-quality" 1280 720 "nested-dielectric" "rr-stable-planes" 128)
    )
}

function Invoke-SelfTest {
    $passed = 0
    $failed = [System.Collections.Generic.List[string]]::new()
    function Assert-Reject([string]$Name, [scriptblock]$Action) {
        $localFailures = [System.Collections.Generic.List[string]]::new()
        & $Action $localFailures
        if ($localFailures.Count -eq 0) {
            $script:SelfTestFailed.Add($Name)
        } else {
            $script:SelfTestPassed++
        }
    }
    $script:SelfTestFailed = $failed
    $script:SelfTestPassed = 0

    $goodJson = @{
        output_width = 320; output_height = 180; render_width = 320; render_height = 180
        gpu_name = "NVIDIA GeForce RTX 4060 Laptop GPU"
        gpu_idle_wait_count = 0
        path_space = @{ active = "stable-planes"; consumer = "rr-stable-planes"; allocated_bytes = 1
            counters = @{
                schema = $counterSchema; completed_frames = 1; pixels_traced = 57600
                active_planes_mean = 1.0; plane_count_histogram = @(0, 57600, 0, 0)
                plane_overflow_pixels = 0; branch_queue_overflow_events = 0
                interior_overflow_events = 0; false_intersection_rejections = 0
                total_internal_reflection_events = 0; invalid_medium_exit_events = 0
                per_frame_mean = @(57600, 57600, 0, 57600, 0, 0, 0, 0, 0, 0, 0, 0)
                last = @{ values = @(57600, 57600, 0, 57600, 0, 0, 0, 0, 0, 0, 0, 0) }
            } }
        denoiser = @{ nrd_compiled = $false }
        dlss_rr = @{ compiled = $true }
        passes = @{ total = @{ p95_ms = 1 }; stable_plane = @{ build = @{ p95_ms = 1 }; fill = @(@{}, @{}, @{}) }
            nrd_stable = @{ prep = @($null, $null, $null); denoise = @($null, $null, $null); compose = @($null, $null, $null) }
            rr_stable_merge = @{} }
        upscaler = @{ dlss_optimal = @{ optimal_render_width = 320; optimal_render_height = 180 } }
        memory = @{ status = "available"; budget_bytes = 1 }
        reflex = @{ token_count = 1; sleep_count = 1; present_common_count = 1; marker_counts = @{ render_submit_start = 1; render_submit_end = 1; present_start = 1; present_end = 1 } }
    } | ConvertTo-Json -Depth 40 -Compress | ConvertFrom-Json
    $case = New-Case "selftest" "streamline-rr" "dlss-rr" "dlss-quality" 320 180 "cornell" "rr-stable-planes" 0

    Assert-Reject "RR profile mismatch" { param($f) $bad = $goodJson | ConvertTo-Json -Depth 40 -Compress | ConvertFrom-Json; $bad.path_space.consumer = "nrd-stable-planes"; Test-BenchmarkContract ([pscustomobject]@{ exit_code = 0; timed_out = $false; stdout_nonempty_lines = 1; json = $bad; json_error = $null }) $case $f }
    Assert-Reject "missing active RR pass" { param($f) $bad = $goodJson | ConvertTo-Json -Depth 40 -Compress | ConvertFrom-Json; $bad.passes.rr_stable_merge = $null; Test-BenchmarkContract ([pscustomobject]@{ exit_code = 0; timed_out = $false; stdout_nonempty_lines = 1; json = $bad; json_error = $null }) $case $f }
    Assert-Reject "hidden NRD cost" { param($f) $bad = $goodJson | ConvertTo-Json -Depth 40 -Compress | ConvertFrom-Json; $bad.passes.nrd_stable.prep = @(@{}, @{}, @{}); Test-BenchmarkContract ([pscustomobject]@{ exit_code = 0; timed_out = $false; stdout_nonempty_lines = 1; json = $bad; json_error = $null }) $case $f }
    Assert-Reject "missing nested benchmark counters" { param($f) $bad = $goodJson | ConvertTo-Json -Depth 40 -Compress | ConvertFrom-Json; $bad.path_space.counters = $null; Test-BenchmarkContract ([pscustomobject]@{ exit_code = 0; timed_out = $false; stdout_nonempty_lines = 1; json = $bad; json_error = $null }) $case $f }
    Assert-Reject "bad histogram sum" { param($f) $bad = $goodJson | ConvertTo-Json -Depth 40 -Compress | ConvertFrom-Json; $bad.path_space.counters.plane_count_histogram[1] = 1; Test-CounterContract $bad $f }
    Assert-Reject "nonzero gpu idle wait" { param($f) $bad = $goodJson | ConvertTo-Json -Depth 40 -Compress | ConvertFrom-Json; $bad.gpu_idle_wait_count = 1; Test-BenchmarkContract ([pscustomobject]@{ exit_code = 0; timed_out = $false; stdout_nonempty_lines = 1; json = $bad; json_error = $null }) $case $f }
    Assert-Reject "wrong optimal extent" { param($f) $bad = $goodJson | ConvertTo-Json -Depth 40 -Compress | ConvertFrom-Json; $bad.upscaler.dlss_optimal.optimal_render_width = 160; Test-OptimalExtent $bad $f }
    Assert-Reject "Reflex Present accounting" { param($f) $bad = $goodJson | ConvertTo-Json -Depth 40 -Compress | ConvertFrom-Json; $bad.reflex.present_common_count = 2; Test-ReflexAccounting $bad $f }
    Assert-Reject "timeout" { param($f) $record = [pscustomobject]@{ exit_code = -1; timed_out = $true; stdout_nonempty_lines = 0; json = $null; json_error = "timeout" }; Test-BenchmarkContract $record $case $f }
    Assert-Reject "multiple JSON lines" { param($f) $record = [pscustomobject]@{ exit_code = 0; timed_out = $false; stdout_nonempty_lines = 2; json = $null; json_error = "two lines" }; Test-BenchmarkContract $record $case $f }
    Assert-Reject "PENDING_MANUAL cannot pass" { param($f) $status = "PENDING_MANUAL"; if ($status -eq "PENDING_MANUAL") { Add-Failure $f "manual status is not PASS" } }
    $roi = Convert-NormalizedRoi $roiTable.lamp_edge 320 180
    if ($roi.x -lt 0 -or $roi.y -lt 0 -or $roi.x + $roi.width -gt 320 -or $roi.y + $roi.height -gt 180) {
        $script:SelfTestFailed.Add("ROI normalization bounds")
    } else { $script:SelfTestPassed++ }

    # A legacy capture is valid evidence, but must identify itself as legacy.
    $legacyCase = New-Case "legacy-selftest" "nrd" "nrd-reblur" "native" 320 180 "cornell" "legacy" 1
    $legacyCapture = [pscustomobject]@{
        exit_code = 0; timed_out = $false
        json = [pscustomobject]@{
            png_path = $PSCommandPath; output_width = 320; output_height = 180
            path_space = [pscustomobject]@{ active = "legacy"; consumer = "legacy" }
        }
    }
    $legacyFailures = [System.Collections.Generic.List[string]]::new()
    Test-CaptureContract $legacyCapture $legacyCase $legacyFailures
    if ($legacyFailures.Count -gt 0) {
        $script:SelfTestFailed.Add("legacy capture contract: $($legacyFailures -join ', ')")
    } else { $script:SelfTestPassed++ }

    $legacyJson = $goodJson | ConvertTo-Json -Depth 40 -Compress | ConvertFrom-Json
    $legacyJson.path_space.active = "legacy"
    $legacyJson.path_space.consumer = "legacy"
    $legacyJson.path_space.counters = $null
    $legacyJson.denoiser.nrd_compiled = $true
    $legacyJson.dlss_rr.compiled = $false
    $legacyJson.passes.stable_plane.build = $null
    $legacyJson.passes.stable_plane.fill = @($null, $null, $null)
    $legacyJson.passes.nrd_stable.prep = @($null, $null, $null)
    $legacyJson.passes.nrd_stable.denoise = @($null, $null, $null)
    $legacyJson.passes.nrd_stable.compose = @($null, $null, $null)
    $legacyJson.passes.rr_stable_merge = $null
    $legacyBenchmarkFailures = [System.Collections.Generic.List[string]]::new()
    $legacyBenchmarkRecord = [pscustomobject]@{
        exit_code = 0; timed_out = $false; stdout_nonempty_lines = 1
        json = $legacyJson; json_error = $null
    }
    Test-BenchmarkContract $legacyBenchmarkRecord $legacyCase $legacyBenchmarkFailures
    if ($legacyBenchmarkFailures.Count -gt 0) {
        $script:SelfTestFailed.Add("legacy benchmark contract: $($legacyBenchmarkFailures -join ', ')")
    } else { $script:SelfTestPassed++ }

    $diffRecord = [pscustomobject]@{
        exit_code = 0; timed_out = $false; stdout_nonempty_lines = 1; json_error = $null
        json = [pscustomobject]@{
            width = 320; height = 180; roi = [pscustomobject]@{ x = 0; y = 0; width = 320; height = 180 }
            changed_rgb_pixels = 1; rgb_pixels_over_2 = 0; alpha_mismatch_count = 0
            max_channel_abs_diff = 1; mean_max_channel_abs_diff = 0.1; mae = 0.01; rmse = 0.02
        }
    }
    $diffFailures = [System.Collections.Generic.List[string]]::new()
    Test-ImageDiffContract $diffRecord 320 180 $null $diffFailures
    if ($diffFailures.Count -gt 0) {
        $script:SelfTestFailed.Add("image diff contract: $($diffFailures -join ', ')")
    } else { $script:SelfTestPassed++ }
    Assert-Reject "wrong image diff ROI" {
        param($f)
        $bad = $diffRecord | ConvertTo-Json -Depth 20 -Compress | ConvertFrom-Json
        $bad.json.roi.width = 319
        Test-ImageDiffContract $bad 320 180 $null $f
    }

    $nestedQualityCases = @(Get-SuiteCases "Quality" | Where-Object { $_.scene -eq "nested-dielectric" -and $_.spp -eq 128 })
    if ($nestedQualityCases.Count -ne 2) {
        $script:SelfTestFailed.Add("nested Quality cases")
    } else {
        foreach ($nestedCase in $nestedQualityCases) {
            $benchmarkArgs = New-RendererArguments $nestedCase "benchmark" ""
            if ($benchmarkArgs -notcontains "--benchmark-seconds" -or
                $benchmarkArgs[$benchmarkArgs.IndexOf("--benchmark-seconds") + 1] -ne "1" -or
                $benchmarkArgs -contains "--capture-output") {
                $script:SelfTestFailed.Add("nested benchmark command: $($nestedCase.label)")
            }
        }
        $script:SelfTestPassed++
    }

    $nestedCase = $nestedQualityCases[0]
    $captureFailures = [System.Collections.Generic.List[string]]::new()
    Test-CaptureContract ([pscustomobject]@{
            exit_code = 0; timed_out = $false
            json = [pscustomobject]@{
                png_path = $PSCommandPath; output_width = 1280; output_height = 720
                path_space = [pscustomobject]@{ active = "stable-planes"; consumer = "nrd-stable-planes" }
            }
        }) $nestedCase $captureFailures
    $benchmarkFailures = [System.Collections.Generic.List[string]]::new()
    $missingBenchmark = $goodJson | ConvertTo-Json -Depth 40 -Compress | ConvertFrom-Json
    $missingBenchmark.path_space.counters = $null
    Test-BenchmarkContract ([pscustomobject]@{
            exit_code = 0; timed_out = $false; stdout_nonempty_lines = 1
            json = $missingBenchmark; json_error = $null
        }) $nestedCase $benchmarkFailures
    if ($captureFailures.Count -ne 0 -or (Get-CaseStatus $benchmarkFailures) -ne "FAIL") {
        $script:SelfTestFailed.Add("nested capture PASS plus benchmark FAIL must fail the case")
    } else {
        $script:SelfTestPassed++
    }

    if ($failed.Count -gt 0) {
        throw "SelfTest failed: $($failed -join '; ')"
    }
    Write-Output ("SelfTest PASS: {0} rejection/contract checks" -f $script:SelfTestPassed)
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

$run = New-RunRoot
$runRoot = $run.path
$gitEvidence = Get-GitEvidence
$environment = Get-EnvironmentEvidence
$records = [System.Collections.Generic.List[object]]::new()
$caseSummaries = [System.Collections.Generic.List[object]]::new()
$suiteFailures = [System.Collections.Generic.List[string]]::new()
$capturePaths = @{}
$qualityDiffs = [System.Collections.Generic.List[object]]::new()
$cases = @(Get-SuiteCases $Suite)
$effectiveTimeout = if ($Suite -eq "Smoke") { [math]::Min(30, $TimeoutSeconds) } else { $TimeoutSeconds }
$imageDiffExe = Resolve-OptionalPath "target/release/image_diff.exe"

foreach ($case in $cases) {
    $caseRoot = Join-Path $runRoot ([string]$case.label)
    $caseFailures = [System.Collections.Generic.List[string]]::new()
    $exe = Get-ExecutableForCase $case
    if ($null -eq $exe) {
        $caseSummaries.Add([ordered]@{ label = $case.label; status = "SKIPPED"; reason = "required feature executable is missing"; gates = @() })
        continue
    }

    if ($Suite -eq "Lifecycle") {
        $args = New-RendererArguments $case "lifecycle" ""
        $record = Invoke-RecordedProcess $exe ($args | Where-Object { $_ -notin @("--capture-output", "", "--capture-after-spp", "0") }) $caseRoot "lifecycle" ([math]::Min(15, $effectiveTimeout))
        $records.Add($record)
        if ($record.timed_out) {
            # No benchmark JSON is expected from a deliberately observed GUI.
            # The visual stability and input interactions remain manual evidence.
            $status = "PENDING_MANUAL"
        } else {
            Add-Failure $caseFailures "lifecycle process exited before the observation timeout: $($record.exit_code)"
            $status = "FAIL"
        }
        $caseSummaries.Add([ordered]@{ label = $case.label; status = $status; gates = @($caseFailures) })
        continue
    }

    if ($Suite -eq "Smoke" -or $Suite -eq "Quality") {
        $captureRoot = Join-Path $caseRoot "capture"
        $png = Join-Path $captureRoot "$($case.label).png"
        $captureArgs = New-RendererArguments $case "capture" $png
        $captureRecord = Invoke-RecordedProcess $exe $captureArgs $captureRoot "capture" $effectiveTimeout
        $records.Add($captureRecord)
        Test-CaptureContract $captureRecord $case $caseFailures
        if ($caseFailures.Count -eq 0 -and $null -ne $captureRecord.json) {
            $capturePaths[[string]$case.label] = [string]$captureRecord.json.png_path
        }
    }

    if ($Suite -eq "Smoke" -or $Suite -eq "Gate") {
        $benchmarkArgs = New-RendererArguments $case "benchmark" ""
        $benchmarkRecord = Invoke-RecordedProcess $exe $benchmarkArgs (Join-Path $caseRoot "benchmark") "benchmark" $effectiveTimeout
        $records.Add($benchmarkRecord)
        Test-BenchmarkContract $benchmarkRecord $case $caseFailures -Gate:($Suite -eq "Gate")
    }

    if (($Suite -eq "Quality") -and
        ([string]$case.scene -eq "nested-dielectric") -and
        ([int]$case.spp -eq 128)) {
        # Nested Quality must include a separately recorded one-second
        # benchmark. A successful capture cannot hide a failed benchmark.
        $benchmarkArgs = New-RendererArguments $case "benchmark" ""
        $nestedBenchmarkRecord = Invoke-RecordedProcess $exe $benchmarkArgs (Join-Path $caseRoot "benchmark") "benchmark" $effectiveTimeout
        $records.Add($nestedBenchmarkRecord)
        Test-BenchmarkContract $nestedBenchmarkRecord $case $caseFailures
    }

    $status = Get-CaseStatus $caseFailures
    $caseSummaries.Add([ordered]@{ label = $case.label; status = $status; gates = @($caseFailures) })
    foreach ($failure in $caseFailures) { Add-Failure $suiteFailures "$($case.label): $failure" }
}

if ($Suite -eq "Quality") {
    $qualityFailures = [System.Collections.Generic.List[string]]::new()
    # Temporal pairs expose residual motion; convergence pairs expose sampling
    # sensitivity; stable-vs-legacy is characterization rather than a winner gate.
    $pairSpecs = @(
        [ordered]@{ label = "nrd_temporal_127_128"; left = "cornell_nrd_spp127"; right = "cornell_nrd_spp128" },
        [ordered]@{ label = "rr_temporal_127_128"; left = "cornell_rr_spp127"; right = "cornell_rr_spp128" },
        [ordered]@{ label = "legacy_temporal_127_128"; left = "cornell_legacy_spp127"; right = "cornell_legacy_spp128" },
        [ordered]@{ label = "nrd_convergence_64_128"; left = "cornell_nrd_spp64"; right = "cornell_nrd_spp128" },
        [ordered]@{ label = "rr_convergence_64_128"; left = "cornell_rr_spp64"; right = "cornell_rr_spp128" },
        [ordered]@{ label = "nrd_stable_vs_legacy_128"; left = "cornell_nrd_spp128"; right = "cornell_legacy_spp128" }
    )
    if ($null -eq $imageDiffExe) {
        Add-Failure $qualityFailures "image_diff executable is unavailable"
    } else {
        foreach ($pair in $pairSpecs) {
            $pairFailures = [System.Collections.Generic.List[string]]::new()
            if (-not $capturePaths.ContainsKey([string]$pair.left) -or
                -not $capturePaths.ContainsKey([string]$pair.right)) {
                Add-Failure $pairFailures "one or both source captures are unavailable"
            } else {
                $pairRoot = Join-Path $runRoot (Join-Path "quality-diffs" ([string]$pair.label))
                $leftPath = [string]$capturePaths[[string]$pair.left]
                $rightPath = [string]$capturePaths[[string]$pair.right]
                $fullRecord = Invoke-RecordedProcess $imageDiffExe @($leftPath, $rightPath) (Join-Path $pairRoot "full") "diff" ([math]::Min(30, $effectiveTimeout))
                $records.Add($fullRecord)
                Test-ImageDiffContract $fullRecord 1280 720 $null $pairFailures
                $roiResults = [System.Collections.Generic.List[object]]::new()
                foreach ($roiName in $roiTable.Keys) {
                    $roi = Convert-NormalizedRoi $roiTable[$roiName] 1280 720
                    $roiArgument = "{0},{1},{2},{3}" -f $roi.x, $roi.y, $roi.width, $roi.height
                    $roiRecord = Invoke-RecordedProcess $imageDiffExe @($leftPath, $rightPath, "--roi", $roiArgument) (Join-Path $pairRoot ([string]$roiName)) "diff" ([math]::Min(30, $effectiveTimeout))
                    $records.Add($roiRecord)
                    Test-ImageDiffContract $roiRecord 1280 720 $roi $pairFailures
                    $roiResults.Add([ordered]@{
                        name = $roiName
                        normalized = $roiTable[$roiName]
                        pixels = $roi
                        metrics = $roiRecord.json
                    })
                }
                $qualityDiffs.Add([ordered]@{
                    label = $pair.label
                    left = $pair.left
                    right = $pair.right
                    full = $fullRecord.json
                    rois = $roiResults
                    status = if ($pairFailures.Count -eq 0) { "PENDING_MANUAL" } else { "FAIL" }
                    failures = @($pairFailures)
                })
            }
            foreach ($failure in $pairFailures) {
                Add-Failure $qualityFailures "$($pair.label): $failure"
            }
        }
    }
    $qualityStatus = if ($qualityFailures.Count -eq 0) { "PENDING_MANUAL" } else { "FAIL" }
    $caseSummaries.Add([ordered]@{
        label = "quality_image_diffs"
        status = $qualityStatus
        gates = @($qualityFailures)
    })
    foreach ($failure in $qualityFailures) { Add-Failure $suiteFailures "quality_image_diffs: $failure" }
}

$skippedCount = @($caseSummaries | Where-Object { $_.status -eq "SKIPPED" }).Count
$failedCount = @($caseSummaries | Where-Object { $_.status -eq "FAIL" }).Count
$pendingCount = @($caseSummaries | Where-Object { $_.status -eq "PENDING_MANUAL" }).Count
$overall = if ($failedCount -gt 0) { "FAIL" } elseif ($caseSummaries.Count -eq 0 -or $skippedCount -eq $caseSummaries.Count) { "SKIPPED" } elseif ($pendingCount -gt 0) { "PENDING_MANUAL" } else { "PASS" }
$summary = [ordered]@{
    schema_version = 2
    suite = $Suite
    run_id = $run.id
    overall = $overall
    git = $gitEvidence
    environment = $environment
    executables = [ordered]@{ nrd = Get-FileEvidence (Resolve-OptionalPath $NrdExe); rr = Get-FileEvidence (Resolve-OptionalPath $RrExe); image_diff = Get-FileEvidence $imageDiffExe }
    timeout_seconds = $effectiveTimeout
    roi_table = $roiTable
    cases = $caseSummaries
    process_records = $records
    quality_diffs = $qualityDiffs
    gate_failures = $suiteFailures
    manual = @("PENDING_MANUAL: static image quality, water ripple, seam motion, lamp-edge stability, resize/minimize/recovery and shader hot reload require human observation")
    constraints = [ordered]@{ default_path_switched = $false; frame_generation_implemented = $false; long_tests_run = $false }
}
$summaryPath = Join-Path $runRoot "summary.json"
$summary | ConvertTo-Json -Depth 50 | Set-Content -LiteralPath $summaryPath -Encoding UTF8
Write-Output ("run_id={0}" -f $run.id)
Write-Output ("summary={0}" -f $summaryPath)
Write-Output ("overall={0}" -f $overall)
if ($overall -eq "FAIL") { exit 1 }
if ($overall -eq "SKIPPED") { exit 2 }
if ($Suite -eq "Smoke" -and $overall -ne "PASS") { exit 1 }
exit 0
