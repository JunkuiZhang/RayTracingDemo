[CmdletBinding()]
param(
    [string]$CandidateCommit = "HEAD",
    [string]$ReferenceCommit = "2031abf",
    [string]$OutputRoot = "output/stage8g",
    [string]$RunId,
    [ValidateSet(3)]
    [int]$Runs = 3,
    [ValidateRange(1, 30)]
    [int]$Seconds = 30,
    [ValidateRange(45, 300)]
    [int]$ProcessTimeoutSeconds = 90,
    [ValidateRange(60, 600)]
    [int]$BuildTimeoutSeconds = 300,
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

function Get-ZipCommitCommentFromBytes([byte[]]$Bytes) {
    if ($Bytes.Length -lt 22) { throw "ZIP is too short to contain EOCD" }
    $start = [Math]::Max(0, $Bytes.Length - 65557)
    $eocd = -1
    for ($index = $Bytes.Length - 22; $index -ge $start; $index--) {
        if ($Bytes[$index] -eq 0x50 -and $Bytes[$index + 1] -eq 0x4B -and
            $Bytes[$index + 2] -eq 0x05 -and $Bytes[$index + 3] -eq 0x06) {
            $eocd = $index
            break
        }
    }
    if ($eocd -lt 0) { throw "ZIP EOCD was not found" }
    $commentLength = [BitConverter]::ToUInt16($Bytes, $eocd + 20)
    if ($eocd + 22 + $commentLength -ne $Bytes.Length) {
        throw "ZIP EOCD comment length is inconsistent"
    }
    return [Text.Encoding]::UTF8.GetString($Bytes, $eocd + 22, $commentLength)
}

function Get-ZipCommitComment([string]$Path) {
    return Get-ZipCommitCommentFromBytes ([IO.File]::ReadAllBytes($Path))
}

function Get-PairedSequence {
    return @(
        [pscustomobject]@{ side = "candidate"; index = 1 },
        [pscustomobject]@{ side = "reference"; index = 1 },
        [pscustomobject]@{ side = "reference"; index = 2 },
        [pscustomobject]@{ side = "candidate"; index = 2 },
        [pscustomobject]@{ side = "candidate"; index = 3 },
        [pscustomobject]@{ side = "reference"; index = 3 }
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
        if (-not ("Stage8Validation.PairedPowerStatus" -as [type])) {
            Add-Type -TypeDefinition @"
using System.Runtime.InteropServices;
namespace Stage8Validation {
    [StructLayout(LayoutKind.Sequential)]
    public struct PairedPowerStatus {
        public byte ACLineStatus;
        public byte BatteryFlag;
        public byte BatteryLifePercent;
        public byte Reserved;
        public int BatteryLifeTime;
        public int BatteryFullLifeTime;
    }
    public static class PairedPowerStatusApi {
        [DllImport("kernel32.dll")]
        public static extern bool GetSystemPowerStatus(out PairedPowerStatus status);
    }
}
"@
        }
        $status = New-Object Stage8Validation.PairedPowerStatus
        if ([Stage8Validation.PairedPowerStatusApi]::GetSystemPowerStatus([ref]$status)) {
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
        if ([string]$Before.$field -ne [string]$After.$field) {
            $failures.Add("environment changed: $field")
        }
    }
    if ([string]$Before.power_source -ne "AC" -or [string]$After.power_source -ne "AC") {
        $failures.Add("power source was not AC for both snapshots")
    }
    if ([string]$Before.gpu_name -notmatch 'RTX 4060 Laptop') {
        $failures.Add("GPU is not RTX 4060 Laptop")
    }
    return [pscustomobject]@{ passed = $failures.Count -eq 0; failures = $failures.ToArray() }
}

function Invoke-RecordedProcess(
    [string]$FilePath,
    [string[]]$Arguments,
    [string]$CaseDirectory,
    [string]$Label,
    [int]$TimeoutSeconds,
    [string]$WorkingDirectory,
    [switch]$ParseLastJson
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
        timeout_seconds = $TimeoutSeconds
    })

    $started = Get-Date
    $exitCode = -1
    $timedOut = $false
    $launchError = $null
    try {
        $startParameters = @{
            FilePath = $FilePath
            ArgumentList = $argumentString
            RedirectStandardOutput = $stdoutPath
            RedirectStandardError = $stderrPath
            PassThru = $true
            WindowStyle = "Hidden"
        }
        if (-not [string]::IsNullOrWhiteSpace($WorkingDirectory)) {
            $startParameters.WorkingDirectory = $WorkingDirectory
        }
        $process = Start-Process @startParameters
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $timedOut = $true
            try { $process.Kill($true) } catch { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
            $process.WaitForExit(10000) | Out-Null
            Add-Content -LiteralPath $stderrPath -Value "process timed out after $TimeoutSeconds seconds" -Encoding UTF8
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
    $jsonError = if ($timedOut) { "process timed out after $TimeoutSeconds seconds" } else { $launchError }
    if ($ParseLastJson) {
        if ($lines.Count -gt 0) {
            try { $json = $lines[-1] | ConvertFrom-Json -Depth 50 } catch { if ($null -eq $jsonError) { $jsonError = $_.Exception.Message } }
        } elseif ($null -eq $jsonError) {
            $jsonError = "stdout has no non-empty JSON line"
        }
    }

    return [pscustomobject]@{
        label = $Label
        command = $commandLine
        args = $Arguments
        working_directory = $WorkingDirectory
        stdout_path = $stdoutPath
        stderr_path = $stderrPath
        args_path = $argsPath
        exit_code = $exitCode
        timed_out = $timedOut
        timeout_seconds = $TimeoutSeconds
        elapsed_seconds = ($finished - $started).TotalSeconds
        stdout_nonempty_lines = $lines.Count
        json = $json
        json_error = $jsonError
        stderr_nonempty = -not [string]::IsNullOrWhiteSpace($stderr)
    }
}

function Resolve-GitObject([string]$Revision, [string]$Suffix) {
    $expression = "$Revision$Suffix"
    $output = @(& git rev-parse --verify $expression 2>&1 | ForEach-Object { [string]$_ })
    if ($LASTEXITCODE -ne 0 -or $output.Count -ne 1 -or $output[0] -notmatch '^[0-9a-fA-F]{40}$') {
        throw "cannot resolve git object $expression`: $($output -join ' ')"
    }
    return $output[0].ToLowerInvariant()
}

function Build-CommitSide(
    [string]$Side,
    [string]$Revision,
    [string]$RunRoot,
    [string]$GitExe,
    [string]$CargoExe
) {
    $manifestPath = Join-Path $RunRoot "$Side-build-manifest.json"
    $manifest = [ordered]@{
        schema_version = 1
        side = $Side
        requested_revision = $Revision
        commit = $null
        tree = $null
        archive_path = $null
        archive_sha256 = $null
        archive_comment = $null
        source_directory = $null
        cargo_lock_sha256 = $null
        build = $null
        executable = $null
        executable_bytes = $null
        exe_sha256 = $null
        valid = $false
        error = $null
    }

    try {
        $commit = Resolve-GitObject $Revision "^{commit}"
        $tree = Resolve-GitObject $commit "^{tree}"
        $archivePath = Join-Path $RunRoot "$Side-source.zip"
        $sourceDirectory = Join-Path $RunRoot "$Side-src"
        $targetDirectory = Join-Path $RunRoot "$Side-target"
        $archiveDirectory = Join-Path $RunRoot "$Side-archive"
        $archiveRecord = Invoke-RecordedProcess $GitExe @("archive", "--format=zip", "--output=$archivePath", $commit) $archiveDirectory "git-archive" 120 $script:RepoRoot
        if ($archiveRecord.timed_out -or $archiveRecord.exit_code -ne 0 -or -not (Test-Path -LiteralPath $archivePath -PathType Leaf)) {
            throw "$Side git archive failed"
        }
        $archiveComment = Get-ZipCommitComment $archivePath
        if ($archiveComment -ne $commit) {
            throw "$Side archive comment does not match resolved commit"
        }

        New-Item -ItemType Directory -Path $sourceDirectory | Out-Null
        Expand-Archive -LiteralPath $archivePath -DestinationPath $sourceDirectory
        $cargoLockPath = Join-Path $sourceDirectory "Cargo.lock"
        if (-not (Test-Path -LiteralPath $cargoLockPath -PathType Leaf)) {
            throw "$Side Cargo.lock is missing from archive"
        }

        $buildDirectory = Join-Path $RunRoot "$Side-build"
        $buildRecord = Invoke-RecordedProcess $CargoExe @("build", "--release", "--locked", "--offline", "--target-dir", $targetDirectory) $buildDirectory "cargo-build" $BuildTimeoutSeconds $sourceDirectory
        $manifest.build = $buildRecord
        if ($buildRecord.timed_out -or $buildRecord.exit_code -ne 0) {
            throw "$Side cargo build failed"
        }

        $executable = Join-Path $targetDirectory "release/ray_tracing_demo.exe"
        if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
            throw "$Side release executable is missing"
        }
        $executableItem = Get-Item -LiteralPath $executable
        $manifest.commit = $commit
        $manifest.tree = $tree
        $manifest.archive_path = $archivePath
        $manifest.archive_sha256 = Get-Sha256 $archivePath
        $manifest.archive_comment = $archiveComment
        $manifest.source_directory = $sourceDirectory
        $manifest.cargo_lock_sha256 = Get-Sha256 $cargoLockPath
        $manifest.executable = $executableItem.FullName
        $manifest.executable_bytes = $executableItem.Length
        $manifest.exe_sha256 = Get-Sha256 $executableItem.FullName
        $manifest.valid = $true
    } catch {
        $manifest.error = $_.Exception.Message
    }

    Save-Json $manifestPath $manifest
    if (-not $manifest.valid) { throw "$Side build manifest is invalid: $($manifest.error)" }
    return [pscustomobject]$manifest
}

function Test-BuildManifest($Manifest, [string]$ExpectedCommit) {
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($null -eq $Manifest) {
        $failures.Add("manifest is missing")
        return [pscustomobject]@{ passed = $false; failures = $failures.ToArray() }
    }
    foreach ($field in @("commit", "tree", "archive_path", "archive_sha256", "archive_comment", "cargo_lock_sha256", "executable", "exe_sha256")) {
        if ([string]::IsNullOrWhiteSpace([string]$Manifest.$field)) { $failures.Add("manifest.$field is missing") }
    }
    if ([string]$Manifest.commit -ne $ExpectedCommit) { $failures.Add("manifest commit differs from expected commit") }
    if ([string]$Manifest.archive_comment -ne [string]$Manifest.commit) { $failures.Add("archive comment differs from manifest commit") }
    if (-not [bool]$Manifest.valid) { $failures.Add("manifest valid flag is false") }
    if (-not [string]::IsNullOrWhiteSpace([string]$Manifest.archive_path)) {
        if ((Get-Sha256 ([string]$Manifest.archive_path)) -ne [string]$Manifest.archive_sha256) { $failures.Add("archive SHA-256 differs") }
    }
    if (-not [string]::IsNullOrWhiteSpace([string]$Manifest.executable)) {
        if ((Get-Sha256 ([string]$Manifest.executable)) -ne [string]$Manifest.exe_sha256) { $failures.Add("executable SHA-256 differs") }
    }
    return [pscustomobject]@{ passed = $failures.Count -eq 0; failures = $failures.ToArray() }
}

function Get-RunStderr($Run) {
    if (-not [string]::IsNullOrWhiteSpace([string]$Run.stderr_path) -and (Test-Path -LiteralPath $Run.stderr_path)) {
        return Get-Content -LiteralPath $Run.stderr_path -Raw
    }
    return ""
}

function Test-PairedRun($Run, [string]$ExpectedExeSha256) {
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
    return [pscustomobject]@{ passed = $failures.Count -eq 0; failures = $failures.ToArray() }
}

function Invoke-SelfTest {
    $caseCount = 0
    foreach ($case in @(
        [pscustomobject]@{ values = @(3, 1, 2); expected = 2.0 },
        [pscustomobject]@{ values = @(4, 1, 3, 2); expected = 2.5 }
    )) {
        if ((Get-Median $case.values) -ne $case.expected) { throw "median self-test failed" }
        $caseCount++
    }

    $sequenceText = @((Get-PairedSequence) | ForEach-Object { "$($_.side)-$($_.index)" }) -join ","
    if ($sequenceText -ne "candidate-1,reference-1,reference-2,candidate-2,candidate-3,reference-3") {
        throw "paired sequence self-test failed"
    }
    $caseCount++

    $comment = "0123456789abcdef0123456789abcdef01234567"
    $commentBytes = [Text.Encoding]::UTF8.GetBytes($comment)
    $zipBytes = New-Object byte[] (22 + $commentBytes.Length)
    $zipBytes[0] = 0x50; $zipBytes[1] = 0x4B; $zipBytes[2] = 0x05; $zipBytes[3] = 0x06
    $lengthBytes = [BitConverter]::GetBytes([uint16]$commentBytes.Length)
    $zipBytes[20] = $lengthBytes[0]; $zipBytes[21] = $lengthBytes[1]
    [Array]::Copy($commentBytes, 0, $zipBytes, 22, $commentBytes.Length)
    if ((Get-ZipCommitCommentFromBytes $zipBytes) -ne $comment) { throw "ZIP comment self-test failed" }
    $caseCount++

    $environment = [pscustomobject]@{ gpu_name = "NVIDIA GeForce RTX 4060 Laptop GPU"; driver_version = "1"; active_power_scheme = "balanced"; power_source = "AC" }
    $changedEnvironment = [pscustomobject]@{ gpu_name = $environment.gpu_name; driver_version = "1"; active_power_scheme = "balanced"; power_source = "battery" }
    if ((Test-EnvironmentContinuity $environment $environment).passed -ne $true -or (Test-EnvironmentContinuity $environment $changedEnvironment).passed) {
        throw "environment continuity self-test failed"
    }
    $caseCount++

    if ((Test-BuildManifest $null "0123456789abcdef0123456789abcdef01234567").passed) {
        throw "missing provenance self-test failed"
    }
    $caseCount++

    $timeoutRun = [pscustomobject]@{ timed_out = $true; exit_code = -1; exe_sha256_before = "x"; stdout_nonempty_lines = 0; json = $null; json_error = "timeout"; stderr_path = "" }
    if ((Test-PairedRun $timeoutRun "x").passed) { throw "timeout validation self-test failed" }
    $caseCount++

    [ordered]@{ self_test = "passed"; cases = $caseCount; runs = $Runs } | ConvertTo-Json -Compress
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

$script:RepoRoot = (Get-Location).Path
$resolvedOutputRoot = [IO.Path]::GetFullPath((Join-Path $script:RepoRoot $OutputRoot))
if ([string]::IsNullOrWhiteSpace($RunId)) {
    $runId = "paired-auth-1080-{0}-{1}" -f (Get-Date -Format "yyyyMMdd-HHmmss"), ([guid]::NewGuid().ToString("N").Substring(0, 8))
} else {
    $runId = $RunId
}
$runRoot = Join-Path $resolvedOutputRoot $runId
if (Test-Path -LiteralPath $runRoot) { throw "run root already exists: $runRoot" }
New-Item -ItemType Directory -Path $runRoot -Force | Out-Null

$startedAt = Get-Date
$summaryPath = Join-Path $runRoot "summary.json"
$gitExe = (Get-Command git -ErrorAction Stop).Source
$cargoExe = (Get-Command cargo -ErrorAction Stop).Source
$environmentBefore = Get-EnvironmentSnapshot
Save-Json (Join-Path $runRoot "environment_before.json") $environmentBefore
$candidateResolvedCommit = $null
$referenceResolvedCommit = $null
$candidateManifest = $null
$referenceManifest = $null
$provenanceValidation = [pscustomobject]@{ passed = $false; failures = @("provenance was not evaluated") }
$runRecords = [System.Collections.Generic.List[object]]::new()
$fatalError = $null

try {
    $candidateResolvedCommit = Resolve-GitObject $CandidateCommit "^{commit}"
    $referenceResolvedCommit = Resolve-GitObject $ReferenceCommit "^{commit}"
    $candidateManifest = Build-CommitSide "candidate" $candidateResolvedCommit $runRoot $gitExe $cargoExe
    $referenceManifest = Build-CommitSide "reference" $referenceResolvedCommit $runRoot $gitExe $cargoExe
    $candidateManifestValidation = Test-BuildManifest $candidateManifest $candidateResolvedCommit
    $referenceManifestValidation = Test-BuildManifest $referenceManifest $referenceResolvedCommit
    $provenanceFailures = @($candidateManifestValidation.failures) + @($referenceManifestValidation.failures)
    $provenanceValidation = [pscustomobject]@{ passed = $provenanceFailures.Count -eq 0; failures = $provenanceFailures }
    if (-not $provenanceValidation.passed) { throw "build provenance validation failed" }

    $workload = @(
        "--benchmark-seconds", "$Seconds",
        "--output-size", "1920x1080",
        "--render-scale", "1.0",
        "--command-recording-mode", "optimized",
        "--atrous-mode", "baseline",
        "--acceleration-structure-mode", "baseline"
    )
    foreach ($item in @(Get-PairedSequence)) {
        $manifest = if ($item.side -eq "candidate") { $candidateManifest } else { $referenceManifest }
        $caseDirectory = Join-Path $runRoot ("{0}-{1:D2}" -f $item.side, $item.index)
        $run = Invoke-RecordedProcess $manifest.executable $workload $caseDirectory "run" $ProcessTimeoutSeconds ([IO.Path]::GetDirectoryName($manifest.executable)) -ParseLastJson
        $run | Add-Member -NotePropertyName side -NotePropertyValue $item.side
        $run | Add-Member -NotePropertyName index -NotePropertyValue $item.index
        $run | Add-Member -NotePropertyName exe_sha256_before -NotePropertyValue (Get-Sha256 $manifest.executable)
        $run | Add-Member -NotePropertyName validation -NotePropertyValue (Test-PairedRun $run $manifest.exe_sha256)
        $runRecords.Add($run)
        if (-not $run.validation.passed) { throw "paired benchmark run failed: $($item.side)-$($item.index)" }
    }
} catch {
    $fatalError = $_.Exception.Message
}

foreach ($side in @("candidate", "reference")) {
    $manifestPath = Join-Path $runRoot "$side-build-manifest.json"
    if (Test-Path -LiteralPath $manifestPath) {
        $loadedManifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json -Depth 60
        if ($side -eq "candidate") { $candidateManifest = $loadedManifest } else { $referenceManifest = $loadedManifest }
    }
}

$environmentAfter = Get-EnvironmentSnapshot
Save-Json (Join-Path $runRoot "environment_after.json") $environmentAfter
$environmentValidation = Test-EnvironmentContinuity $environmentBefore $environmentAfter
$candidateRuns = @($runRecords | Where-Object { $_.side -eq "candidate" })
$referenceRuns = @($runRecords | Where-Object { $_.side -eq "reference" })
$candidateValidRuns = @($candidateRuns | Where-Object { $_.validation.passed })
$referenceValidRuns = @($referenceRuns | Where-Object { $_.validation.passed })
$candidateP95 = @($candidateValidRuns | ForEach-Object { [double]$_.json.passes.total.p95_ms })
$referenceP95 = @($referenceValidRuns | ForEach-Object { [double]$_.json.passes.total.p95_ms })
$candidateMedian = Get-Median $candidateP95
$referenceMedian = Get-Median $referenceP95
$regression = $null
$absoluteGate = "INCONCLUSIVE"
$decision = "INCONCLUSIVE"
$evidenceValid = $null -eq $fatalError -and $provenanceValidation.passed -and $environmentValidation.passed -and
    $candidateP95.Count -eq $Runs -and $referenceP95.Count -eq $Runs -and
    (Test-Finite $candidateMedian) -and (Test-Finite $referenceMedian) -and [double]$referenceMedian -gt 0
if ($evidenceValid) {
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
    schema_version = 2
    stage = "8G"
    suite = "AuthenticatedPaired1080"
    run_id = $runId
    started_at = $startedAt.ToString("o")
    finished_at = (Get-Date).ToString("o")
    configured_runs_per_side = $Runs
    benchmark_seconds = $Seconds
    process_timeout_seconds = $ProcessTimeoutSeconds
    environment = [ordered]@{ before = $environmentBefore; after = $environmentAfter; validation = $environmentValidation }
    provenance = [ordered]@{ candidate = $candidateManifest; reference = $referenceManifest; validation = $provenanceValidation; valid = $provenanceValidation.passed }
    workload = @("--benchmark-seconds", "$Seconds", "--output-size", "1920x1080", "--render-scale", "1.0", "--command-recording-mode", "optimized", "--atrous-mode", "baseline", "--acceleration-structure-mode", "baseline")
    order = @((Get-PairedSequence) | ForEach-Object { "$($_.side)-$($_.index)" })
    candidate_runs = $candidateRuns
    reference_runs = $referenceRuns
    runs = $runRecords.ToArray()
    candidate_p95_ms = $candidateP95
    reference_p95_ms = $referenceP95
    candidate_median_p95_ms = $candidateMedian
    reference_median_p95_ms = $referenceMedian
    candidate_vs_reference_percent = $regression
    absolute_gate = $absoluteGate
    decision = $decision
    fatal_error = $fatalError
    passed = $decision -eq "HISTORICAL GATE PASS" -or $decision -eq "ABSOLUTE GATE FAIL / NO PAIRED REGRESSION"
}
Save-Json $summaryPath $summary
Write-Output ([ordered]@{
    run_id = $runId
    summary = $summaryPath
    decision = $decision
    candidate_median_p95_ms = $candidateMedian
    reference_median_p95_ms = $referenceMedian
    candidate_vs_reference_percent = $regression
    environment_valid = $environmentValidation.passed
    provenance_valid = $provenanceValidation.passed
} | ConvertTo-Json -Compress)
if (-not $summary.passed) { exit 1 }
exit 0
