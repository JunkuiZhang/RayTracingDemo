use std::{error::Error, path::PathBuf};

use crate::{
    debug_view::DebugView, reconstruction::DenoiserBackend, resolution::ResolutionMode,
    upscaler::UpscalerMode,
};

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
    pub denoiser: DenoiserBackend,
    pub upscaler: UpscalerMode,
    pub reflex_mode: ReflexMode,
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
