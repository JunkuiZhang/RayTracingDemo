use std::{cmp::Ordering, ffi::c_void};

use windows::{
    Win32::Graphics::{Direct3D12::*, Dxgi::Common::*},
    core::Result,
};

use super::pix::PixEventRuntime;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum GpuPass {
    Total = 0,
    AccelerationStructure = 1,
    PathTrace = 2,
    Temporal = 3,
    Atrous = 4,
    Atrous0 = 5,
    Atrous1 = 6,
    Atrous2 = 7,
    Atrous3 = 8,
    ToneMap = 9,
}

pub const PASS_COUNT: usize = 10;
pub const TIMING_WINDOW_CAPACITY: usize = 240;
const TIMESTAMPS_PER_FRAME: usize = PASS_COUNT * 2;
const BENCHMARK_HISTOGRAM_RESOLUTION_MS: f64 = 0.01;
const BENCHMARK_HISTOGRAM_MAX_MS: f64 = 1_000.0;
const BENCHMARK_HISTOGRAM_BIN_COUNT: usize =
    (BENCHMARK_HISTOGRAM_MAX_MS / BENCHMARK_HISTOGRAM_RESOLUTION_MS) as usize + 1;

/// One completed GPU timestamp sample. `valid` is false for warm-up or a
/// malformed/incomplete timestamp pair and such samples never enter the
/// rolling statistics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuTimingSample {
    pub acceleration_structure_ms: f64,
    pub path_trace_ms: f64,
    pub temporal_ms: f64,
    pub atrous_ms: f64,
    pub atrous_iterations_ms: [f64; 4],
    pub tone_map_ms: f64,
    pub total_ms: f64,
    pub valid: bool,
}

impl GpuTimingSample {
    fn value(self, pass: GpuPass) -> f64 {
        match pass {
            GpuPass::Total => self.total_ms,
            GpuPass::AccelerationStructure => self.acceleration_structure_ms,
            GpuPass::PathTrace => self.path_trace_ms,
            GpuPass::Temporal => self.temporal_ms,
            GpuPass::Atrous => self.atrous_ms,
            GpuPass::Atrous0 => self.atrous_iterations_ms[0],
            GpuPass::Atrous1 => self.atrous_iterations_ms[1],
            GpuPass::Atrous2 => self.atrous_iterations_ms[2],
            GpuPass::Atrous3 => self.atrous_iterations_ms[3],
            GpuPass::ToneMap => self.tone_map_ms,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuTimingStats {
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub valid_samples: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuTimingReport {
    pub passes: [GpuTimingStats; PASS_COUNT],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandRecordingFrameStats {
    pub tracked_transition_api_calls: u32,
    pub tracked_transition_barriers: u32,
    pub atrous_pipeline_binds: u32,
    pub atrous_argument_updates: u32,
}

impl CommandRecordingFrameStats {
    pub fn add_transition_submission(&mut self, api_calls: u32, transitions: u32) {
        self.tracked_transition_api_calls =
            self.tracked_transition_api_calls.saturating_add(api_calls);
        self.tracked_transition_barriers =
            self.tracked_transition_barriers.saturating_add(transitions);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CommandRecordingStats {
    pub cpu_ms: GpuTimingStats,
    pub tracked_transition_api_calls_mean: Option<f64>,
    pub tracked_transition_barriers_mean: Option<f64>,
    pub atrous_pipeline_binds_mean: Option<f64>,
    pub atrous_argument_updates_mean: Option<f64>,
}

impl GpuTimingReport {
    pub fn pass(&self, pass: GpuPass) -> GpuTimingStats {
        self.passes[pass as usize]
    }
}

#[derive(Clone, Copy)]
struct RollingStats {
    values: [f64; TIMING_WINDOW_CAPACITY],
    next: usize,
    len: usize,
}

impl Default for RollingStats {
    fn default() -> Self {
        Self {
            values: [0.0; TIMING_WINDOW_CAPACITY],
            next: 0,
            len: 0,
        }
    }
}

impl RollingStats {
    fn clear(&mut self) {
        self.next = 0;
        self.len = 0;
    }

    fn push(&mut self, value: f64) {
        if !value.is_finite() || value < 0.0 {
            return;
        }
        self.values[self.next] = value;
        self.next = (self.next + 1) % TIMING_WINDOW_CAPACITY;
        self.len = (self.len + 1).min(TIMING_WINDOW_CAPACITY);
    }

    fn snapshot(&self) -> GpuTimingStats {
        if self.len == 0 {
            return GpuTimingStats::default();
        }

        // This copy and sort happen only when a low-frequency report asks for
        // statistics. Frame recording only writes one fixed-size ring slot.
        let mut sorted = self.values;
        sorted[..self.len]
            .sort_unstable_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
        GpuTimingStats {
            p50_ms: Some(sorted[percentile_index(self.len, 0.50)]),
            p95_ms: Some(sorted[percentile_index(self.len, 0.95)]),
            valid_samples: self.len,
        }
    }
}

/// Fixed-memory full benchmark accumulator. The UI still uses the latest 240
/// samples, while a benchmark needs every completed sample from the requested
/// wall-clock interval. A 0.01 ms histogram keeps percentile error bounded to
/// 0.005 ms without allocating on the render path.
struct BenchmarkHistogram {
    bins: Vec<u32>,
    len: usize,
}

impl Default for BenchmarkHistogram {
    fn default() -> Self {
        Self {
            bins: vec![0; BENCHMARK_HISTOGRAM_BIN_COUNT],
            len: 0,
        }
    }
}

impl BenchmarkHistogram {
    fn push(&mut self, value: f64) {
        if !value.is_finite() || value < 0.0 {
            return;
        }
        let bin = (value / BENCHMARK_HISTOGRAM_RESOLUTION_MS)
            .round()
            .clamp(0.0, (BENCHMARK_HISTOGRAM_BIN_COUNT - 1) as f64) as usize;
        self.bins[bin] = self.bins[bin].saturating_add(1);
        self.len = self.len.saturating_add(1);
    }

    fn snapshot(&self) -> GpuTimingStats {
        if self.len == 0 {
            return GpuTimingStats::default();
        }
        GpuTimingStats {
            p50_ms: Some(self.percentile(0.50)),
            p95_ms: Some(self.percentile(0.95)),
            valid_samples: self.len,
        }
    }

    fn percentile(&self, percentile: f64) -> f64 {
        let target = (self.len as f64 * percentile).ceil() as usize;
        let mut cumulative = 0usize;
        for (index, count) in self.bins.iter().copied().enumerate() {
            cumulative = cumulative.saturating_add(count as usize);
            if cumulative >= target {
                return index as f64 * BENCHMARK_HISTOGRAM_RESOLUTION_MS;
            }
        }
        BENCHMARK_HISTOGRAM_MAX_MS
    }
}

struct BenchmarkAccumulator {
    passes: [BenchmarkHistogram; PASS_COUNT],
}

#[derive(Default)]
struct CommandRecordingAccumulator {
    cpu_ms: BenchmarkHistogram,
    valid_samples: usize,
    tracked_transition_api_calls: u64,
    tracked_transition_barriers: u64,
    atrous_pipeline_binds: u64,
    atrous_argument_updates: u64,
}

impl CommandRecordingAccumulator {
    fn push(&mut self, cpu_ms: f64, stats: CommandRecordingFrameStats) {
        if !cpu_ms.is_finite() || cpu_ms < 0.0 {
            return;
        }
        self.cpu_ms.push(cpu_ms);
        self.valid_samples = self.valid_samples.saturating_add(1);
        self.tracked_transition_api_calls = self
            .tracked_transition_api_calls
            .saturating_add(stats.tracked_transition_api_calls as u64);
        self.tracked_transition_barriers = self
            .tracked_transition_barriers
            .saturating_add(stats.tracked_transition_barriers as u64);
        self.atrous_pipeline_binds = self
            .atrous_pipeline_binds
            .saturating_add(stats.atrous_pipeline_binds as u64);
        self.atrous_argument_updates = self
            .atrous_argument_updates
            .saturating_add(stats.atrous_argument_updates as u64);
    }

    fn report(&self) -> CommandRecordingStats {
        let mean =
            |sum: u64| (self.valid_samples > 0).then_some(sum as f64 / self.valid_samples as f64);
        CommandRecordingStats {
            cpu_ms: self.cpu_ms.snapshot(),
            tracked_transition_api_calls_mean: mean(self.tracked_transition_api_calls),
            tracked_transition_barriers_mean: mean(self.tracked_transition_barriers),
            atrous_pipeline_binds_mean: mean(self.atrous_pipeline_binds),
            atrous_argument_updates_mean: mean(self.atrous_argument_updates),
        }
    }
}

impl Default for BenchmarkAccumulator {
    fn default() -> Self {
        Self {
            passes: std::array::from_fn(|_| BenchmarkHistogram::default()),
        }
    }
}

impl BenchmarkAccumulator {
    fn push(&mut self, values: [f64; PASS_COUNT]) {
        for (histogram, value) in self.passes.iter_mut().zip(values) {
            histogram.push(value);
        }
    }

    fn report(&self) -> GpuTimingReport {
        GpuTimingReport {
            passes: std::array::from_fn(|index| self.passes[index].snapshot()),
        }
    }
}

fn percentile_index(length: usize, percentile: f64) -> usize {
    let rank = (length as f64 * percentile).ceil() as usize;
    rank.saturating_sub(1).min(length.saturating_sub(1))
}

/// Timestamp profiler with per-frame query storage protected by the owning
/// Frame Context fence. The readback resource is never read until the caller
/// has waited for that frame's fence to complete.
pub struct GpuProfiler {
    query_heap: ID3D12QueryHeap,
    readback: ID3D12Resource,
    timestamp_frequency: u64,
    frame_count: usize,
    last_sample: Option<GpuTimingSample>,
    windows: [RollingStats; PASS_COUNT],
    benchmark: Option<BenchmarkAccumulator>,
    command_recording: Option<CommandRecordingAccumulator>,
    valid_sample_serial: u64,
    collected_frames: Vec<bool>,
    pix: PixEventRuntime,
}

impl GpuProfiler {
    pub fn new(
        device: &ID3D12Device,
        command_queue: &ID3D12CommandQueue,
        frame_count: usize,
    ) -> Result<Self> {
        let query_description = D3D12_QUERY_HEAP_DESC {
            Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
            Count: (frame_count * TIMESTAMPS_PER_FRAME) as u32,
            NodeMask: 0,
        };
        let mut query_heap = None;
        unsafe { device.CreateQueryHeap(&query_description, &mut query_heap)? };

        let heap_properties = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_READBACK,
            ..Default::default()
        };
        let resource_description = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: (frame_count * TIMESTAMPS_PER_FRAME * size_of::<u64>()) as u64,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_UNKNOWN,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let mut readback = None;
        unsafe {
            device.CreateCommittedResource(
                &heap_properties,
                D3D12_HEAP_FLAG_NONE,
                &resource_description,
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut readback,
            )?;
        }
        Ok(Self {
            query_heap: query_heap.unwrap(),
            readback: readback.unwrap(),
            timestamp_frequency: unsafe { command_queue.GetTimestampFrequency()? },
            frame_count,
            last_sample: None,
            windows: [RollingStats::default(); PASS_COUNT],
            benchmark: None,
            command_recording: None,
            valid_sample_serial: 0,
            collected_frames: vec![false; frame_count],
            pix: PixEventRuntime::load(),
        })
    }

    pub fn begin(
        &self,
        command_list: &ID3D12GraphicsCommandList,
        frame_index: usize,
        pass: GpuPass,
    ) {
        self.write_timestamp(command_list, frame_index, pass, 0);
    }

    pub fn end(&self, command_list: &ID3D12GraphicsCommandList, frame_index: usize, pass: GpuPass) {
        self.write_timestamp(command_list, frame_index, pass, 1);
    }

    /// Add a named GPU capture region through WinPixEventRuntime. The payload
    /// is static and does not allocate; if the optional runtime is absent this
    /// safely becomes a no-op instead of calling D3D12's internal API.
    pub fn begin_event(&self, command_list: &ID3D12GraphicsCommandList, pass: GpuPass) {
        let label: &'static [u8] = match pass {
            GpuPass::AccelerationStructure => b"Stage8 AS\0",
            GpuPass::PathTrace => b"Stage8 Path Trace\0",
            GpuPass::Temporal => b"Stage8 Temporal\0",
            GpuPass::Atrous => b"Stage8 A-Trous aggregate\0",
            GpuPass::Atrous0 => b"Stage8 A-Trous 0\0",
            GpuPass::Atrous1 => b"Stage8 A-Trous 1\0",
            GpuPass::Atrous2 => b"Stage8 A-Trous 2\0",
            GpuPass::Atrous3 => b"Stage8 A-Trous 3\0",
            GpuPass::ToneMap => b"Stage8 ToneMap\0",
            GpuPass::Total => b"Stage8 Total\0",
        };
        self.pix.begin(command_list, label);
    }

    pub fn end_event(&self, command_list: &ID3D12GraphicsCommandList) {
        self.pix.end(command_list);
    }

    fn write_timestamp(
        &self,
        command_list: &ID3D12GraphicsCommandList,
        frame_index: usize,
        pass: GpuPass,
        endpoint: usize,
    ) {
        assert!(frame_index < self.frame_count);
        let query = frame_index * TIMESTAMPS_PER_FRAME + pass as usize * 2 + endpoint;
        unsafe {
            command_list.EndQuery(&self.query_heap, D3D12_QUERY_TYPE_TIMESTAMP, query as u32);
        }
    }

    pub fn resolve_frame(&mut self, command_list: &ID3D12GraphicsCommandList, frame_index: usize) {
        let query_start = frame_index * TIMESTAMPS_PER_FRAME;
        unsafe {
            command_list.ResolveQueryData(
                &self.query_heap,
                D3D12_QUERY_TYPE_TIMESTAMP,
                query_start as u32,
                TIMESTAMPS_PER_FRAME as u32,
                &self.readback,
                (query_start * size_of::<u64>()) as u64,
            );
        }
        self.collected_frames[frame_index] = false;
    }

    /// `fence_completed` must be true only after the frame context fence has
    /// completed. This explicit gate prevents accidentally reading a query
    /// slot that the GPU still owns.
    pub fn collect(
        &mut self,
        frame_index: usize,
        fence_completed: bool,
        sample_valid: bool,
    ) -> Result<Option<GpuTimingSample>> {
        if !fence_completed {
            return Ok(None);
        }
        if self.collected_frames[frame_index] {
            return Ok(None);
        }

        let timestamp_start = frame_index * TIMESTAMPS_PER_FRAME;
        let byte_start = timestamp_start * size_of::<u64>();
        let read_range = D3D12_RANGE {
            Begin: byte_start,
            End: byte_start + TIMESTAMPS_PER_FRAME * size_of::<u64>(),
        };
        let mut timestamps = [0_u64; TIMESTAMPS_PER_FRAME];
        unsafe {
            let mut mapped = std::ptr::null_mut::<c_void>();
            self.readback.Map(0, Some(&read_range), Some(&mut mapped))?;
            timestamps.copy_from_slice(std::slice::from_raw_parts(
                mapped.cast::<u64>().add(timestamp_start),
                TIMESTAMPS_PER_FRAME,
            ));
            self.readback
                .Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
        }
        self.collected_frames[frame_index] = true;

        let mut values = [0.0_f64; PASS_COUNT];
        let mut valid = sample_valid && self.timestamp_frequency != 0;
        for (pass, value) in values.iter_mut().enumerate() {
            let begin = timestamps[pass * 2];
            let end = timestamps[pass * 2 + 1];
            if begin == 0 || end < begin {
                valid = false;
                continue;
            }
            *value = (end - begin) as f64 * 1000.0 / self.timestamp_frequency as f64;
            if !value.is_finite() || *value < 0.0 {
                valid = false;
            }
        }

        if !valid {
            return Ok(None);
        }
        let sample = GpuTimingSample {
            acceleration_structure_ms: values[GpuPass::AccelerationStructure as usize],
            path_trace_ms: values[GpuPass::PathTrace as usize],
            temporal_ms: values[GpuPass::Temporal as usize],
            atrous_ms: values[GpuPass::Atrous as usize],
            atrous_iterations_ms: [
                values[GpuPass::Atrous0 as usize],
                values[GpuPass::Atrous1 as usize],
                values[GpuPass::Atrous2 as usize],
                values[GpuPass::Atrous3 as usize],
            ],
            tone_map_ms: values[GpuPass::ToneMap as usize],
            total_ms: values[GpuPass::Total as usize],
            valid: true,
        };
        self.last_sample = Some(sample);
        for (window, value) in self.windows.iter_mut().zip(values) {
            window.push(value);
        }
        if let Some(benchmark) = self.benchmark.as_mut() {
            benchmark.push(values);
        }
        self.valid_sample_serial = self.valid_sample_serial.wrapping_add(1);
        Ok(Some(sample))
    }

    pub fn invalidate(&mut self) {
        self.last_sample = None;
        self.clear_statistics();
        if self.benchmark.is_some() {
            self.benchmark = Some(BenchmarkAccumulator::default());
            self.command_recording = Some(CommandRecordingAccumulator::default());
        }
    }

    pub fn clear_statistics(&mut self) {
        for window in &mut self.windows {
            window.clear();
        }
    }

    pub fn time_ms(&self, pass: GpuPass) -> f64 {
        self.last_sample
            .map(|sample| sample.value(pass))
            .unwrap_or(0.0)
    }

    pub fn statistics(&self) -> GpuTimingReport {
        GpuTimingReport {
            passes: self.windows.map(|window| window.snapshot()),
        }
    }

    pub fn begin_benchmark_measurement(&mut self) {
        self.last_sample = None;
        self.clear_statistics();
        self.benchmark = Some(BenchmarkAccumulator::default());
        self.command_recording = Some(CommandRecordingAccumulator::default());
    }

    pub fn benchmark_statistics(&self) -> GpuTimingReport {
        self.benchmark
            .as_ref()
            .map(BenchmarkAccumulator::report)
            .unwrap_or_default()
    }

    pub fn record_command_recording(
        &mut self,
        elapsed: std::time::Duration,
        stats: CommandRecordingFrameStats,
    ) {
        if let Some(command_recording) = self.command_recording.as_mut() {
            command_recording.push(elapsed.as_secs_f64() * 1000.0, stats);
        }
    }

    pub fn command_recording_statistics(&self) -> CommandRecordingStats {
        self.command_recording
            .as_ref()
            .map(CommandRecordingAccumulator::report)
            .unwrap_or_default()
    }

    pub fn pix_events_available(&self) -> bool {
        self.pix.is_available()
    }

    pub fn valid_sample_serial(&self) -> u64 {
        self.valid_sample_serial
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_window_has_no_percentiles() {
        let stats = RollingStats::default().snapshot();
        assert_eq!(stats, GpuTimingStats::default());
    }

    #[test]
    fn invalid_values_do_not_enter_the_window() {
        let mut window = RollingStats::default();
        window.push(f64::NAN);
        window.push(f64::INFINITY);
        window.push(-1.0);
        assert_eq!(window.snapshot(), GpuTimingStats::default());
    }

    #[test]
    fn percentiles_use_the_fixed_capacity_tail() {
        let mut window = RollingStats::default();
        for value in 1..=300 {
            window.push(value as f64);
        }
        let stats = window.snapshot();
        assert_eq!(stats.valid_samples, TIMING_WINDOW_CAPACITY);
        assert_eq!(stats.p50_ms, Some(180.0));
        assert_eq!(stats.p95_ms, Some(288.0));
    }

    #[test]
    fn pass_report_preserves_atrous_children_and_aggregate_slots() {
        let sample = GpuTimingSample {
            acceleration_structure_ms: 1.0,
            path_trace_ms: 2.0,
            temporal_ms: 3.0,
            atrous_ms: 10.0,
            atrous_iterations_ms: [1.0, 2.0, 3.0, 4.0],
            tone_map_ms: 5.0,
            total_ms: 30.0,
            valid: true,
        };
        assert_eq!(sample.value(GpuPass::Atrous), 10.0);
        assert_eq!(sample.value(GpuPass::Atrous2), 3.0);
    }

    #[test]
    fn percentile_index_handles_single_and_small_windows() {
        assert_eq!(percentile_index(1, 0.50), 0);
        assert_eq!(percentile_index(2, 0.95), 1);
        assert_eq!(percentile_index(4, 0.50), 1);
    }

    #[test]
    fn benchmark_accumulator_retains_the_full_measurement_interval() {
        let mut accumulator = BenchmarkAccumulator::default();
        for value in 1..=600 {
            accumulator.push([value as f64 / 10.0; PASS_COUNT]);
        }
        let stats = accumulator.report().pass(GpuPass::Total);
        assert_eq!(stats.valid_samples, 600);
        assert_eq!(stats.p50_ms, Some(30.0));
        assert_eq!(stats.p95_ms, Some(57.0));
    }

    #[test]
    fn command_recording_statistics_use_cpu_samples_and_means() {
        let mut accumulator = CommandRecordingAccumulator::default();
        accumulator.push(
            1.0,
            CommandRecordingFrameStats {
                tracked_transition_api_calls: 10,
                tracked_transition_barriers: 58,
                atrous_pipeline_binds: 4,
                atrous_argument_updates: 4,
            },
        );
        accumulator.push(
            3.0,
            CommandRecordingFrameStats {
                tracked_transition_api_calls: 8,
                tracked_transition_barriers: 58,
                atrous_pipeline_binds: 2,
                atrous_argument_updates: 4,
            },
        );
        let report = accumulator.report();
        assert_eq!(report.cpu_ms.valid_samples, 2);
        assert_eq!(report.cpu_ms.p50_ms, Some(1.0));
        assert_eq!(report.cpu_ms.p95_ms, Some(3.0));
        assert_eq!(report.tracked_transition_api_calls_mean, Some(9.0));
        assert_eq!(report.tracked_transition_barriers_mean, Some(58.0));
        assert_eq!(report.atrous_pipeline_binds_mean, Some(3.0));
        assert_eq!(report.atrous_argument_updates_mean, Some(4.0));
    }

    #[test]
    fn command_recording_statistics_reject_invalid_cpu_samples() {
        let mut accumulator = CommandRecordingAccumulator::default();
        let stats = CommandRecordingFrameStats {
            tracked_transition_api_calls: 1,
            ..Default::default()
        };
        accumulator.push(f64::NAN, stats);
        accumulator.push(f64::INFINITY, stats);
        accumulator.push(-1.0, stats);
        assert_eq!(accumulator.report(), CommandRecordingStats::default());
    }
}
