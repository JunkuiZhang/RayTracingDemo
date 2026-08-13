use std::{error::Error, path::PathBuf};

use crate::{
    debug_view::DebugView, path_space::PathSpaceMode, reconstruction::DenoiserBackend,
    resolution::ResolutionMode, upscaler::UpscalerMode,
};

/// Stable NGX identity for this custom engine. NVIDIA's DLSS guide requires a
/// GUID-like project ID when no NVIDIA-assigned numeric application ID exists.
pub const STREAMLINE_PROJECT_ID: &str = "59083655-5525-475b-95a2-a904bcf8f4c0";
pub const STREAMLINE_ENGINE_VERSION: &str = concat!("RayTracingDemo-", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RealtimeConfig {
    pub model_path: Option<PathBuf>,
    pub animate_model: bool,
    pub benchmark_seconds: Option<u32>,
    pub capture_output: Option<PathBuf>,
    pub capture_after_spp: Option<u32>,
    pub debug_view: DebugView,
    pub atrous_mode: AtrousMode,
    pub output_size: Option<(u32, u32)>,
    pub resolution_mode: ResolutionMode,
    pub command_recording_mode: CommandRecordingMode,
    pub acceleration_structure_mode: AccelerationStructureMode,
    pub path_space_mode: PathSpaceMode,
    pub denoiser: DenoiserBackend,
    pub upscaler: UpscalerMode,
    /// `None` means the RR compatibility rule selected the default quality
    /// mode; keeping the request separate makes benchmark JSON distinguish
    /// an implicit resolved mode from an explicit CLI request.
    pub requested_upscaler: Option<UpscalerMode>,
    pub reflex_mode: ReflexMode,
    /// Optional NVIDIA-assigned NGX identity. `None` uses this custom engine's
    /// stable Project ID and package-derived engine version instead.
    pub streamline_application_id: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReflexMode {
    Off,
    #[default]
    On,
    OnBoost,
}

impl ReflexMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::OnBoost => "on-boost",
        }
    }

    #[cfg(feature = "streamline")]
    pub const fn sdk_mode(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::On => 1,
            Self::OnBoost => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccelerationStructureMode {
    #[default]
    Baseline,
    Optimized,
}

impl AccelerationStructureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Optimized => "optimized",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AtrousMode {
    #[default]
    Baseline,
    Shared,
}

impl AtrousMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Shared => "shared",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CommandRecordingMode {
    Baseline,
    #[default]
    Optimized,
}

impl CommandRecordingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Optimized => "optimized",
        }
    }
}

#[cfg(target_os = "windows")]
mod windows_app;

/// 启动实时渲染窗口。
#[cfg(target_os = "windows")]
pub fn run(config: RealtimeConfig) -> Result<(), Box<dyn Error>> {
    windows_app::run(config)
}

/// 非 Windows 平台仍可编译和运行 CPU 参考模式。
#[cfg(not(target_os = "windows"))]
pub fn run(_config: RealtimeConfig) -> Result<(), Box<dyn Error>> {
    Err("实时渲染器只支持 Windows 10/11".into())
}
