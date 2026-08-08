use std::time::{Duration, Instant};

use windows::Win32::Graphics::Dxgi::{
    DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter1, IDXGIAdapter3,
};
use windows::core::Interface;

pub const MEMORY_QUERY_INTERVAL: Duration = Duration::from_millis(500);

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

pub struct VideoMemoryTelemetry {
    // This is the IDXGIAdapter3 cast of the exact adapter selected for device
    // creation. Holding it here keeps telemetry tied to the active GPU.
    adapter: Option<IDXGIAdapter3>,
    snapshot: VideoMemorySnapshot,
    last_query: Instant,
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
        }
    }

    pub fn poll(&mut self, force: bool) {
        if !force && self.last_query.elapsed() < MEMORY_QUERY_INTERVAL {
            return;
        }
        self.last_query = Instant::now();
        let Some(adapter) = self.adapter.as_ref() else {
            return;
        };

        let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        if unsafe { adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info) }
            .is_ok()
        {
            self.snapshot = VideoMemorySnapshot::from_info(info);
        } else {
            self.snapshot = VideoMemorySnapshot::unavailable(VideoMemoryStatus::QueryFailed);
        }
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
}
