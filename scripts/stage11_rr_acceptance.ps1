[CmdletBinding()]
param(
    [ValidateSet("Smoke", "Matrix", "DebugValidation")]
    [string]$Suite = "Smoke",
    [ValidateSet("", "rr_quality", "rr_balanced", "rr_performance", "rr_animated")]
    [string]$CaseName = "",
    [string]$RrExe,
    [string]$RrDebugExe,
    [string]$DefaultExe,
    [string]$NrdExe,
    [ValidateRange(1, 5)]
    [int]$Seconds = 1,
    [ValidateRange(10, 60)]
    [int]$TimeoutSeconds = 60,
    [uint32]$StreamlineApplicationId = 0,
    [switch]$SelfTest,
    [string]$OutputRoot = "output/stage11"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

function Quote-Argument([string]$Value) {
    if ($Value -notmatch '[\s"]') {
        return $Value
    }
    return '"' + $Value.Replace('"', '\"') + '"'
}

function Resolve-RepoPath([string]$PathValue) {
    if ([string]::IsNullOrWhiteSpace($PathValue)) {
        return $null
    }
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

function Get-GitEvidence {
    $head = (& git -C $repoRoot rev-parse HEAD 2>$null).Trim()
    $tree = (& git -C $repoRoot rev-parse 'HEAD^{tree}' 2>$null).Trim()
    $statusLines = @(& git -C $repoRoot status --porcelain=v1 --untracked-files=all 2>$null)
    $untrackedDirty = @($statusLines | Where-Object { $_.StartsWith("??") }).Count -gt 0
    $indexDirty = @($statusLines | Where-Object {
        $_.Length -ge 2 -and $_[0] -ne ' ' -and $_[0] -ne '?'
    }).Count -gt 0
    $worktreeDirty = @($statusLines | Where-Object {
        $_.Length -ge 2 -and (($_[1] -ne ' ' -and $_[1] -ne '?') -or $_.StartsWith("??"))
    }).Count -gt 0
    [ordered]@{
        head = if ([string]::IsNullOrWhiteSpace($head)) { "unavailable" } else { $head }
        tree = if ([string]::IsNullOrWhiteSpace($tree)) { "unavailable" } else { $tree }
        dirty = @($statusLines).Count -gt 0 -or [string]::IsNullOrWhiteSpace($head) -or [string]::IsNullOrWhiteSpace($tree)
        worktree_dirty = $worktreeDirty
        index_dirty = $indexDirty
        untracked_dirty = $untrackedDirty
        status = @($statusLines)
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

    $driverVersion = "N/A"
    $driverSource = "unavailable"
    $driverError = $null
    if ($null -ne $gpu -and -not [string]::IsNullOrWhiteSpace([string]$gpu.DriverVersion)) {
        $driverVersion = [string]$gpu.DriverVersion
        $driverSource = "Win32_VideoController.DriverVersion"
    } else {
        try {
            $smi = @(& nvidia-smi --query-gpu=driver_version --format=csv,noheader,nounits 2>&1 |
                ForEach-Object { [string]$_ } |
                Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
            if (@($smi).Count -gt 0 -and $smi[0] -notmatch "not recognized|failed|error") {
                $driverVersion = $smi[0].Trim()
                $driverSource = "nvidia-smi"
            } else {
                $driverError = ($smi -join " ").Trim()
            }
        } catch {
            $driverError = $_.Exception.Message
        }
    }

    $powerPlan = "N/A"
    $powerPlanSource = "unavailable"
    $powerPlanError = $null
    try {
        $powerOutput = @(& powercfg /getactivescheme 2>&1 | ForEach-Object { [string]$_ })
        $powerPlan = ($powerOutput -join " ").Trim()
        if (-not [string]::IsNullOrWhiteSpace($powerPlan)) {
            $powerPlanSource = "powercfg /getactivescheme"
        } else {
            $powerPlanError = "powercfg returned no output"
        }
    } catch {
        $powerPlanError = $_.Exception.Message
    }

    $powerSource = "N/A"
    $powerSourceProbe = "unavailable"
    $powerSourceError = $null
    try {
        if (-not ("Stage11Acceptance.Native.PowerStatus" -as [type])) {
            Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
namespace Stage11Acceptance.Native {
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
        $status = New-Object Stage11Acceptance.Native.SYSTEM_POWER_STATUS
        if ([Stage11Acceptance.Native.PowerStatus]::GetSystemPowerStatus([ref]$status)) {
            $powerSourceProbe = "GetSystemPowerStatus"
            if ($status.ACLineStatus -eq 1) {
                $powerSource = "AC"
            } elseif ($status.ACLineStatus -eq 0) {
                $powerSource = "battery"
            }
        } else {
            $powerSourceError = "GetSystemPowerStatus returned false"
        }
    } catch {
        $powerSourceError = $_.Exception.Message
    }
    if ($powerSource -eq "N/A") {
        try {
            $battery = Get-CimInstance Win32_Battery -ErrorAction Stop | Select-Object -First 1
            if ($null -ne $battery) {
                $powerSourceProbe = "Win32_Battery.BatteryStatus"
                if ([int]$battery.BatteryStatus -eq 2) {
                    $powerSource = "AC"
                } elseif ([int]$battery.BatteryStatus -in @(1, 3, 4, 5)) {
                    $powerSource = "battery"
                }
            } else {
                $powerSourceError = "Win32_Battery returned no instance"
            }
        } catch {
            $powerSourceError = $_.Exception.Message
        }
    }

    [ordered]@{
        gpu_name = if ($null -ne $gpu) { [string]$gpu.Name } else { "N/A" }
        gpu_name_source = if ($null -ne $gpu) { "Win32_VideoController.Name" } else { "unavailable" }
        driver_version = $driverVersion
        driver_version_source = $driverSource
        driver_version_error = $driverError
        active_power_scheme = $powerPlan
        active_power_scheme_source = $powerPlanSource
        active_power_scheme_error = $powerPlanError
        power_source = $powerSource
        power_source_probe = $powerSourceProbe
        power_source_error = $powerSourceError
        gpu_probe_error = $gpuError
        os = [Environment]::OSVersion.VersionString
        powershell = $PSVersionTable.PSVersion.ToString()
        machine = [Environment]::MachineName
    }
}

function Get-StreamlineSdkEvidence([string[]]$ExecutablePaths) {
    $sdkPath = Join-Path $repoRoot "external/streamline-v2.12.0"
    $lockPath = Join-Path $repoRoot "third_party/streamline/version.lock.json"
    $lock = $null
    if (Test-Path -LiteralPath $lockPath) {
        $lock = Get-Content -LiteralPath $lockPath -Raw | ConvertFrom-Json -Depth 20
    }
    $dllNames = @(
        "sl.interposer.dll", "sl.common.dll", "sl.dlss.dll", "sl.reflex.dll",
        "sl.pcl.dll", "nvngx_dlss.dll", "sl.dlss_d.dll", "nvngx_dlssd.dll"
    )
    $dlls = [System.Collections.Generic.List[object]]::new()
    foreach ($exePath in @($ExecutablePaths | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })) {
        $exeDirectory = Split-Path -Parent $exePath
        foreach ($name in $dllNames) {
            $path = Join-Path $exeDirectory $name
            if (-not (Test-Path -LiteralPath $path)) {
                continue
            }
            $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
            $flavor = "unmatched"
            if ($null -ne $lock) {
                foreach ($entry in @($lock.files)) {
                    if ([IO.Path]::GetFileName([string]$entry.path) -eq $name -and
                        [string]$entry.sha256 -eq $hash) {
                        $flavor = if ([string]$entry.path -match "/development/") { "development" } else { "production" }
                        break
                    }
                }
            }
            $dlls.Add([ordered]@{
                executable_directory = $exeDirectory
                name = $name
                path = $path
                sha256 = $hash
                flavor = $flavor
            })
        }
    }
    $uniqueFlavors = @($dlls | ForEach-Object { $_.flavor } | Sort-Object -Unique)
    [ordered]@{
        version = if ($null -ne $lock) { [string]$lock.version } else { "unavailable" }
        tag = if ($null -ne $lock) { [string]$lock.tag } else { "unavailable" }
        archive_sha256 = if ($null -ne $lock) { [string]$lock.archive_sha256 } else { $null }
        lock_path = $lockPath
        sdk_path = $sdkPath
        dll_flavors = $uniqueFlavors
        dlls = @($dlls)
    }
}

function Get-ExecutableEvidence([string]$PathValue, [string]$Feature) {
    if ([string]::IsNullOrWhiteSpace($PathValue)) {
        return $null
    }
    $path = Resolve-RepoPath $PathValue
    [ordered]@{
        expected_features = @($Feature -split ',' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Sort-Object)
        path = $path
        sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    }
}

function Get-BuildProvenanceFailures(
    [object]$Json,
    [object]$ExpectedGit,
    [string[]]$ExpectedFeatures
) {
    $failures = [System.Collections.Generic.List[string]]::new()
    $build = Get-JsonPathValue $Json "build"
    if ($null -eq $build) {
        return @("benchmark JSON is missing embedded build provenance")
    }
    if ([string](Get-JsonPathValue $build "git_head") -ne [string]$ExpectedGit.head) {
        $failures.Add("embedded build git_head does not match the runner checkout")
    }
    if ([string](Get-JsonPathValue $build "git_tree") -ne [string]$ExpectedGit.tree) {
        $failures.Add("embedded build git_tree does not match the runner checkout")
    }
    if ([bool](Get-JsonPathValue $build "git_dirty")) {
        $failures.Add("executable was built from a dirty worktree")
    }
    $actual = @((Get-JsonPathValue $build "features") | ForEach-Object { [string]$_ } | Sort-Object)
    $expected = @($ExpectedFeatures | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Sort-Object)
    if (($actual -join ',') -ne ($expected -join ',')) {
        $failures.Add("embedded build features mismatch: expected=$($expected -join ',') actual=$($actual -join ',')")
    }
    return @($failures)
}

function Get-StreamlineSdkFailures(
    [object]$Evidence,
    [object]$Executables,
    [string]$ExpectedFlavor
) {
    $failures = [System.Collections.Generic.List[string]]::new()
    if ([string]$Evidence.version -ne "2.12.0" -or [string]$Evidence.tag -ne "v2.12.0") {
        $failures.Add("Streamline lock identity is unavailable or unexpected")
    }
    $common = @(
        "sl.interposer.dll", "sl.common.dll", "sl.dlss.dll", "sl.reflex.dll",
        "sl.pcl.dll", "nvngx_dlss.dll"
    )
    $rr = @($common) + @("sl.dlss_d.dll", "nvngx_dlssd.dll")
    $expectations = @(
        [pscustomobject]@{ executable = $Executables.rr; required = $rr; label = "rr" },
        [pscustomobject]@{ executable = $Executables.default; required = @(); label = "default" },
        [pscustomobject]@{ executable = $Executables.nrd_streamline; required = $common; label = "nrd_streamline" }
    )
    foreach ($expectation in $expectations) {
        if ($null -eq $expectation.executable) { continue }
        $directory = Split-Path -Parent ([string]$expectation.executable.path)
        $deployed = @($Evidence.dlls | Where-Object { [string]$_.executable_directory -eq $directory })
        $names = @($deployed | ForEach-Object { [string]$_.name })
        foreach ($required in @($expectation.required)) {
            if ($required -notin $names) {
                $failures.Add("$($expectation.label) runtime is missing locked DLL $required")
            }
        }
        if (@($expectation.required).Count -eq 0 -and $deployed.Count -ne 0) {
            $failures.Add("default runtime contains proprietary Streamline DLLs")
        }
        foreach ($dll in $deployed) {
            if ([string]$dll.flavor -eq "unmatched") {
                $failures.Add("$($expectation.label) DLL hash is not present in version.lock.json: $($dll.name)")
            } elseif ([string]$dll.flavor -ne $ExpectedFlavor) {
                $failures.Add("$($expectation.label) DLL flavor mismatch for $($dll.name): expected=$ExpectedFlavor actual=$($dll.flavor)")
            }
        }
    }
    return @($failures)
}

function Get-QualityEvidence {
    $commit = (& git -C $repoRoot rev-parse "a09748f^{commit}" 2>$null).Trim()
    & git -C $repoRoot merge-base --is-ancestor $commit HEAD 2>$null
    $isAncestor = -not [string]::IsNullOrWhiteSpace($commit) -and $LASTEXITCODE -eq 0
    $imageDiffPath = Join-Path $repoRoot "src/bin/image_diff.rs"
    $source = if (Test-Path -LiteralPath $imageDiffPath) {
        Get-Content -LiteralPath $imageDiffPath -Raw
    } else { "" }
    $roiMetricTest = $source -match '(?s)#\[test\]\s*fn\s+roi_metrics_exclude_pixels_outside_the_region\s*\('
    $roiParserTest = $source -match '(?s)#\[test\]\s*fn\s+roi_parser_requires_four_unsigned_components\s*\('
    [ordered]@{
        boundary_commit = if ([string]::IsNullOrWhiteSpace($commit)) { "unavailable" } else { $commit }
        boundary_commit_is_ancestor = $isAncestor
        image_diff_path = $imageDiffPath
        roi_metric_unit_test_present = $roiMetricTest
        roi_parser_unit_test_present = $roiParserTest
        valid = $isAncestor -and $roiMetricTest -and $roiParserTest
    }
}

function Get-ExactJsonLine([string]$RawText) {
    $text = if ($null -eq $RawText) { "" } else { $RawText }
    if ($text.StartsWith([char]0xFEFF)) {
        $text = $text.Substring(1)
    }
    if ($text.EndsWith("`r`n")) {
        $text = $text.Substring(0, $text.Length - 2)
    } elseif ($text.EndsWith("`n")) {
        $text = $text.Substring(0, $text.Length - 1)
    }
    $lines = if ($text.Length -eq 0) { @() } else { @($text -split "`r?`n") }
    $parseable = [System.Collections.Generic.List[object]]::new()
    foreach ($line in $lines) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        try {
            $parseable.Add(($line | ConvertFrom-Json -Depth 50))
        } catch {
            continue
        }
    }
    $json = $null
    $error = $null
    $exact = $false
    if (@($lines).Count -ne 1 -or $parseable.Count -ne 1) {
        $error = "stdout must contain exactly one JSON line; lines=$(@($lines).Count) parseable_json_lines=$($parseable.Count)"
    } else {
        $json = $parseable[0]
        $exact = $true
    }
    [pscustomobject]@{
        exact = $exact
        line_count = @($lines).Count
        parseable_json_lines = @($parseable).Count
        non_json_line_count = @($lines).Count - @($parseable).Count
        json = $json
        error = $error
    }
}

function Stop-ProcessTree([System.Diagnostics.Process]$Process) {
    try {
        $Process.Kill($true)
    } catch {
        # The fallback is still scoped to the PID created by this runner, so a
        # hung GUI cannot remain alive without allowing a broad process kill.
        try { & taskkill.exe /PID $Process.Id /T /F 2>$null | Out-Null } catch { }
    }
}

function Invoke-RecordedProcess(
    [string]$FilePath,
    [string[]]$Arguments,
    [string]$CaseDirectory,
    [string]$Label,
    [int]$Timeout,
    [bool]$RequireExactJson = $true
) {
    New-Item -ItemType Directory -Path $CaseDirectory -Force | Out-Null
    $stdoutPath = Join-Path $CaseDirectory "$Label.stdout.txt"
    $stderrPath = Join-Path $CaseDirectory "$Label.stderr.txt"
    $argsPath = Join-Path $CaseDirectory "$Label.args.txt"
    $exitPath = Join-Path $CaseDirectory "$Label.exit.json"
    $command = "$(Quote-Argument $FilePath) " + (($Arguments | ForEach-Object { Quote-Argument $_ }) -join " ")
    Set-Content -LiteralPath $argsPath -Value $command -Encoding UTF8

    $started = Get-Date
    $exitCode = -1
    $timedOut = $false
    $launchError = $null
    $process = $null
    try {
        # D3D/window processes are started directly so Windows keeps the same
        # foreground scheduling behavior used by the Stage 10 runner.
        $process = Start-Process -FilePath $FilePath `
            -ArgumentList (($Arguments | ForEach-Object { Quote-Argument $_ }) -join " ") `
            -RedirectStandardOutput $stdoutPath `
            -RedirectStandardError $stderrPath `
            -PassThru -WindowStyle Hidden
        if (-not $process.WaitForExit($Timeout * 1000)) {
            $timedOut = $true
            Stop-ProcessTree $process
            $process.WaitForExit(10000) | Out-Null
        } else {
            $process.Refresh()
            if ($process.HasExited) {
                $exitCode = [int]$process.ExitCode
            }
        }
    } catch {
        $launchError = $_.Exception.Message
        Set-Content -LiteralPath $stderrPath -Value $launchError -Encoding UTF8
    } finally {
        if ($null -ne $process) {
            $process.Dispose()
        }
    }
    $elapsed = ((Get-Date) - $started).TotalSeconds
    $stdout = if (Test-Path -LiteralPath $stdoutPath) { Get-Content -LiteralPath $stdoutPath -Raw } else { "" }
    $stderr = if (Test-Path -LiteralPath $stderrPath) { Get-Content -LiteralPath $stderrPath -Raw } else { "" }
    $parsed = Get-ExactJsonLine $stdout
    if ($timedOut) {
        $parsed = [pscustomobject]@{
            exact = $false
            line_count = $parsed.line_count
            parseable_json_lines = $parsed.parseable_json_lines
            non_json_line_count = $parsed.non_json_line_count
            json = $null
            error = "process timed out after $Timeout seconds"
        }
    } elseif ($null -ne $launchError) {
        $parsed = [pscustomobject]@{
            exact = $false
            line_count = $parsed.line_count
            parseable_json_lines = $parsed.parseable_json_lines
            non_json_line_count = $parsed.non_json_line_count
            json = $null
            error = $launchError
        }
    } elseif (-not $RequireExactJson) {
        # Stage 10 prints its summary path; its own GUI child still enforces
        # one JSON line. This parent record preserves that raw output verbatim.
        $parsed = [pscustomobject]@{
            exact = $false
            line_count = $parsed.line_count
            parseable_json_lines = $parsed.parseable_json_lines
            non_json_line_count = $parsed.non_json_line_count
            json = $null
            error = $null
        }
    }
    $result = [pscustomobject]@{
        label = $Label
        command = $command
        arguments = $Arguments
        args_path = $argsPath
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        exit_path = $exitPath
        exit_code = $exitCode
        timed_out = $timedOut
        timeout_seconds = $Timeout
        elapsed_seconds = $elapsed
        stdout_raw = $stdout
        stderr_raw = $stderr
        stdout_line_count = $parsed.line_count
        parseable_json_lines = $parsed.parseable_json_lines
        stdout_non_json_lines = $parsed.non_json_line_count
        stdout_exact_one_json = $parsed.exact
        json = $parsed.json
        json_error = $parsed.error
    }
    [ordered]@{
        label = $Label
        command = $command
        args_path = $argsPath
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        exit_code = $exitCode
        timed_out = $timedOut
        timeout_seconds = $Timeout
        elapsed_seconds = $elapsed
        stdout_line_count = $parsed.line_count
        parseable_json_lines = $parsed.parseable_json_lines
        stdout_non_json_lines = $parsed.non_json_line_count
        stdout_exact_one_json = $parsed.exact
        json_error = $parsed.error
    } | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $exitPath -Encoding UTF8
    return $result
}

function Test-GpuPass([object]$Json, [string]$PassName) {
    $pass = Get-JsonPathValue $Json "passes.$PassName"
    if ($null -eq $pass) {
        return $false
    }
    $samples = Get-JsonPathValue $pass "valid_samples"
    $p50 = Get-JsonPathValue $pass "p50_ms"
    $p95 = Get-JsonPathValue $pass "p95_ms"
    return $null -ne $samples -and [uint64]$samples -gt 0 -and (Test-Finite $p50) -and (Test-Finite $p95)
}

function Get-RrGateFailures(
    [object]$Result,
    [string]$CaseLabel,
    [object]$ExpectedGit = $null,
    [string[]]$ExpectedFeatures = @("streamline", "streamline-rr")
) {
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($Result.exit_code -ne 0) { $failures.Add("exit_code=$($Result.exit_code)") }
    if ($Result.timed_out) { $failures.Add("timeout after $($Result.timeout_seconds) seconds") }
    if (-not $Result.stdout_exact_one_json) {
        $failures.Add("stdout is not exactly one JSON line: $($Result.json_error)")
    }
    if ($null -eq $Result.json) {
        return @($failures)
    }
    try {
        $json = $Result.json
        if ($null -ne $ExpectedGit) {
            foreach ($failure in @(Get-BuildProvenanceFailures $json $ExpectedGit $ExpectedFeatures)) {
                $failures.Add($failure)
            }
        }
        $gpuName = [string](Get-JsonPathValue $json "gpu_name")
        if ($gpuName -notmatch "RTX 4060 Laptop") {
            $failures.Add("GPU must be NVIDIA RTX 4060 Laptop: actual=$gpuName")
        }
        if ([string](Get-JsonPathValue $json "denoiser.requested") -ne "dlss-rr" -or
            [string](Get-JsonPathValue $json "denoiser.active") -ne "dlss-rr") {
            $failures.Add("RR denoiser requested/active must both be dlss-rr")
        }
        if ([bool](Get-JsonPathValue $json "dlss_rr.compiled") -ne $true -or
            [bool](Get-JsonPathValue $json "dlss_rr.supported") -ne $true) {
            $failures.Add("dlss_rr compiled and supported must both be true")
        }
        $expectedUpscaler = switch ($CaseLabel) {
            "rr_quality" { "dlss-quality" }
            "rr_balanced" { "dlss-balanced" }
            "rr_performance" { "dlss-performance" }
            default { "dlss-quality" }
        }
        $actualUpscaler = [string](Get-JsonPathValue $json "upscaler.mode")
        if ($actualUpscaler -ne $expectedUpscaler) {
            $failures.Add("RR profile mismatch: expected=$expectedUpscaler actual=$actualUpscaler")
        }
        $optimal = Get-JsonPathValue $json "upscaler.dlss_optimal"
        if ($null -eq $optimal) {
            $failures.Add("RR optimal settings are missing")
        } else {
            $renderWidth = [uint32](Get-JsonPathValue $json "render_width")
            $renderHeight = [uint32](Get-JsonPathValue $json "render_height")
            $optimalWidth = [uint32](Get-JsonPathValue $optimal "optimal_render_width")
            $optimalHeight = [uint32](Get-JsonPathValue $optimal "optimal_render_height")
            if ($renderWidth -ne $optimalWidth -or $renderHeight -ne $optimalHeight) {
                $failures.Add("render extent does not match RR optimal settings")
            }
            if ([string](Get-JsonPathValue $optimal "kind") -ne "dlss_rr") {
                $failures.Add("optimal settings kind must be dlss_rr")
            }
        }
        foreach ($passName in @("rr_evaluate", "rr_input_adapter", "rr_primary_visibility", "rr_boundary_resolve")) {
            if (-not (Test-GpuPass $json $passName)) {
                $failures.Add("active RR pass $passName is missing valid samples")
            }
        }
        foreach ($passName in @(
            "temporal", "atrous", "atrous_0", "atrous_1", "atrous_2", "atrous_3",
            "nrd_prep", "nrd_denoise", "nrd_compose", "dlss_compose", "dlss_evaluate"
        )) {
            $pass = Get-JsonPathValue $json "passes.$passName"
            if ($null -eq $pass) {
                $failures.Add("inactive pass $passName is missing from JSON")
                continue
            }
            $samples = Get-JsonPathValue $pass "valid_samples"
            $p50 = Get-JsonPathValue $pass "p50_ms"
            $p95 = Get-JsonPathValue $pass "p95_ms"
            if (($null -ne $samples -and [uint64]$samples -ne 0) -or $null -ne $p50 -or $null -ne $p95) {
                $failures.Add("inactive pass $passName has hidden GPU cost")
            }
        }
        $idleWaits = Get-JsonPathValue $json "gpu_idle_wait_count"
        if ($null -eq $idleWaits -or [uint64]$idleWaits -ne 0) {
            $failures.Add("gpu_idle_wait_count must be zero")
        }
        $totalP95 = Get-JsonPathValue $json "passes.total.p95_ms"
        if (-not (Test-Finite $totalP95) -or [double]$totalP95 -ge 16.67) {
            $failures.Add("Total GPU p95 must be finite and below 16.67 ms")
        }
        $usageRatio = Get-JsonPathValue $json "memory.measurement.peak_usage_ratio"
        if ($null -eq $usageRatio) { $usageRatio = Get-JsonPathValue $json "memory.usage_ratio" }
        if (-not (Test-Finite $usageRatio) -or [double]$usageRatio -ge 0.70) {
            $failures.Add("local VRAM usage ratio must be finite and below 0.70")
        }
        $reflex = Get-JsonPathValue $json "reflex"
        $tokenCount = Get-JsonPathValue $reflex "token_count"
        if ($null -eq $reflex -or [bool](Get-JsonPathValue $reflex "compiled") -ne $true -or
            [bool](Get-JsonPathValue $reflex "support.reflex") -ne $true -or
            [bool](Get-JsonPathValue $reflex "support.pcl") -ne $true) {
            $failures.Add("Reflex/PCL support must be available for application-frame accounting")
        } elseif ($null -eq $tokenCount -or [uint64]$tokenCount -eq 0) {
            $failures.Add("Reflex token count must be nonzero")
        } else {
            if ([uint64](Get-JsonPathValue $reflex "present_common_count") -ne [uint64]$tokenCount) {
                $failures.Add("presentCommon count must equal token count")
            }
            if ([uint64](Get-JsonPathValue $reflex "order_errors") -ne 0) {
                $failures.Add("Reflex/PCL marker order_errors must be zero")
            }
            foreach ($marker in @(
                "simulation_start", "simulation_end", "render_submit_start",
                "render_submit_end", "present_start", "present_end"
            )) {
                if ([uint64](Get-JsonPathValue $reflex "marker_counts.$marker") -ne [uint64]$tokenCount) {
                    $failures.Add("Reflex marker $marker must equal token count")
                }
            }
            if ([string](Get-JsonPathValue $reflex "active_mode") -ne "off" -and
                [uint64](Get-JsonPathValue $reflex "sleep_count") -ne [uint64]$tokenCount) {
                $failures.Add("Reflex sleep count must equal token count")
            }
        }
        $stderr = [string]$Result.stderr_raw
        if ($stderr -match "(?i)device removed|DRED|NaN|Infinity|(?:^|\s)Inf(?:\s|$)|silent fallback|静默回退") {
            $failures.Add("stderr contains device removal, invalid guide, or fallback evidence")
        }
        if ($stderr -match "(?i)D3D12 Debug.*severity=ERROR|InfoQueue.*CORRUPTION\s+[1-9]|InfoQueue.*ERROR\s+[1-9]") {
            $failures.Add("D3D12 Debug InfoQueue contains CORRUPTION or ERROR messages")
        }
        if ($CaseLabel -eq "rr_animated" -and
            [bool](Get-JsonPathValue $json "acceleration_structures.tlas_update_enabled") -ne $true) {
            $failures.Add("animated RR case must use TLAS update policy")
        }
    } catch {
        $failures.Add("benchmark JSON gate evaluation failed: $($_.Exception.Message)")
    }
    return @($failures)
}

function Get-Stage10Summary([object]$Result) {
    $raw = [string]$Result.stdout_raw
    if ($raw.EndsWith("`r`n")) { $raw = $raw.Substring(0, $raw.Length - 2) }
    elseif ($raw.EndsWith("`n")) { $raw = $raw.Substring(0, $raw.Length - 1) }
    $lines = @($raw -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    if (@($lines).Count -ne 1) {
        return [pscustomobject]@{ summary = $null; path = $null; error = "Stage 10 runner stdout must contain one summary path" }
    }
    $summaryPath = $lines[0].Trim()
    if (-not (Test-Path -LiteralPath $summaryPath)) {
        return [pscustomobject]@{ summary = $null; path = $summaryPath; error = "Stage 10 summary not found: $summaryPath" }
    }
    try {
        $summary = Get-Content -LiteralPath $summaryPath -Raw | ConvertFrom-Json -Depth 50
        return [pscustomobject]@{ summary = $summary; path = $summaryPath; error = $null }
    } catch {
        return [pscustomobject]@{ summary = $null; path = $summaryPath; error = $_.Exception.Message }
    }
}

function Invoke-Stage10SingleCase(
    [string]$CaseLabel,
    [string]$Stage10Exe,
    [string]$NrdExecutable,
    [string]$CaseDirectory,
    [string]$Stage10OutputRoot,
    [object]$ExpectedGit,
    [string[]]$ExpectedFeatures
) {
    $pwsh = (Get-Command pwsh.exe -ErrorAction SilentlyContinue).Source
    if ([string]::IsNullOrWhiteSpace($pwsh)) { $pwsh = Join-Path $PSHOME "pwsh.exe" }
    $stage10Script = Join-Path $repoRoot "scripts/stage10_acceptance.ps1"
    $arguments = @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $stage10Script,
        "-Configuration", "Release", "-Suite", "Smoke", "-CaseName", $CaseLabel,
        "-Runs", "1", "-Seconds", [string]$Seconds, "-TimeoutSeconds", [string]$TimeoutSeconds,
        "-Exe", $Stage10Exe, "-OutputRoot", $Stage10OutputRoot
    )
    if (-not [string]::IsNullOrWhiteSpace($NrdExecutable)) {
        $arguments += @("-NrdExe", $NrdExecutable)
    }
    if ($StreamlineApplicationId -ne 0) {
        $arguments += @("-StreamlineApplicationId", [string]$StreamlineApplicationId)
    }
    $record = Invoke-RecordedProcess $pwsh $arguments $CaseDirectory "stage10-$CaseLabel" $TimeoutSeconds $false
    $stage10 = Get-Stage10Summary $record
    $record | Add-Member -NotePropertyName stage10_summary_path -NotePropertyValue $stage10.path
    $record | Add-Member -NotePropertyName stage10_summary -NotePropertyValue $stage10.summary
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($record.exit_code -ne 0) { $failures.Add("stage10 runner exit_code=$($record.exit_code)") }
    if ($record.timed_out) { $failures.Add("stage10 runner timed out") }
    if ($null -ne $stage10.error) { $failures.Add($stage10.error) }
    if ($null -ne $stage10.summary -and -not [bool]$stage10.summary.pass) {
        foreach ($failure in @($stage10.summary.failures)) { $failures.Add("Stage 10: $failure") }
    }
    if ($null -ne $stage10.summary) {
        if ([string]$stage10.summary.git_head -ne [string]$ExpectedGit.head) {
            $failures.Add("Stage 10 runner git_head does not match the Stage 11 checkout")
        }
        foreach ($case in @($stage10.summary.cases)) {
            if ([uint32]$case.stdout_nonempty_lines -ne 1 -or [uint32]$case.parseable_json_lines -ne 1) {
                $failures.Add("Stage 10 benchmark stdout must contain exactly one JSON line: $($case.label)")
            }
            foreach ($failure in @(Get-BuildProvenanceFailures $case.json $ExpectedGit $ExpectedFeatures)) {
                $failures.Add("$($case.label): $failure")
            }
        }
    }
    $record | Add-Member -NotePropertyName gate_failures -NotePropertyValue @($failures)
    return $record
}

function New-SelfTestJson {
    $inactive = [pscustomobject]@{ p50_ms = $null; p95_ms = $null; valid_samples = 0 }
    $active = [pscustomobject]@{ p50_ms = 0.1; p95_ms = 0.2; valid_samples = 1 }
    [pscustomobject]@{
        build = [pscustomobject]@{
            git_head = "self-test-head"
            git_tree = "self-test-tree"
            git_dirty = $false
            features = @("streamline", "streamline-rr")
        }
        gpu_name = "NVIDIA GeForce RTX 4060 Laptop GPU"
        denoiser = [pscustomobject]@{ requested = "dlss-rr"; active = "dlss-rr" }
        dlss_rr = [pscustomobject]@{ compiled = $true; supported = $true }
        upscaler = [pscustomobject]@{
            mode = "dlss-quality"
            dlss_optimal = [pscustomobject]@{ kind = "dlss_rr"; optimal_render_width = 853; optimal_render_height = 480 }
        }
        render_width = 853
        render_height = 480
        passes = [pscustomobject]@{
            total = [pscustomobject]@{ p50_ms = 4.0; p95_ms = 5.0; valid_samples = 1 }
            rr_evaluate = $active
            rr_input_adapter = $active
            rr_primary_visibility = $active
            rr_boundary_resolve = $active
            temporal = $inactive; atrous = $inactive; atrous_0 = $inactive; atrous_1 = $inactive
            atrous_2 = $inactive; atrous_3 = $inactive; nrd_prep = $inactive; nrd_denoise = $inactive
            nrd_compose = $inactive; dlss_compose = $inactive; dlss_evaluate = $inactive
        }
        gpu_idle_wait_count = 0
        memory = [pscustomobject]@{ usage_ratio = 0.1; measurement = [pscustomobject]@{ peak_usage_ratio = 0.1 } }
        reflex = [pscustomobject]@{
            compiled = $true; active_mode = "on"; token_count = 1; sleep_count = 1
            present_common_count = 1; order_errors = 0
            support = [pscustomobject]@{ reflex = $true; pcl = $true }
            marker_counts = [pscustomobject]@{
                simulation_start = 1; simulation_end = 1; render_submit_start = 1
                render_submit_end = 1; present_start = 1; present_end = 1
            }
        }
        acceleration_structures = [pscustomobject]@{ tlas_update_enabled = $true }
    }
}

function Invoke-SelfTest {
    $validJson = New-SelfTestJson
    $expectedGit = [pscustomobject]@{ head = "self-test-head"; tree = "self-test-tree" }
    $base = [pscustomobject]@{
        exit_code = 0; timed_out = $false; timeout_seconds = 60
        stdout_exact_one_json = $true; json = $validJson; json_error = $null; stderr_raw = ""
    }
    $cases = [System.Collections.Generic.List[object]]::new()
    $positiveFailures = @(Get-RrGateFailures $base "rr_quality" $expectedGit @("streamline", "streamline-rr"))
    if ($positiveFailures.Count -ne 0) {
        throw "SelfTest valid RR evidence was rejected: $($positiveFailures -join '; ')"
    }
    function Assert-Rejected([string]$Name, [scriptblock]$Mutation) {
        $candidate = $Mutation.Invoke()
        $failures = @(Get-RrGateFailures $candidate "rr_quality" $expectedGit @("streamline", "streamline-rr"))
        if ($failures.Count -eq 0) { throw "SelfTest expected rejection: $Name" }
        $cases.Add([ordered]@{ name = $Name; rejected = $true })
    }
    Assert-Rejected "rr_profile_mismatch" {
        $copy = $base.PSObject.Copy(); $copy.json = $validJson.PSObject.Copy()
        $copy.json.upscaler = $validJson.upscaler.PSObject.Copy(); $copy.json.upscaler.mode = "dlss-balanced"; $copy
    }
    Assert-Rejected "missing_active_rr_pass" {
        $copy = $base.PSObject.Copy(); $copy.json = $validJson.PSObject.Copy()
        $copy.json.passes = $validJson.passes.PSObject.Copy(); $copy.json.passes.rr_evaluate = $null; $copy
    }
    Assert-Rejected "hidden_nrd_svgf_sr_cost" {
        $copy = $base.PSObject.Copy(); $copy.json = $validJson.PSObject.Copy()
        $copy.json.passes = $validJson.passes.PSObject.Copy(); $copy.json.passes.temporal = [pscustomobject]@{ p50_ms = 0.1; p95_ms = 0.2; valid_samples = 1 }; $copy
    }
    Assert-Rejected "gpu_idle_wait" {
        $copy = $base.PSObject.Copy(); $copy.json = $validJson.PSObject.Copy(); $copy.json.gpu_idle_wait_count = 1; $copy
    }
    Assert-Rejected "wrong_optimal_extent" {
        $copy = $base.PSObject.Copy(); $copy.json = $validJson.PSObject.Copy()
        $copy.json.upscaler = $validJson.upscaler.PSObject.Copy(); $copy.json.upscaler.dlss_optimal = $validJson.upscaler.dlss_optimal.PSObject.Copy(); $copy.json.upscaler.dlss_optimal.optimal_render_width = 854; $copy
    }
    Assert-Rejected "reflex_present_common_mismatch" {
        $copy = $base.PSObject.Copy(); $copy.json = $validJson.PSObject.Copy()
        $copy.json.reflex = $validJson.reflex.PSObject.Copy(); $copy.json.reflex.present_common_count = 0; $copy
    }
    Assert-Rejected "timeout" {
        [pscustomobject]@{ exit_code = -1; timed_out = $true; timeout_seconds = 10; stdout_exact_one_json = $false; json = $null; json_error = "timeout"; stderr_raw = "" }
    }
    Assert-Rejected "multiple_json_lines" {
        [pscustomobject]@{ exit_code = 0; timed_out = $false; timeout_seconds = 60; stdout_exact_one_json = $false; json = $null; json_error = "stdout must contain exactly one JSON line"; stderr_raw = "" }
    }
    Assert-Rejected "stale_executable_provenance" {
        $copy = $base.PSObject.Copy(); $copy.json = $validJson.PSObject.Copy()
        $copy.json.build = $validJson.build.PSObject.Copy(); $copy.json.build.git_head = "stale-head"; $copy
    }
    Assert-Rejected "debug_infoqueue_error" {
        $copy = $base.PSObject.Copy(); $copy.json = $validJson.PSObject.Copy(); $copy.stderr_raw = "D3D12 Debug InfoQueue：CORRUPTION 0，ERROR 1"; $copy
    }
    $extraStdout = Get-ExactJsonLine "{`"ok`":true}`nStreamline diagnostic"
    if ($extraStdout.exact) { throw "SelfTest accepted extra non-JSON stdout" }
    $cases.Add([ordered]@{ name = "extra_stdout_line"; rejected = $true })
    [ordered]@{ self_test = "passed"; gpu_started = $false; rejected_cases = @($cases) } | ConvertTo-Json -Compress -Depth 20
}

if ($SelfTest) {
    Write-Output (Invoke-SelfTest)
    exit 0
}

if ($CaseName -and $Suite -eq "DebugValidation") {
    throw "-CaseName cannot be combined with -Suite DebugValidation"
}

$needsMatrix = $Suite -eq "Matrix" -and [string]::IsNullOrWhiteSpace($CaseName)
$needsDebug = $Suite -eq "DebugValidation"
$rrPathInput = if ($needsDebug -and -not [string]::IsNullOrWhiteSpace($RrDebugExe)) { $RrDebugExe } else { $RrExe }
$rrExePath = Resolve-RepoPath $rrPathInput
if ($null -eq $rrExePath -or -not (Test-Path -LiteralPath $rrExePath)) { throw "RR executable not found; pass -RrExe (and -RrDebugExe for DebugValidation)" }
$defaultExePath = if ($needsMatrix) { Resolve-RepoPath $DefaultExe } else { $null }
$nrdExePath = if ($needsMatrix) { Resolve-RepoPath $NrdExe } else { $null }
if ($needsMatrix -and ($null -eq $defaultExePath -or $null -eq $nrdExePath)) {
    throw "Matrix requires -DefaultExe and -NrdExe so Stage 10 single-case baselines remain independent"
}

$runId = "$(Get-Date -Format yyyyMMdd-HHmmss)-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
$runRoot = if ([IO.Path]::IsPathRooted($OutputRoot)) { $OutputRoot } else { Join-Path $repoRoot $OutputRoot }
$runRoot = Join-Path $runRoot $runId
New-Item -ItemType Directory -Path $runRoot -Force | Out-Null
$gitEvidence = Get-GitEvidence
$environment = Get-EnvironmentSnapshot
$executablePaths = @($rrExePath, $defaultExePath, $nrdExePath) | Where-Object { $null -ne $_ }
$sdkEvidence = Get-StreamlineSdkEvidence $executablePaths
$executableEvidence = [ordered]@{
    rr = Get-ExecutableEvidence $rrExePath "streamline,streamline-rr"
    default = Get-ExecutableEvidence $defaultExePath "default"
    nrd_streamline = Get-ExecutableEvidence $nrdExePath "nrd,streamline"
}
$qualityEvidence = Get-QualityEvidence

$processRecords = [System.Collections.Generic.List[object]]::new()
$rrRecords = [System.Collections.Generic.List[object]]::new()
$baselineRecords = [System.Collections.Generic.List[object]]::new()
$allFailures = [System.Collections.Generic.List[string]]::new()
$stage10Root = Join-Path $runRoot "stage10"
if ($gitEvidence.head -eq "unavailable" -or $gitEvidence.tree -eq "unavailable") {
    $allFailures.Add("Git HEAD/tree evidence is unavailable")
}
foreach ($failure in @(Get-StreamlineSdkFailures $sdkEvidence $executableEvidence $(if ($needsDebug) { "development" } else { "production" }))) {
    $allFailures.Add("Streamline SDK: $failure")
}
if (-not $qualityEvidence.valid) {
    $allFailures.Add("RR boundary evidence commit/ROI unit-test contract is no longer valid")
}

$rrCases = if (-not [string]::IsNullOrWhiteSpace($CaseName)) {
    @($CaseName)
} elseif ($Suite -eq "Smoke" -or $Suite -eq "DebugValidation") {
    @("rr_quality")
} else {
    @("rr_quality", "rr_balanced", "rr_performance", "rr_animated")
}

foreach ($case in $rrCases) {
    $caseRoot = Join-Path $runRoot $case
    $caseArguments = @(
        "--benchmark-seconds", [string]$Seconds,
        "--output-size", $(if ($needsDebug) { "1280x720" } else { "1920x1080" }),
        "--denoiser", "dlss-rr",
        "--upscaler", $(switch ($case) {
            "rr_quality" { "dlss-quality" }
            "rr_balanced" { "dlss-balanced" }
            "rr_performance" { "dlss-performance" }
            default { "dlss-quality" }
        }),
        "--atrous-mode", "baseline",
        "--command-recording-mode", "optimized",
        "--acceleration-structure-mode", "baseline"
    )
    if ($case -eq "rr_animated") {
        $modelPath = Resolve-RepoPath "assets/gltf/Triangle/Triangle.gltf"
        $caseArguments += @("--model", $modelPath, "--animate-model")
    }
    if ($StreamlineApplicationId -ne 0) {
        $caseArguments += @("--streamline-application-id", [string]$StreamlineApplicationId)
    }
    $record = Invoke-RecordedProcess $rrExePath $caseArguments $caseRoot $case $TimeoutSeconds $true
    $gateFailures = @(Get-RrGateFailures $record $case $gitEvidence @("streamline", "streamline-rr"))
    $record | Add-Member -NotePropertyName gate_failures -NotePropertyValue $gateFailures
    $rrRecords.Add($record)
    $processRecords.Add($record)
    foreach ($failure in $gateFailures) { $allFailures.Add("${case}: $failure") }
}

if ($needsMatrix) {
    $baselineCases = @(
        [ordered]@{ label = "default-native"; stage10_case = "native_svgf"; exe = $defaultExePath; nrd = $null; features = @() },
        [ordered]@{ label = "sr-quality"; stage10_case = "quality_svgf"; exe = $rrExePath; nrd = $null; features = @("streamline", "streamline-rr") },
        [ordered]@{ label = "nrd-quality"; stage10_case = "quality_nrd"; exe = $nrdExePath; nrd = $nrdExePath; features = @("nrd", "streamline") }
    )
    foreach ($baseline in $baselineCases) {
        $baselineRoot = Join-Path $runRoot $baseline.label
        try {
            $record = Invoke-Stage10SingleCase $baseline.stage10_case $baseline.exe $baseline.nrd $baselineRoot $stage10Root $gitEvidence $baseline.features
        } catch {
            # A runner exception is evidence of an inconclusive baseline, not
            # permission to omit it. Keep the failure in the same summary so a
            # partial matrix can never be mistaken for a complete PASS.
            $message = $_.Exception.Message
            New-Item -ItemType Directory -Path $baselineRoot -Force | Out-Null
            $errorPath = Join-Path $baselineRoot "$($baseline.label).runner-error.txt"
            Set-Content -LiteralPath $errorPath -Value $message -Encoding UTF8
            $record = [pscustomobject]@{
                label = "stage10-$($baseline.stage10_case)"
                command = "stage10 case $($baseline.stage10_case)"
                arguments = @()
                args_path = $null
                stdout_path = $null
                stderr_path = $errorPath
                exit_path = $null
                exit_code = -1
                timed_out = $false
                timeout_seconds = $TimeoutSeconds
                elapsed_seconds = 0.0
                stdout_raw = ""
                stderr_raw = $message
                stdout_line_count = 0
                parseable_json_lines = 0
                stdout_non_json_lines = 0
                stdout_exact_one_json = $false
                json = $null
                json_error = $message
                stage10_summary_path = $null
                stage10_summary = $null
                gate_failures = @("Stage 10 runner exception: $message")
            }
        }
        $baselineRecords.Add($record)
        $processRecords.Add($record)
        foreach ($failure in @($record.gate_failures)) { $allFailures.Add("$($baseline.label): $failure") }
    }
    $releaseExtents = @($rrRecords | Where-Object { $_.label -in @("rr_quality", "rr_balanced", "rr_performance") -and $null -ne $_.json } |
        ForEach-Object { "$(Get-JsonPathValue $_.json 'upscaler.dlss_optimal.optimal_render_width')x$(Get-JsonPathValue $_.json 'upscaler.dlss_optimal.optimal_render_height')" } |
        Sort-Object -Unique)
    if (@($releaseExtents).Count -lt 2) {
        $allFailures.Add("RR Quality/Balanced/Performance must expose at least two distinct optimal extents; observed=$($releaseExtents -join ',')")
    }
}

if ($gitEvidence.dirty) {
    $allFailures.Add("working tree is dirty; only development smoke is allowed, not formal PASS")
}

$summary = [ordered]@{
    schema_version = 1
    stage = "11F-R"
    run_id = $runId
    suite = $Suite
    case_name = if ([string]::IsNullOrWhiteSpace($CaseName)) { $null } else { $CaseName }
    seconds = $Seconds
    timeout_seconds = $TimeoutSeconds
    git_head = $gitEvidence.head
    git_tree = $gitEvidence.tree
    git_dirty = $gitEvidence.dirty
    git = $gitEvidence
    executable = $executableEvidence
    streamline_sdk = $sdkEvidence
    quality_evidence = $qualityEvidence
    environment = $environment
    commands = @($processRecords | ForEach-Object {
        [ordered]@{
            label = $_.label
            command = $_.command
            arguments = $_.arguments
            args_path = $_.args_path
            stdout_path = $_.stdout_path
            stderr_path = $_.stderr_path
            exit_code = $_.exit_code
            timed_out = $_.timed_out
            timeout_seconds = $_.timeout_seconds
            elapsed_seconds = $_.elapsed_seconds
            stdout_raw = $_.stdout_raw
            stderr_raw = $_.stderr_raw
            stdout_line_count = $_.stdout_line_count
            parseable_json_lines = $_.parseable_json_lines
            stdout_non_json_lines = $_.stdout_non_json_lines
            stdout_exact_one_json = $_.stdout_exact_one_json
            json_error = $_.json_error
        }
    })
    rr_cases = @($rrRecords | ForEach-Object {
        [ordered]@{ label = $_.label; json = $_.json; gate_failures = @($_.gate_failures); stdout_path = $_.stdout_path; stderr_path = $_.stderr_path }
    })
    stage10_baselines = @($baselineRecords | ForEach-Object {
        [ordered]@{ label = $_.label; summary_path = $_.stage10_summary_path; summary = $_.stage10_summary; gate_failures = @($_.gate_failures) }
    })
    failures = @($allFailures)
    pass = $allFailures.Count -eq 0
    note = "Bounded RR closeout only; no shader, renderer, sampling, ToneMap, exposure, or Frame Generation changes are made."
}
$summaryPath = Join-Path $runRoot "summary.json"
$summary | ConvertTo-Json -Depth 60 | Set-Content -LiteralPath $summaryPath -Encoding UTF8
Write-Output $summaryPath
if ($allFailures.Count -gt 0) { exit 1 }
