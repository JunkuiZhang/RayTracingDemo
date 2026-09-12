[CmdletBinding()]
param(
    [ValidateSet("Smoke", "Matrix", "DebugValidation")]
    [string]$Suite = "Smoke",
    [ValidateSet("", "feature_off", "fg_compiled_off", "fg_quality", "fg_rr_quality", "fg_animated", "fg_debug")]
    [string]$CaseName = "",
    [string]$DefaultExe,
    [string]$FgExe,
    [string]$RrFgExe,
    [string]$FgDebugExe,
    [ValidateRange(1, 5)]
    [int]$Seconds = 1,
    [ValidateRange(10, 60)]
    [int]$TimeoutSeconds = 60,
    [uint32]$StreamlineApplicationId = 0,
    [switch]$SelfTest,
    [string]$OutputRoot = "output/stage11-fg"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

function Quote-Argument([string]$Value) {
    if ($Value -notmatch '[\s"]') { return $Value }
    return '"' + $Value.Replace('"', '\"') + '"'
}

function Resolve-RepoPath([string]$PathValue) {
    if ([string]::IsNullOrWhiteSpace($PathValue)) { return $null }
    $candidate = if ([IO.Path]::IsPathRooted($PathValue)) {
        $PathValue
    } else {
        Join-Path $repoRoot $PathValue
    }
    return (Resolve-Path -LiteralPath $candidate).Path
}

function Get-JsonPathValue([object]$Object, [string]$PathValue) {
    $current = $Object
    foreach ($part in ($PathValue -split '\.')) {
        if ($null -eq $current) { return $null }
        $property = $current.PSObject.Properties[$part]
        if ($null -eq $property) { return $null }
        $current = $property.Value
    }
    return $current
}

function Test-Finite([object]$Value) {
    if ($null -eq $Value) { return $false }
    try {
        $number = [double]$Value
        return -not [double]::IsNaN($number) -and -not [double]::IsInfinity($number)
    } catch {
        return $false
    }
}

function Get-ExactJsonLine([string]$RawText) {
    $text = if ($null -eq $RawText) { "" } else { $RawText }
    if ($text.StartsWith([char]0xFEFF)) { $text = $text.Substring(1) }
    if ($text.EndsWith("`r`n")) { $text = $text.Substring(0, $text.Length - 2) }
    elseif ($text.EndsWith("`n")) { $text = $text.Substring(0, $text.Length - 1) }
    $lines = if ($text.Length -eq 0) { @() } else { @($text -split "`r?`n") }
    $parseable = [System.Collections.Generic.List[object]]::new()
    foreach ($line in $lines) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        try { $parseable.Add(($line | ConvertFrom-Json -Depth 60)) } catch { }
    }
    $exact = @($lines).Count -eq 1 -and $parseable.Count -eq 1
    [pscustomobject]@{
        exact = $exact
        line_count = @($lines).Count
        parseable_json_lines = $parseable.Count
        json = if ($exact) { $parseable[0] } else { $null }
        error = if ($exact) { $null } else {
            "stdout must contain exactly one JSON line; lines=$(@($lines).Count) parseable_json_lines=$($parseable.Count)"
        }
    }
}

function Get-GitEvidence {
    $head = (& git -C $repoRoot rev-parse HEAD 2>$null).Trim()
    $tree = (& git -C $repoRoot rev-parse 'HEAD^{tree}' 2>$null).Trim()
    $status = @(& git -C $repoRoot status --porcelain=v1 --untracked-files=all 2>$null)
    [ordered]@{
        head = if ([string]::IsNullOrWhiteSpace($head)) { "unavailable" } else { $head }
        tree = if ([string]::IsNullOrWhiteSpace($tree)) { "unavailable" } else { $tree }
        dirty = $status.Count -ne 0
        status = $status
    }
}

function Get-EnvironmentSnapshot {
    $gpu = $null
    $gpuError = $null
    try {
        $gpu = Get-CimInstance Win32_VideoController -ErrorAction Stop |
            Where-Object { $_.Name -match "NVIDIA" } |
            Select-Object -First 1
    } catch {
        $gpuError = $_.Exception.Message
    }
    $powerSource = "N/A"
    try {
        if (-not ("Stage11FgAcceptance.Native.PowerStatus" -as [type])) {
            Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
namespace Stage11FgAcceptance.Native {
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
        $status = New-Object Stage11FgAcceptance.Native.SYSTEM_POWER_STATUS
        if ([Stage11FgAcceptance.Native.PowerStatus]::GetSystemPowerStatus([ref]$status)) {
            if ($status.ACLineStatus -eq 1) { $powerSource = "AC" }
            elseif ($status.ACLineStatus -eq 0) { $powerSource = "battery" }
        }
    } catch { }
    [ordered]@{
        gpu_name = if ($null -ne $gpu) { [string]$gpu.Name } else { "N/A" }
        gpu_name_source = if ($null -ne $gpu) { "Win32_VideoController.Name" } else { "unavailable" }
        driver_version = if ($null -ne $gpu) { [string]$gpu.DriverVersion } else { "N/A" }
        power_source = $powerSource
        gpu_probe_error = $gpuError
        os = [Environment]::OSVersion.VersionString
        powershell = $PSVersionTable.PSVersion.ToString()
        machine = [Environment]::MachineName
    }
}

function Get-Cases([string]$SelectedSuite, [string]$SelectedCase) {
    $all = @(
        [pscustomobject]@{ label = "feature_off"; exe_kind = "default"; configuration = "Release"; fg_on = $false; rr = $false; animated = $false; features = @(); plugin_set = $null },
        [pscustomobject]@{ label = "fg_compiled_off"; exe_kind = "fg"; configuration = "Release"; fg_on = $false; rr = $false; animated = $false; features = @("streamline", "streamline-fg"); plugin_set = "fg" },
        [pscustomobject]@{ label = "fg_quality"; exe_kind = "fg"; configuration = "Release"; fg_on = $true; rr = $false; animated = $false; features = @("streamline", "streamline-fg"); plugin_set = "fg" },
        [pscustomobject]@{ label = "fg_rr_quality"; exe_kind = "rr-fg"; configuration = "Release"; fg_on = $true; rr = $true; animated = $false; features = @("streamline", "streamline-fg", "streamline-rr"); plugin_set = "rr-fg" },
        [pscustomobject]@{ label = "fg_animated"; exe_kind = "fg"; configuration = "Release"; fg_on = $true; rr = $false; animated = $true; features = @("streamline", "streamline-fg"); plugin_set = "fg" },
        [pscustomobject]@{ label = "fg_debug"; exe_kind = "fg-debug"; configuration = "Debug"; fg_on = $true; rr = $false; animated = $false; features = @("streamline", "streamline-fg"); plugin_set = "fg" }
    )
    if (-not [string]::IsNullOrWhiteSpace($SelectedCase)) {
        return @($all | Where-Object { $_.label -eq $SelectedCase })
    }
    if ($SelectedSuite -eq "Smoke") { return @($all | Where-Object { $_.label -eq "fg_rr_quality" }) }
    if ($SelectedSuite -eq "DebugValidation") { return @($all | Where-Object { $_.label -eq "fg_debug" }) }
    return @($all | Where-Object { $_.configuration -eq "Release" })
}

function Get-BuildProvenanceFailures([object]$Json, [object]$Git, [string[]]$Features) {
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($null -eq $Git) { return @($failures) }
    $build = Get-JsonPathValue $Json "build"
    if ($null -eq $build) { return @("benchmark JSON is missing build provenance") }
    if ([string](Get-JsonPathValue $build "git_head") -ne [string]$Git.head) {
        $failures.Add("embedded git_head does not match runner checkout")
    }
    if ([string](Get-JsonPathValue $build "git_tree") -ne [string]$Git.tree) {
        $failures.Add("embedded git_tree does not match runner checkout")
    }
    if ([bool](Get-JsonPathValue $build "git_dirty")) {
        $failures.Add("executable was built from a dirty worktree")
    }
    $actual = @((Get-JsonPathValue $build "features") | ForEach-Object { [string]$_ } | Sort-Object)
    $expected = @($Features | Sort-Object)
    if (($actual -join ',') -ne ($expected -join ',')) {
        $failures.Add("embedded features mismatch: expected=$($expected -join ',') actual=$($actual -join ',')")
    }
    return @($failures)
}

function Test-ActiveGpuPass([object]$Json, [string]$Name) {
    $pass = Get-JsonPathValue $Json "passes.$Name"
    if ($null -eq $pass) { return $false }
    $samples = Get-JsonPathValue $pass "valid_samples"
    return $null -ne $samples -and [uint64]$samples -gt 0 -and
        (Test-Finite (Get-JsonPathValue $pass "p95_ms"))
}

function Get-FgGateFailures([object]$Result, [object]$Case, [object]$Git = $null) {
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($Result.exit_code -ne 0) { $failures.Add("exit_code=$($Result.exit_code)") }
    if ($Result.timed_out) { $failures.Add("timeout after $($Result.timeout_seconds) seconds") }
    if (-not $Result.stdout_exact_one_json) { $failures.Add([string]$Result.json_error) }
    if ($null -eq $Result.json) { return @($failures) }

    try {
        $json = $Result.json
        foreach ($failure in @(Get-BuildProvenanceFailures $json $Git $Case.features)) {
            $failures.Add($failure)
        }
        if ([uint32](Get-JsonPathValue $json "schema_version") -lt 3) {
            $failures.Add("benchmark schema_version must be at least 3")
        }
        if ([string](Get-JsonPathValue $json "gpu_name") -notmatch "RTX 4060 Laptop") {
            $failures.Add("GPU must be NVIDIA RTX 4060 Laptop")
        }
        if ([uint64](Get-JsonPathValue $json "valid_samples") -eq 0 -or
            -not (Test-Finite (Get-JsonPathValue $json "passes.total.p95_ms"))) {
            $failures.Add("GPU Total must contain valid timing samples")
        }
        if ([uint64](Get-JsonPathValue $json "gpu_idle_wait_count") -ne 0) {
            $failures.Add("steady-state gpu_idle_wait_count must be zero")
        }

        $fg = Get-JsonPathValue $json "frame_generation"
        if ($null -eq $fg) {
            $failures.Add("frame_generation object is missing")
            return @($failures)
        }
        $appValue = Get-JsonPathValue $fg "application_frames"
        $displayedValue = Get-JsonPathValue $fg "displayed_frames"
        $generatedValue = Get-JsonPathValue $fg "generated_frames"
        $droppedValue = Get-JsonPathValue $fg "dropped_generated_frames"
        if ($null -eq $appValue -or $null -eq $displayedValue -or $null -eq $generatedValue -or $null -eq $droppedValue) {
            $failures.Add("frame_generation counters are incomplete")
            return @($failures)
        }
        $application = [uint64]$appValue
        $displayed = [uint64]$displayedValue
        $generated = [uint64]$generatedValue
        $dropped = [uint64]$droppedValue
        if ($application -eq 0) { $failures.Add("application_frames must be nonzero") }
        if ($displayed -lt $application) { $failures.Add("displayed_frames must be at least application_frames") }
        if ($displayed -ge $application -and $generated -ne ($displayed - $application)) {
            $failures.Add("generated_frames must equal displayed_frames - application_frames")
        }
        foreach ($rateName in @("base_fps", "display_fps", "actual_presented_multiplier")) {
            if (-not (Test-Finite (Get-JsonPathValue $fg $rateName))) {
                $failures.Add("$rateName must be finite")
            }
        }
        if ($application -gt 0) {
            $expectedMultiplier = [double]$displayed / [double]$application
            $reportedMultiplier = [double](Get-JsonPathValue $fg "actual_presented_multiplier")
            if ([Math]::Abs($reportedMultiplier - $expectedMultiplier) -gt 0.001) {
                $failures.Add("actual_presented_multiplier does not match exact counters")
            }
        }

        $reflex = Get-JsonPathValue $json "reflex"
        if ($Case.exe_kind -eq "default") {
            if ([bool](Get-JsonPathValue $fg "compiled") -or
                [string](Get-JsonPathValue $fg "requested") -ne "off" -or
                [string](Get-JsonPathValue $fg "active") -ne "unavailable") {
                $failures.Add("feature-off FG state must be compiled=false/requested=off/active=unavailable")
            }
            if ([bool](Get-JsonPathValue $reflex "compiled")) {
                $failures.Add("feature-off executable must not compile Streamline")
            }
        } else {
            if (-not [bool](Get-JsonPathValue $fg "compiled") -or
                -not [bool](Get-JsonPathValue $fg "supported")) {
                $failures.Add("FG must be compiled and supported")
            }
            $tokenCount = [uint64](Get-JsonPathValue $reflex "token_count")
            if ($tokenCount -ne $application) {
                $failures.Add("Reflex token_count must equal application_frames")
            }
            if ([uint64](Get-JsonPathValue $reflex "present_common_count") -ne $application) {
                $failures.Add("presentCommon count must equal application_frames")
            }
            if ([uint64](Get-JsonPathValue $reflex "sleep_count") -ne $application) {
                $failures.Add("Reflex sleep_count must equal application_frames")
            }
            foreach ($marker in @("simulation_start", "simulation_end", "render_submit_start", "render_submit_end", "present_start", "present_end")) {
                if ([uint64](Get-JsonPathValue $reflex "marker_counts.$marker") -ne $application) {
                    $failures.Add("PCL marker $marker must equal application_frames")
                }
            }
            if ([uint64](Get-JsonPathValue $reflex "order_errors") -ne 0) {
                $failures.Add("Reflex/PCL order_errors must be zero")
            }
            if ([uint64](Get-JsonPathValue $reflex "latency_query_failures") -ne 0) {
                $failures.Add("Reflex latency query must not fail")
            }
            $reportAvailable = [bool](Get-JsonPathValue $reflex "report_available")
            $latency = Get-JsonPathValue $reflex "latency"
            if ($reportAvailable -and $null -eq $latency) {
                $failures.Add("Reflex report_available=true requires latency data")
            } elseif (-not $reportAvailable -and $null -ne $latency) {
                $failures.Add("Reflex unavailable report must remain null")
            } elseif ($null -ne $latency) {
                if (-not (Test-Finite (Get-JsonPathValue $latency "simulation_to_present_ms"))) {
                    $failures.Add("Reflex simulation_to_present_ms must be finite when available")
                }
                if ([string](Get-JsonPathValue $latency "scope") -ne "application-frame; not scan-out/display latency") {
                    $failures.Add("Reflex latency scope is ambiguous")
                }
            }
        }

        if (-not $Case.fg_on) {
            $expectedActive = if ($Case.exe_kind -eq "default") { "unavailable" } else { "off" }
            $expectedLifecycle = if ($Case.exe_kind -eq "default") { "unavailable" } else { "off-native" }
            if ([string](Get-JsonPathValue $fg "requested") -ne "off" -or
                [string](Get-JsonPathValue $fg "active") -ne $expectedActive -or
                [string](Get-JsonPathValue $fg "lifecycle") -ne $expectedLifecycle) {
                $failures.Add("FG-off requested/active/lifecycle state is incorrect")
            }
            if ($displayed -ne $application -or $generated -ne 0 -or $dropped -ne 0) {
                $failures.Add("FG-off counters must remain one displayed frame per application frame")
            }
            if ($Case.exe_kind -eq "fg" -and [string]$Result.stderr_raw -notmatch
                "streamline_swap_chain[^\r\n]*fg_loaded=0") {
                $failures.Add("compiled-off case lacks fg_loaded=0 swap-chain evidence")
            }
        } else {
            if ([string](Get-JsonPathValue $fg "requested") -ne "on" -or
                [string](Get-JsonPathValue $fg "active") -ne "on" -or
                [string](Get-JsonPathValue $fg "lifecycle") -ne "on-proxy") {
                $failures.Add("FG-on requested/active/lifecycle state is incorrect")
            }
            if ([uint32](Get-JsonPathValue $fg "support_result_raw") -ne 0 -or
                [uint32](Get-JsonPathValue $fg "status_raw") -ne 0 -or
                [uint32](Get-JsonPathValue $fg "max_generated_frames") -lt 1 -or
                [uint32](Get-JsonPathValue $fg "requested_generated_frames") -ne 1) {
                $failures.Add("FG-on support/status/capability fields are invalid")
            }
            if ($generated -eq 0 -or $displayed -le $application) {
                $failures.Add("FG-on case did not produce a generated frame")
            }
            $expectedDropped = (2 * $application) - $displayed
            if ($dropped -ne $expectedDropped) {
                $failures.Add("dropped_generated_frames does not match requested 2x display")
            }
            if (-not $Result.focus_succeeded) {
                $failures.Add("runner could not foreground the FG window")
            }
            if ([string]$Result.stderr_raw -notmatch "frame_generation_confirmed[^\r\n]*num_frames_actually_presented=[2-9]") {
                $failures.Add("stderr lacks frame_generation_confirmed actual-presented evidence")
            }
            if ([string]$Result.stderr_raw -notmatch "frame_generation_state[^\r\n]*actual_presented=[2-9][^\r\n]*focused=1") {
                $failures.Add("stderr lacks focused=1 generated-frame state evidence")
            }
        }

        if ($Case.rr) {
            if ([string](Get-JsonPathValue $json "denoiser.active") -ne "dlss-rr" -or
                -not [bool](Get-JsonPathValue $json "dlss_rr.supported") -or
                -not (Test-ActiveGpuPass $json "rr_evaluate")) {
                $failures.Add("RR+FG case must keep Ray Reconstruction active")
            }
        }
        if ($Case.animated -and -not [bool](Get-JsonPathValue $json "acceleration_structures.tlas_update_enabled")) {
            $failures.Add("animated FG case must use TLAS update")
        }

        $stderr = [string]$Result.stderr_raw
        if ($stderr -match "(?i)device removed|silent fallback|静默回退|NaN|Infinity") {
            $failures.Add("stderr contains device removal, invalid value, or silent fallback evidence")
        }
        if ($Case.configuration -eq "Debug") {
            $emptyQueue = $stderr -match 'D3D12 Debug InfoQueue[^\r\n]*0\s*条消息'
            $zeroSeverities = $stderr -match 'CORRUPTION\s+0.*ERROR\s+0'
            if (-not ($emptyQueue -or $zeroSeverities)) {
                $failures.Add("Debug InfoQueue must report CORRUPTION 0 and ERROR 0")
            }
        }
    } catch {
        $failures.Add("benchmark JSON gate evaluation failed: $($_.Exception.Message)")
    }
    return @($failures)
}

function Get-DeploymentFailures([string]$Executable, [object]$Case, [string]$ExpectedFlavor) {
    $failures = [System.Collections.Generic.List[string]]::new()
    $directory = Split-Path -Parent $Executable
    $known = @(
        "sl.interposer.dll", "sl.common.dll", "sl.dlss.dll", "sl.reflex.dll", "sl.pcl.dll",
        "nvngx_dlss.dll", "sl.dlss_d.dll", "nvngx_dlssd.dll", "sl.dlss_g.dll", "nvngx_dlssg.dll"
    )
    if ($Case.exe_kind -eq "default") {
        $deployed = @(Get-ChildItem -LiteralPath $directory -File -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -in $known })
        if ($deployed.Count -ne 0) {
            $failures.Add("feature-off executable directory contains Streamline DLLs")
        }
        return @($failures)
    }

    $lockPath = Join-Path $repoRoot "third_party/streamline/version.lock.json"
    $lock = Get-Content -LiteralPath $lockPath -Raw | ConvertFrom-Json -Depth 20
    if ([string]$lock.version -ne "2.14.1" -or [string]$lock.tag -ne "v2.14.1") {
        $failures.Add("Streamline lock must be v2.14.1")
    }
    $required = @("sl.common.dll", "sl.dlss.dll", "sl.reflex.dll", "sl.pcl.dll", "nvngx_dlss.dll", "sl.dlss_g.dll", "nvngx_dlssg.dll")
    if ($Case.rr) { $required += @("sl.dlss_d.dll", "nvngx_dlssd.dll") }
    $selectedDirectory = Join-Path $directory "streamline-plugins/$($Case.plugin_set)"
    if (-not (Test-Path -LiteralPath $selectedDirectory)) {
        $failures.Add("selected Streamline plugin set is missing: $selectedDirectory")
        return @($failures)
    }
    $files = @((Join-Path $directory "sl.interposer.dll")) + @($required | ForEach-Object { Join-Path $selectedDirectory $_ })
    foreach ($file in $files) {
        if (-not (Test-Path -LiteralPath $file)) {
            $failures.Add("required runtime DLL is missing: $([IO.Path]::GetFileName($file))")
            continue
        }
        $hash = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
        $matches = @($lock.files | Where-Object {
            [IO.Path]::GetFileName([string]$_.path) -eq [IO.Path]::GetFileName($file) -and
            [string]$_.sha256 -eq $hash
        })
        if ($matches.Count -eq 0) {
            $failures.Add("runtime DLL hash is not locked: $([IO.Path]::GetFileName($file))")
            continue
        }
        $flavors = @($matches | ForEach-Object {
            if ([string]$_.path -match '[/\\]development[/\\]') { "development" } else { "production" }
        } | Sort-Object -Unique)
        if ($ExpectedFlavor -notin $flavors) {
            $failures.Add("runtime DLL flavor mismatch: $([IO.Path]::GetFileName($file)) expected=$ExpectedFlavor actual=$($flavors -join ',')")
        }
    }
    if (-not $Case.rr) {
        foreach ($forbidden in @("sl.dlss_d.dll", "nvngx_dlssd.dll")) {
            if (Test-Path -LiteralPath (Join-Path $selectedDirectory $forbidden)) {
                $failures.Add("FG-only plugin set contains RR DLL $forbidden")
            }
        }
    }
    return @($failures)
}

function New-SelfTestJson([object]$Case) {
    $application = 120
    $displayed = if ($Case.fg_on) { 232 } else { 120 }
    $generated = $displayed - $application
    $compiled = $Case.exe_kind -ne "default"
    $activePass = [pscustomobject]@{ valid_samples = 120; p50_ms = 0.1; p95_ms = 0.2 }
    [pscustomobject]@{
        schema_version = 3
        build = [pscustomobject]@{ git_head = "self"; git_tree = "tree"; git_dirty = $false; features = $Case.features }
        gpu_name = "NVIDIA GeForce RTX 4060 Laptop GPU"
        valid_samples = 120
        gpu_idle_wait_count = 0
        passes = [pscustomobject]@{ total = [pscustomobject]@{ valid_samples = 120; p50_ms = 4.0; p95_ms = 5.0 }; rr_evaluate = $activePass }
        denoiser = [pscustomobject]@{ active = if ($Case.rr) { "dlss-rr" } else { "svgf" } }
        dlss_rr = [pscustomobject]@{ supported = [bool]$Case.rr }
        acceleration_structures = [pscustomobject]@{ tlas_update_enabled = [bool]$Case.animated }
        frame_generation = [pscustomobject]@{
            compiled = $compiled
            supported = $compiled
            requested = if ($Case.fg_on) { "on" } else { "off" }
            active = if (-not $compiled) { "unavailable" } elseif ($Case.fg_on) { "on" } else { "off" }
            lifecycle = if (-not $compiled) { "unavailable" } elseif ($Case.fg_on) { "on-proxy" } else { "off-native" }
            support_result_raw = if ($compiled) { 0 } else { $null }
            status_raw = if ($Case.fg_on) { 0 } else { $null }
            requested_generated_frames = if ($Case.fg_on) { 1 } else { 0 }
            max_generated_frames = if ($compiled) { 1 } else { $null }
            application_frames = $application
            displayed_frames = $displayed
            generated_frames = $generated
            dropped_generated_frames = if ($Case.fg_on) { 8 } else { 0 }
            actual_presented_multiplier = [double]$displayed / $application
            base_fps = 120.0
            display_fps = [double]$displayed
        }
        reflex = [pscustomobject]@{
            compiled = $compiled
            token_count = if ($compiled) { $application } else { 0 }
            sleep_count = if ($compiled) { $application } else { 0 }
            present_common_count = if ($compiled) { $application } else { 0 }
            marker_counts = [pscustomobject]@{
                simulation_start = if ($compiled) { $application } else { 0 }
                simulation_end = if ($compiled) { $application } else { 0 }
                render_submit_start = if ($compiled) { $application } else { 0 }
                render_submit_end = if ($compiled) { $application } else { 0 }
                present_start = if ($compiled) { $application } else { 0 }
                present_end = if ($compiled) { $application } else { 0 }
            }
            order_errors = 0
            report_available = $compiled
            latency = if ($compiled) { [pscustomobject]@{
                simulation_to_present_ms = 4.5
                scope = "application-frame; not scan-out/display latency"
            } } else { $null }
            latency_query_failures = 0
        }
    }
}

function New-SelfTestResult([object]$Json, [object]$Case, [string]$StderrOverride = "") {
    $stderr = if (-not [string]::IsNullOrWhiteSpace($StderrOverride)) {
        $StderrOverride
    } elseif ($Case.fg_on) {
        "frame_generation_state status=0 actual_presented=2 max_generated=1 focused=1`nframe_generation_confirmed status=0 num_frames_actually_presented=2"
    } else {
        "streamline_swap_chain state=created proxy=1 fg_loaded=0"
    }
    [pscustomobject]@{
        exit_code = 0
        timed_out = $false
        timeout_seconds = 60
        stdout_exact_one_json = $true
        json_error = $null
        json = $Json
        stderr_raw = $stderr
        focus_succeeded = [bool]$Case.fg_on
    }
}

function Invoke-SelfTest {
    $cases = Get-Cases "Matrix" ""
    $cases += Get-Cases "DebugValidation" ""
    foreach ($case in $cases) {
        $stderr = if ($case.configuration -eq "Debug") {
            "frame_generation_state status=0 actual_presented=2 max_generated=1 focused=1`nframe_generation_confirmed status=0 num_frames_actually_presented=2`nCORRUPTION 0 ERROR 0"
        } else { "" }
        $failures = @(Get-FgGateFailures (New-SelfTestResult (New-SelfTestJson $case) $case $stderr) $case)
        if ($failures.Count -ne 0) { throw "SelfTest rejected valid $($case.label): $($failures -join '; ')" }
    }

    $rejections = [System.Collections.Generic.List[string]]::new()
    $onCase = (Get-Cases "Smoke" "")[0]
    $json = New-SelfTestJson $onCase
    $json.frame_generation.displayed_frames = 120
    $json.frame_generation.generated_frames = 0
    $json.frame_generation.dropped_generated_frames = 120
    $json.frame_generation.actual_presented_multiplier = 1.0
    if (@(Get-FgGateFailures (New-SelfTestResult $json $onCase) $onCase).Count -eq 0) { throw "SelfTest accepted FG-on without generated frames" }
    $rejections.Add("no_generated_frame")

    $json = New-SelfTestJson $onCase
    $json.reflex.marker_counts.present_end = 232
    if (@(Get-FgGateFailures (New-SelfTestResult $json $onCase) $onCase).Count -eq 0) { throw "SelfTest accepted display-frame PCL accounting" }
    $rejections.Add("marker_counted_display_frames")

    $json = New-SelfTestJson $onCase
    $json.frame_generation.actual_presented_multiplier = 2.0
    if (@(Get-FgGateFailures (New-SelfTestResult $json $onCase) $onCase).Count -eq 0) { throw "SelfTest accepted multiplier disconnected from counters" }
    $rejections.Add("invented_multiplier")

    $offCase = (Get-Cases "Matrix" "fg_compiled_off")[0]
    $json = New-SelfTestJson $offCase
    $json.frame_generation.lifecycle = "on-proxy"
    if (@(Get-FgGateFailures (New-SelfTestResult $json $offCase) $offCase).Count -eq 0) { throw "SelfTest accepted proxy lifecycle while FG is off" }
    $rejections.Add("compiled_off_proxy")

    $json = New-SelfTestJson $onCase
    $json.gpu_idle_wait_count = 1
    if (@(Get-FgGateFailures (New-SelfTestResult $json $onCase) $onCase).Count -eq 0) { throw "SelfTest accepted steady-state idle wait" }
    $rejections.Add("steady_state_idle_wait")

    $json = New-SelfTestJson $onCase
    $json.denoiser.active = "svgf"
    if (@(Get-FgGateFailures (New-SelfTestResult $json $onCase) $onCase).Count -eq 0) { throw "SelfTest accepted inactive RR in RR+FG case" }
    $rejections.Add("rr_inactive")

    $animatedCase = (Get-Cases "Matrix" "fg_animated")[0]
    $json = New-SelfTestJson $animatedCase
    $json.acceleration_structures.tlas_update_enabled = $false
    if (@(Get-FgGateFailures (New-SelfTestResult $json $animatedCase) $animatedCase).Count -eq 0) { throw "SelfTest accepted static TLAS in animated case" }
    $rejections.Add("animated_without_tlas_update")

    $exact = Get-ExactJsonLine "{`"ok`":true}`nextra"
    if ($exact.exact) { throw "SelfTest accepted extra stdout" }
    $rejections.Add("extra_stdout")

    [ordered]@{ self_test = "passed"; gpu_started = $false; rejected_cases = @($rejections) } |
        ConvertTo-Json -Compress -Depth 10
}

if ($SelfTest) {
    Write-Output (Invoke-SelfTest)
    exit 0
}

$cases = @(Get-Cases $Suite $CaseName)
if ($cases.Count -eq 0) { throw "no cases selected" }
if ($Suite -eq "DebugValidation" -and -not [string]::IsNullOrWhiteSpace($CaseName)) {
    throw "-CaseName cannot be combined with -Suite DebugValidation"
}

$paths = [ordered]@{
    default = Resolve-RepoPath $DefaultExe
    fg = Resolve-RepoPath $FgExe
    "rr-fg" = Resolve-RepoPath $(if ([string]::IsNullOrWhiteSpace($RrFgExe) -and $Suite -eq "Smoke") { "target/release/ray_tracing_demo.exe" } else { $RrFgExe })
    "fg-debug" = Resolve-RepoPath $(if ([string]::IsNullOrWhiteSpace($FgDebugExe)) { $FgExe } else { $FgDebugExe })
}
foreach ($case in $cases) {
    $path = $paths[$case.exe_kind]
    if ($null -eq $path -or -not (Test-Path -LiteralPath $path)) {
        throw "missing executable for $($case.label); pass the matching -DefaultExe/-FgExe/-RrFgExe/-FgDebugExe"
    }
}

$runId = "$(Get-Date -Format yyyyMMdd-HHmmss)-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
$runBase = if ([IO.Path]::IsPathRooted($OutputRoot)) { $OutputRoot } else { Join-Path $repoRoot $OutputRoot }
$runRoot = Join-Path $runBase $runId
New-Item -ItemType Directory -Path $runRoot -Force | Out-Null
$git = Get-GitEvidence
$environment = Get-EnvironmentSnapshot
$allFailures = [System.Collections.Generic.List[string]]::new()
$records = [System.Collections.Generic.List[object]]::new()
$deployment = [System.Collections.Generic.List[object]]::new()

if ($git.dirty) { $allFailures.Add("working tree is dirty; formal acceptance requires a clean committed tree") }
if ($git.head -eq "unavailable" -or $git.tree -eq "unavailable") { $allFailures.Add("Git HEAD/tree evidence is unavailable") }

$seenDeployments = @{}
foreach ($case in $cases) {
    $exe = [string]$paths[$case.exe_kind]
    $key = "$exe|$($case.plugin_set)|$($case.configuration)"
    if ($seenDeployments.ContainsKey($key)) { continue }
    $seenDeployments[$key] = $true
    $flavor = if ($case.configuration -eq "Debug") { "development" } else { "production" }
    $failures = @(Get-DeploymentFailures $exe $case $flavor)
    $deployment.Add([ordered]@{
        executable = $exe
        sha256 = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash
        plugin_set = $case.plugin_set
        expected_flavor = if ($case.exe_kind -eq "default") { $null } else { $flavor }
        failures = $failures
    })
    foreach ($failure in $failures) { $allFailures.Add("deployment $($case.label): $failure") }
}

if (@($cases | Where-Object { $_.fg_on }).Count -gt 0 -and -not ("Stage11FgAcceptance.Native.WindowFocus" -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
namespace Stage11FgAcceptance.Native {
    public static class WindowFocus {
        [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
        [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr hWnd, int nCmdShow);
    }
}
"@
}

foreach ($case in $cases) {
    $caseRoot = Join-Path $runRoot $case.label
    New-Item -ItemType Directory -Path $caseRoot -Force | Out-Null
    $stdoutPath = Join-Path $caseRoot "$($case.label).stdout.txt"
    $stderrPath = Join-Path $caseRoot "$($case.label).stderr.txt"
    $argsPath = Join-Path $caseRoot "$($case.label).args.txt"
    $exitPath = Join-Path $caseRoot "$($case.label).exit.json"
    $exe = [string]$paths[$case.exe_kind]
    $arguments = @(
        "--benchmark-seconds", [string]$Seconds,
        "--output-size", $(if ($case.configuration -eq "Debug") { "1280x720" } else { "1920x1080" }),
        "--denoiser", $(if ($case.rr) { "dlss-rr" } else { "svgf" }),
        "--upscaler", $(if ($case.exe_kind -eq "default") { "native" } else { "dlss-quality" }),
        "--frame-generation", $(if ($case.fg_on) { "on" } else { "off" }),
        "--reflex-mode", $(if ($case.exe_kind -eq "default") { "off" } else { "on" }),
        "--atrous-mode", "baseline",
        "--command-recording-mode", "optimized",
        "--acceleration-structure-mode", "baseline"
    )
    if ($case.rr) { $arguments += @("--path-space-mode", "stable-planes") }
    if ($case.animated) {
        $arguments += @("--model", (Resolve-RepoPath "assets/gltf/Triangle/Triangle.gltf"), "--animate-model")
    }
    if ($StreamlineApplicationId -ne 0 -and $case.exe_kind -ne "default") {
        $arguments += @("--streamline-application-id", [string]$StreamlineApplicationId)
    }
    $command = "$(Quote-Argument $exe) " + (($arguments | ForEach-Object { Quote-Argument $_ }) -join " ")
    Set-Content -LiteralPath $argsPath -Value $command -Encoding UTF8

    $started = Get-Date
    $exitCode = -1
    $timedOut = $false
    $launchError = $null
    $focusAttempted = $false
    $focusSucceeded = -not $case.fg_on
    $process = $null
    try {
        # Keep D3D process creation at script scope. FG cases must be visible
        # and foreground because the driver intentionally stops interpolation
        # for an unfocused window; a hidden smoke cannot prove generated frames.
        $process = Start-Process -FilePath $exe `
            -ArgumentList (($arguments | ForEach-Object { Quote-Argument $_ }) -join " ") `
            -RedirectStandardOutput $stdoutPath `
            -RedirectStandardError $stderrPath `
            -PassThru -WindowStyle $(if ($case.fg_on) { "Normal" } else { "Hidden" })
        if ($case.fg_on) {
            $focusAttempted = $true
            $deadline = [DateTime]::UtcNow.AddSeconds(5)
            while ([DateTime]::UtcNow -lt $deadline -and -not $process.HasExited -and -not $focusSucceeded) {
                $process.Refresh()
                if ($process.MainWindowHandle -ne [IntPtr]::Zero) {
                    [Stage11FgAcceptance.Native.WindowFocus]::ShowWindowAsync($process.MainWindowHandle, 9) | Out-Null
                    $focusSucceeded = [Stage11FgAcceptance.Native.WindowFocus]::SetForegroundWindow($process.MainWindowHandle)
                }
                if (-not $focusSucceeded) { Start-Sleep -Milliseconds 50 }
            }
        }
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $timedOut = $true
            try { $process.Kill($true) } catch {
                try { & taskkill.exe /PID $process.Id /T /F 2>$null | Out-Null } catch { }
            }
            $process.WaitForExit(10000) | Out-Null
        } else {
            $process.Refresh()
            if ($process.HasExited) { $exitCode = [int]$process.ExitCode }
        }
    } catch {
        $launchError = $_.Exception.Message
        Set-Content -LiteralPath $stderrPath -Value $launchError -Encoding UTF8
    } finally {
        if ($null -ne $process) { $process.Dispose() }
    }

    $stdout = if (Test-Path -LiteralPath $stdoutPath) { Get-Content -LiteralPath $stdoutPath -Raw } else { "" }
    $stderr = if (Test-Path -LiteralPath $stderrPath) { Get-Content -LiteralPath $stderrPath -Raw } else { "" }
    $parsed = Get-ExactJsonLine $stdout
    $jsonError = if ($null -ne $launchError) { $launchError } elseif (-not $parsed.exact) { $parsed.error } else { $null }
    $shutdownBlocked = $timedOut -and $parsed.exact -and $stderr -match "(?i)Shutting down NGX|Telemetry Shutdown Data"
    $record = [pscustomobject]@{
        label = $case.label
        command = $command
        arguments = $arguments
        executable = $exe
        executable_sha256 = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash
        args_path = $argsPath
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        exit_path = $exitPath
        exit_code = $exitCode
        timed_out = $timedOut
        timeout_seconds = $TimeoutSeconds
        shutdown_blocked_after_json = $shutdownBlocked
        elapsed_seconds = ((Get-Date) - $started).TotalSeconds
        focus_attempted = $focusAttempted
        focus_succeeded = $focusSucceeded
        stdout_exact_one_json = $parsed.exact
        stdout_line_count = $parsed.line_count
        parseable_json_lines = $parsed.parseable_json_lines
        json = $parsed.json
        json_error = $jsonError
        stdout_raw = $stdout
        stderr_raw = $stderr
    }
    $gateFailures = @(Get-FgGateFailures $record $case $git)
    $record | Add-Member -NotePropertyName gate_failures -NotePropertyValue $gateFailures
    $record | ConvertTo-Json -Depth 60 | Set-Content -LiteralPath $exitPath -Encoding UTF8
    $records.Add($record)
    foreach ($failure in $gateFailures) { $allFailures.Add("$($case.label): $failure") }
}

$summary = [ordered]@{
    schema_version = 1
    stage = "11G-E"
    run_id = $runId
    suite = $Suite
    case_name = if ([string]::IsNullOrWhiteSpace($CaseName)) { $null } else { $CaseName }
    seconds = $Seconds
    timeout_seconds = $TimeoutSeconds
    git = $git
    environment = $environment
    deployment = @($deployment)
    cases = @($records | ForEach-Object {
        [ordered]@{
            label = $_.label
            executable = $_.executable
            executable_sha256 = $_.executable_sha256
            command = $_.command
            stdout_path = $_.stdout_path
            stderr_path = $_.stderr_path
            exit_path = $_.exit_path
            exit_code = $_.exit_code
            timed_out = $_.timed_out
            shutdown_blocked_after_json = $_.shutdown_blocked_after_json
            focus_attempted = $_.focus_attempted
            focus_succeeded = $_.focus_succeeded
            json = $_.json
            gate_failures = @($_.gate_failures)
        }
    })
    automatic_pass = $allFailures.Count -eq 0
    failures = @($allFailures)
    manual_gates = @(
        [ordered]@{ name = "f5_on_off_twice"; status = "PENDING"; evidence = $null },
        [ordered]@{ name = "resize_minimize_restore_exit"; status = "PENDING"; evidence = $null },
        [ordered]@{ name = "camera_and_animation_visual_quality"; status = "PENDING"; evidence = $null },
        [ordered]@{ name = "frameview_ms_between_display_change"; status = "PENDING"; evidence = $null },
        [ordered]@{ name = "external_reflex_latency"; status = "PENDING"; evidence = $null }
    )
    stage_complete = $false
    note = "Automatic workloads are bounded to 1..5 seconds. Window FPS is not external display-pacing evidence; manual gates remain explicit."
}
$summaryPath = Join-Path $runRoot "summary.json"
$summary | ConvertTo-Json -Depth 60 | Set-Content -LiteralPath $summaryPath -Encoding UTF8
Write-Output $summaryPath
if ($allFailures.Count -gt 0) { exit 1 }
