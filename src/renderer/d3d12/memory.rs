use std::time::{Duration, Instant};

use windows::Win32::Graphics::Dxgi::{
    DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter1, IDXGIAdapter3,
};
use windows::core::Interface;

pub const MEMORY_QUERY_INTERVAL: Duration = Duration::from_millis(500);
pub const MEMORY_CHECKPOINT_CAPACITY: usize = 60;
pub const MEMORY_CHECKPOINT_INTERVAL_SECONDS: u64 = 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoMemoryStatus {
    Available,
    Adapter3Unavailable,
    QueryFailed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoMemorySnapshot {
    pub usage_bytes: Option<u64>,
    pub budget_bytes: Option<u64>,
    pub usage_ratio: Option<f64>,
    pub status: VideoMemoryStatus,
}

impl VideoMemorySnapshot {
    fn unavailable(status: VideoMemoryStatus) -> Self {
        Self {
            usage_bytes: None,
            budget_bytes: None,
            usage_ratio: None,
            status,
        }
    }

    fn from_info(info: DXGI_QUERY_VIDEO_MEMORY_INFO) -> Self {
        let usage_ratio = (info.Budget > 0).then(|| info.CurrentUsage as f64 / info.Budget as f64);
        Self {
            usage_bytes: Some(info.CurrentUsage),
            budget_bytes: Some(info.Budget),
            usage_ratio,
            status: VideoMemoryStatus::Available,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoMemoryCheckpoint {
    pub elapsed_seconds: u32,
    pub usage_bytes: u64,
    pub budget_bytes: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoMemoryMeasurement {
    pub active: bool,
    pub start_usage_bytes: Option<u64>,
    pub end_usage_bytes: Option<u64>,
    pub peak_usage_bytes: Option<u64>,
    pub minimum_budget_bytes: Option<u64>,
    pub peak_usage_ratio: Option<f64>,
    pub valid_query_count: u64,
    pub failed_query_count: u64,
    pub checkpoint_count: usize,
    pub checkpoints: [Option<VideoMemoryCheckpoint>; MEMORY_CHECKPOINT_CAPACITY],
}

impl Default for VideoMemoryMeasurement {
    fn default() -> Self {
        Self {
            active: false,
            start_usage_bytes: None,
            end_usage_bytes: None,
            peak_usage_bytes: None,
            minimum_budget_bytes: None,
            peak_usage_ratio: None,
            valid_query_count: 0,
            failed_query_count: 0,
            checkpoint_count: 0,
            checkpoints: [None; MEMORY_CHECKPOINT_CAPACITY],
        }
    }
}

impl VideoMemoryMeasurement {
    fn begin(&mut self) {
        *self = Self {
            active: true,
            ..Self::default()
        };
    }

    fn record_failure(&mut self) {
        if self.active {
            self.failed_query_count = self.failed_query_count.saturating_add(1);
        }
    }

    fn record_success(
        &mut self,
        elapsed_seconds: u64,
        usage_bytes: Option<u64>,
        budget_bytes: Option<u64>,
        usage_ratio: Option<f64>,
    ) {
        if !self.active {
            return;
        }
        let (Some(usage_bytes), Some(budget_bytes)) = (usage_bytes, budget_bytes) else {
            self.record_failure();
            return;
        };
        self.valid_query_count = self.valid_query_count.saturating_add(1);
        self.start_usage_bytes.get_or_insert(usage_bytes);
        self.end_usage_bytes = Some(usage_bytes);
        self.peak_usage_bytes = Some(
            self.peak_usage_bytes
                .map_or(usage_bytes, |peak| peak.max(usage_bytes)),
        );
        self.minimum_budget_bytes = Some(
            self.minimum_budget_bytes
                .map_or(budget_bytes, |minimum| minimum.min(budget_bytes)),
        );
        if let Some(ratio) = usage_ratio.filter(|ratio| ratio.is_finite()) {
            self.peak_usage_ratio =
                Some(self.peak_usage_ratio.map_or(ratio, |peak| peak.max(ratio)));
        }
        let elapsed_seconds = elapsed_seconds.min(u64::from(u32::MAX)) as u32;
        let checkpoint_due = self.checkpoint_count == 0
            || self
                .checkpoints
                .get(self.checkpoint_count.saturating_sub(1))
                .and_then(|checkpoint| *checkpoint)
                .is_some_and(|checkpoint| {
                    u64::from(elapsed_seconds)
                        >= u64::from(checkpoint.elapsed_seconds)
                            .saturating_add(MEMORY_CHECKPOINT_INTERVAL_SECONDS)
                });
        if checkpoint_due && self.checkpoint_count < MEMORY_CHECKPOINT_CAPACITY {
            self.checkpoints[self.checkpoint_count] = Some(VideoMemoryCheckpoint {
                elapsed_seconds,
                usage_bytes,
                budget_bytes,
            });
            self.checkpoint_count += 1;
        }
    }

    fn finish(&mut self) {
        self.active = false;
    }
}

pub struct VideoMemoryTelemetry {
    // This is the IDXGIAdapter3 cast of the exact adapter selected for device
    // creation. Holding it here keeps telemetry tied to the active GPU.
    adapter: Option<IDXGIAdapter3>,
    snapshot: VideoMemorySnapshot,
    last_query: Instant,
    measurement: VideoMemoryMeasurement,
    measurement_started: Option<Instant>,
}

impl VideoMemoryTelemetry {
    pub fn new(adapter: &IDXGIAdapter1) -> Self {
        let adapter = adapter.cast::<IDXGIAdapter3>().ok();
        let snapshot = if adapter.is_some() {
            VideoMemorySnapshot::unavailable(VideoMemoryStatus::QueryFailed)
        } else {
            eprintln!("DXGI 显存遥测：当前 adapter 不支持 IDXGIAdapter3，显示 N/A");
            VideoMemorySnapshot::unavailable(VideoMemoryStatus::Adapter3Unavailable)
        };
        Self {
            adapter,
            snapshot,
            last_query: Instant::now() - MEMORY_QUERY_INTERVAL,
            measurement: VideoMemoryMeasurement::default(),
            measurement_started: None,
        }
    }

    pub fn poll(&mut self, force: bool) {
        if !force && self.last_query.elapsed() < MEMORY_QUERY_INTERVAL {
            return;
        }
        self.last_query = Instant::now();
        let queried = self.query_now();
        if self.measurement.active {
            let elapsed_seconds = self
                .measurement_started
                .map_or(0, |started| started.elapsed().as_secs());
            match queried {
                Some(snapshot) => self.measurement.record_success(
                    elapsed_seconds,
                    snapshot.usage_bytes,
                    snapshot.budget_bytes,
                    snapshot.usage_ratio,
                ),
                None => self.measurement.record_failure(),
            }
        }
    }

    fn query_now(&mut self) -> Option<VideoMemorySnapshot> {
        let Some(adapter) = self.adapter.as_ref() else {
            self.snapshot =
                VideoMemorySnapshot::unavailable(VideoMemoryStatus::Adapter3Unavailable);
            return None;
        };

        let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        if unsafe { adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info) }
            .is_ok()
        {
            self.snapshot = VideoMemorySnapshot::from_info(info);
            Some(self.snapshot)
        } else {
            // Preserve the last known numbers so a transient query failure cannot
            // turn a valid benchmark endpoint into a false zero/N/A reading.
            self.snapshot.status = VideoMemoryStatus::QueryFailed;
            None
        }
    }

    pub fn begin_benchmark_measurement(&mut self) {
        self.measurement.begin();
        self.measurement_started = Some(Instant::now());
        self.poll(true);
    }

    pub fn finish_benchmark_measurement(&mut self) {
        self.measurement.finish();
        self.measurement_started = None;
    }

    pub fn measurement(&self) -> VideoMemoryMeasurement {
        self.measurement.clone()
    }

    pub fn snapshot(&self) -> VideoMemorySnapshot {
        self.snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_ratio_is_derived_from_local_usage_and_budget() {
        let snapshot = VideoMemorySnapshot::from_info(DXGI_QUERY_VIDEO_MEMORY_INFO {
            Budget: 2_000,
            CurrentUsage: 500,
            ..Default::default()
        });
        assert_eq!(snapshot.usage_bytes, Some(500));
        assert_eq!(snapshot.budget_bytes, Some(2_000));
        assert_eq!(snapshot.usage_ratio, Some(0.25));
        assert_eq!(snapshot.status, VideoMemoryStatus::Available);
    }

    #[test]
    fn zero_budget_does_not_create_an_infinite_ratio() {
        let snapshot = VideoMemorySnapshot::from_info(DXGI_QUERY_VIDEO_MEMORY_INFO::default());
        assert_eq!(snapshot.usage_ratio, None);
    }

    #[test]
    fn unsupported_adapter_is_reported_as_na() {
        let snapshot = VideoMemorySnapshot::unavailable(VideoMemoryStatus::Adapter3Unavailable);
        assert_eq!(snapshot.usage_bytes, None);
        assert_eq!(snapshot.budget_bytes, None);
        assert_eq!(snapshot.usage_ratio, None);
    }

    #[test]
    fn measurement_tracks_start_end_peak_budget_ratio_and_failures() {
        let mut measurement = VideoMemoryMeasurement::default();
        measurement.begin();
        measurement.record_success(0, Some(100), Some(1_000), Some(0.1));
        measurement.record_success(60, Some(250), Some(900), Some(250.0 / 900.0));
        measurement.record_failure();
        measurement.record_success(120, Some(150), Some(950), Some(150.0 / 950.0));
        measurement.finish();

        assert!(!measurement.active);
        assert_eq!(measurement.start_usage_bytes, Some(100));
        assert_eq!(measurement.end_usage_bytes, Some(150));
        assert_eq!(measurement.peak_usage_bytes, Some(250));
        assert_eq!(measurement.minimum_budget_bytes, Some(900));
        assert_eq!(measurement.valid_query_count, 3);
        assert_eq!(measurement.failed_query_count, 1);
        assert_eq!(measurement.checkpoint_count, 3);
        assert_eq!(measurement.checkpoints[1].unwrap().elapsed_seconds, 60);
        assert!((measurement.peak_usage_ratio.unwrap() - 250.0 / 900.0).abs() < f64::EPSILON);
    }

    #[test]
    fn measurement_checkpoints_have_fixed_capacity_and_reset() {
        let mut measurement = VideoMemoryMeasurement::default();
        measurement.begin();
        for elapsed in 0..(MEMORY_CHECKPOINT_CAPACITY as u64 + 20) * 60 {
            measurement.record_success(elapsed, Some(elapsed), Some(1_000), Some(0.5));
        }
        assert_eq!(measurement.checkpoint_count, MEMORY_CHECKPOINT_CAPACITY);
        assert!(measurement.checkpoints.iter().all(Option::is_some));
        measurement.begin();
        assert_eq!(measurement.checkpoint_count, 0);
        assert_eq!(measurement.valid_query_count, 0);
        assert_eq!(measurement.failed_query_count, 0);
    }
}
