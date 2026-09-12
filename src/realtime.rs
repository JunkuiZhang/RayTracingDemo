use std::{error::Error, path::PathBuf, time::Duration};

use crate::{
    debug_view::DebugView, path_space::PathSpaceMode, reconstruction::DenoiserBackend,
    resolution::ResolutionMode, upscaler::UpscalerMode,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SceneKind {
    #[default]
    Cornell,
    NestedDielectric,
}

impl SceneKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cornell => "cornell",
            Self::NestedDielectric => "nested-dielectric",
        }
    }
}

/// Stable NGX identity for this custom engine. NVIDIA's DLSS guide requires a
/// GUID-like project ID when no NVIDIA-assigned numeric application ID exists.
pub const STREAMLINE_PROJECT_ID: &str = "59083655-5525-475b-95a2-a904bcf8f4c0";
pub const STREAMLINE_ENGINE_VERSION: &str = concat!("RayTracingDemo-", env!("CARGO_PKG_VERSION"));

pub const DEFAULT_HDR_PAPER_WHITE_NITS: u32 = 200;
pub const DEFAULT_HDR_PEAK_NITS: u32 = 1_000;

/// Explicit display-output intent. HDR stays opt-in so an SDR desktop keeps
/// the byte-for-byte presentation and capture behavior used by acceptance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HdrConfig {
    pub enabled: bool,
    pub paper_white_nits: u32,
    /// `None` uses the active display's reported peak, with a conservative
    /// fallback when the driver does not expose useful luminance metadata.
    pub peak_nits: Option<u32>,
}

impl Default for HdrConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paper_white_nits: DEFAULT_HDR_PAPER_WHITE_NITS,
            peak_nits: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RealtimeConfig {
    pub scene: SceneKind,
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
    pub frame_generation: FrameGenerationMode,
    pub hdr: HdrConfig,
    /// Optional NVIDIA-assigned NGX identity. `None` uses this custom engine's
    /// stable Project ID and package-derived engine version instead.
    pub streamline_application_id: Option<u32>,
}

/// User intent is separate from the runtime state machine: `On` requests a
/// proxy chain, but does not claim the SDK accepted a complete input frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameGenerationMode {
    #[default]
    Off,
    On,
}

impl FrameGenerationMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
        }
    }
}

/// Monotonic presentation counters owned by the successful Present path.
/// Generated frames never receive an application frame token, so this is the
/// only accounting layer that may combine host and DLSS-G presentation data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresentationCounters {
    pub application_frames: u64,
    pub displayed_frames: u64,
    pub generated_frames: u64,
    pub dropped_generated_frames: u64,
}

impl PresentationCounters {
    pub fn observe_present(&mut self, requested_generated_frames: u32, actual_presented: u32) {
        let requested_multiplier = requested_generated_frames.saturating_add(1);
        self.application_frames = self.application_frames.saturating_add(1);
        self.displayed_frames = self
            .displayed_frames
            .saturating_add(u64::from(actual_presented));
        self.generated_frames = self
            .generated_frames
            .saturating_add(u64::from(actual_presented.saturating_sub(1)));
        self.dropped_generated_frames = self.dropped_generated_frames.saturating_add(u64::from(
            requested_multiplier.saturating_sub(actual_presented),
        ));
    }

    pub const fn delta(self, baseline: Self) -> Self {
        Self {
            application_frames: self
                .application_frames
                .saturating_sub(baseline.application_frames),
            displayed_frames: self
                .displayed_frames
                .saturating_sub(baseline.displayed_frames),
            generated_frames: self
                .generated_frames
                .saturating_sub(baseline.generated_frames),
            dropped_generated_frames: self
                .dropped_generated_frames
                .saturating_sub(baseline.dropped_generated_frames),
        }
    }

    pub fn rates(self, elapsed: Duration) -> PresentationRates {
        let seconds = elapsed.as_secs_f64();
        let base_fps = if seconds > 0.0 {
            self.application_frames as f64 / seconds
        } else {
            0.0
        };
        let display_fps = if seconds > 0.0 {
            self.displayed_frames as f64 / seconds
        } else {
            0.0
        };
        let actual_presented_multiplier = if self.application_frames > 0 {
            self.displayed_frames as f64 / self.application_frames as f64
        } else {
            0.0
        };
        PresentationRates {
            base_fps,
            display_fps,
            actual_presented_multiplier,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PresentationRates {
    pub base_fps: f64,
    pub display_fps: f64,
    pub actual_presented_multiplier: f64,
}

#[cfg(feature = "streamline-fg")]
pub const FRAME_GENERATION_WARMUP_PRESENT_LIMIT: u32 = 120;

#[cfg(feature = "streamline-fg")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameGenerationActivation {
    Startup,
    Interactive,
}

#[cfg(feature = "streamline-fg")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameGenerationFault {
    SdkStatus(u32),
    WarmupTimeout,
}

#[cfg(feature = "streamline-fg")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameGenerationLifecycle {
    Unavailable,
    OffNative,
    EnablingProxy,
    OnProxy,
    SuspendedProxy,
    Disabling,
    FaultPendingDisable(FrameGenerationFault),
}

#[cfg(feature = "streamline-fg")]
impl FrameGenerationLifecycle {
    #[cfg(feature = "streamline-fg")]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::OffNative => "off-native",
            Self::EnablingProxy => "enabling-proxy",
            Self::OnProxy => "on-proxy",
            Self::SuspendedProxy => "suspended-proxy",
            Self::Disabling => "disabling",
            Self::FaultPendingDisable(_) => "fault-pending-disable",
        }
    }

    /// Whether the renderer still owns an FG-loaded proxy chain.
    ///
    /// A fault changes health, not ownership: options and tags still have to
    /// be released before the plug-in and swap chain can be torn down.
    pub const fn proxy_loaded(self) -> bool {
        matches!(
            self,
            Self::EnablingProxy
                | Self::OnProxy
                | Self::SuspendedProxy
                | Self::Disabling
                | Self::FaultPendingDisable(_)
        )
    }
}

#[cfg(feature = "streamline-fg")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameGenerationObservation {
    NotActive,
    Ineligible,
    Pending,
    Confirmed,
    AlreadyConfirmed,
    WarmupExpired(FrameGenerationActivation),
}

/// The only owner of FG requested/runtime transitions. Keeping these rules in
/// one small state machine prevents F5, debug views, and Present errors from
/// inventing independent loaded/active/tagged flags.
#[cfg(feature = "streamline-fg")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameGenerationController {
    requested: FrameGenerationMode,
    lifecycle: FrameGenerationLifecycle,
    activation: Option<FrameGenerationActivation>,
    warmup_presents: u32,
    confirmed: bool,
}

#[cfg(feature = "streamline-fg")]
impl FrameGenerationController {
    pub const fn new(requested: FrameGenerationMode, supported: bool) -> Self {
        let lifecycle = match (requested, supported) {
            (FrameGenerationMode::Off, true) => FrameGenerationLifecycle::OffNative,
            (FrameGenerationMode::Off, false) => FrameGenerationLifecycle::Unavailable,
            (FrameGenerationMode::On, true) => FrameGenerationLifecycle::EnablingProxy,
            (FrameGenerationMode::On, false) => FrameGenerationLifecycle::Unavailable,
        };
        let activation = match (requested, supported) {
            (FrameGenerationMode::On, true) => Some(FrameGenerationActivation::Startup),
            _ => None,
        };
        Self {
            requested,
            lifecycle,
            activation,
            warmup_presents: 0,
            confirmed: false,
        }
    }

    pub const fn requested(self) -> FrameGenerationMode {
        self.requested
    }

    pub const fn lifecycle(self) -> FrameGenerationLifecycle {
        self.lifecycle
    }

    pub fn request_enable(&mut self) -> Result<(), &'static str> {
        if self.lifecycle != FrameGenerationLifecycle::OffNative {
            return Err("FG 只能从 OffNative 开始启用");
        }
        self.requested = FrameGenerationMode::On;
        self.activation = Some(FrameGenerationActivation::Interactive);
        self.warmup_presents = 0;
        self.confirmed = false;
        Ok(())
    }

    pub fn complete_enable(&mut self) -> Result<(), &'static str> {
        if self.lifecycle != FrameGenerationLifecycle::OffNative
            || self.requested != FrameGenerationMode::On
        {
            return Err("FG loaded chain 只能完成一次待处理的启用请求");
        }
        self.lifecycle = FrameGenerationLifecycle::EnablingProxy;
        Ok(())
    }

    pub fn request_disable(&mut self) -> Result<(), &'static str> {
        if !self.lifecycle.proxy_loaded() {
            return Err("FG 当前没有 loaded proxy chain");
        }
        self.requested = FrameGenerationMode::Off;
        self.lifecycle = FrameGenerationLifecycle::Disabling;
        Ok(())
    }

    pub const fn complete_disable(&mut self) {
        self.requested = FrameGenerationMode::Off;
        self.lifecycle = FrameGenerationLifecycle::OffNative;
        self.activation = None;
        self.warmup_presents = 0;
        self.confirmed = false;
    }

    pub fn suspend(&mut self) -> Result<(), &'static str> {
        match self.lifecycle {
            FrameGenerationLifecycle::EnablingProxy | FrameGenerationLifecycle::OnProxy => {
                self.lifecycle = FrameGenerationLifecycle::SuspendedProxy;
                Ok(())
            }
            FrameGenerationLifecycle::SuspendedProxy => Ok(()),
            _ => Err("FG 只有 loaded proxy chain 才能暂停"),
        }
    }

    pub fn accept_complete_frame(&mut self) -> Result<(), &'static str> {
        match self.lifecycle {
            FrameGenerationLifecycle::EnablingProxy | FrameGenerationLifecycle::SuspendedProxy => {
                self.lifecycle = FrameGenerationLifecycle::OnProxy;
                Ok(())
            }
            FrameGenerationLifecycle::OnProxy => Ok(()),
            _ => Err("FG 当前没有等待完整输入的 proxy chain"),
        }
    }

    pub fn fault(&mut self, raw_status: u32) {
        if self.lifecycle.proxy_loaded() {
            self.lifecycle = FrameGenerationLifecycle::FaultPendingDisable(
                FrameGenerationFault::SdkStatus(raw_status),
            );
        }
    }

    pub fn warmup_timeout(&mut self) {
        if self.lifecycle.proxy_loaded() {
            self.lifecycle =
                FrameGenerationLifecycle::FaultPendingDisable(FrameGenerationFault::WarmupTimeout);
        }
    }

    pub fn observe_present(
        &mut self,
        actual_presented: u32,
        interpolation_eligible: bool,
    ) -> FrameGenerationObservation {
        if self.lifecycle != FrameGenerationLifecycle::OnProxy {
            return FrameGenerationObservation::NotActive;
        }
        if actual_presented >= 2 {
            if self.confirmed {
                return FrameGenerationObservation::AlreadyConfirmed;
            }
            self.confirmed = true;
            return FrameGenerationObservation::Confirmed;
        }
        if self.confirmed {
            return FrameGenerationObservation::AlreadyConfirmed;
        }
        // DLSS-G intentionally pauses interpolation while the application
        // window is not focused. Such Presents are not evidence of a broken
        // integration and must not consume the bounded validation budget.
        if !interpolation_eligible {
            return FrameGenerationObservation::Ineligible;
        }
        self.warmup_presents = self.warmup_presents.saturating_add(1);
        if self.warmup_presents >= FRAME_GENERATION_WARMUP_PRESENT_LIMIT {
            return FrameGenerationObservation::WarmupExpired(
                self.activation
                    .unwrap_or(FrameGenerationActivation::Interactive),
            );
        }
        FrameGenerationObservation::Pending
    }

    pub const fn warmup_presents(self) -> u32 {
        self.warmup_presents
    }
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

#[cfg(test)]
mod presentation_tests {
    use std::time::Duration;

    use super::PresentationCounters;

    #[test]
    fn presentation_counters_distinguish_base_generated_and_dropped_frames() {
        let mut counters = PresentationCounters::default();
        counters.observe_present(0, 1);
        counters.observe_present(1, 2);
        counters.observe_present(1, 1);
        assert_eq!(counters.application_frames, 3);
        assert_eq!(counters.displayed_frames, 4);
        assert_eq!(counters.generated_frames, 1);
        assert_eq!(counters.dropped_generated_frames, 1);

        let delta = counters.delta(PresentationCounters {
            application_frames: 1,
            displayed_frames: 1,
            generated_frames: 0,
            dropped_generated_frames: 0,
        });
        assert_eq!(delta.application_frames, 2);
        assert_eq!(delta.displayed_frames, 3);
        let rates = delta.rates(Duration::from_millis(500));
        assert_eq!(rates.base_fps, 4.0);
        assert_eq!(rates.display_fps, 6.0);
        assert_eq!(rates.actual_presented_multiplier, 1.5);
    }

    #[test]
    fn zero_duration_and_empty_windows_do_not_invent_rates() {
        let rates = PresentationCounters::default().rates(Duration::ZERO);
        assert_eq!(rates.base_fps, 0.0);
        assert_eq!(rates.display_fps, 0.0);
        assert_eq!(rates.actual_presented_multiplier, 0.0);
    }
}

#[cfg(all(test, feature = "streamline-fg"))]
mod tests {
    use super::{
        FRAME_GENERATION_WARMUP_PRESENT_LIMIT, FrameGenerationActivation,
        FrameGenerationController, FrameGenerationFault, FrameGenerationLifecycle,
        FrameGenerationMode, FrameGenerationObservation,
    };

    #[test]
    fn frame_generation_state_machine_covers_enable_suspend_resume_disable() {
        let mut controller = FrameGenerationController::new(FrameGenerationMode::On, true);
        assert_eq!(
            controller.lifecycle(),
            FrameGenerationLifecycle::EnablingProxy
        );
        assert!(controller.accept_complete_frame().is_ok());
        assert_eq!(controller.lifecycle(), FrameGenerationLifecycle::OnProxy);
        assert!(controller.suspend().is_ok());
        assert_eq!(
            controller.lifecycle(),
            FrameGenerationLifecycle::SuspendedProxy
        );
        assert!(controller.accept_complete_frame().is_ok());
        assert!(controller.request_disable().is_ok());
        assert_eq!(controller.lifecycle(), FrameGenerationLifecycle::Disabling);
        controller.complete_disable();
        assert_eq!(controller.lifecycle(), FrameGenerationLifecycle::OffNative);
        assert_eq!(controller.requested(), FrameGenerationMode::Off);
    }

    #[test]
    fn unsupported_and_invalid_transitions_do_not_change_state() {
        let mut unavailable = FrameGenerationController::new(FrameGenerationMode::On, false);
        assert_eq!(
            unavailable.lifecycle(),
            FrameGenerationLifecycle::Unavailable
        );
        assert!(unavailable.request_disable().is_err());
        assert_eq!(
            unavailable.lifecycle(),
            FrameGenerationLifecycle::Unavailable
        );

        let mut off = FrameGenerationController::new(FrameGenerationMode::Off, true);
        assert!(off.accept_complete_frame().is_err());
        assert!(off.request_enable().is_ok());
        assert_eq!(off.lifecycle(), FrameGenerationLifecycle::OffNative);
        assert!(off.complete_enable().is_ok());
        off.fault(17);
        assert_eq!(
            off.lifecycle(),
            FrameGenerationLifecycle::FaultPendingDisable(FrameGenerationFault::SdkStatus(17))
        );
        assert!(off.lifecycle().proxy_loaded());
    }

    #[test]
    fn frame_generation_requires_real_generated_frame_within_bounded_warmup() {
        let mut startup = FrameGenerationController::new(FrameGenerationMode::On, true);
        startup.accept_complete_frame().unwrap();
        for _ in 1..FRAME_GENERATION_WARMUP_PRESENT_LIMIT {
            assert_eq!(
                startup.observe_present(1, true),
                FrameGenerationObservation::Pending
            );
        }
        assert_eq!(
            startup.observe_present(1, true),
            FrameGenerationObservation::WarmupExpired(FrameGenerationActivation::Startup)
        );
        startup.warmup_timeout();
        assert_eq!(
            startup.lifecycle(),
            FrameGenerationLifecycle::FaultPendingDisable(FrameGenerationFault::WarmupTimeout)
        );
        assert!(startup.lifecycle().proxy_loaded());

        let mut interactive = FrameGenerationController::new(FrameGenerationMode::Off, true);
        interactive.request_enable().unwrap();
        interactive.complete_enable().unwrap();
        interactive.accept_complete_frame().unwrap();
        assert_eq!(
            interactive.observe_present(2, true),
            FrameGenerationObservation::Confirmed
        );
        assert_eq!(
            interactive.observe_present(1, true),
            FrameGenerationObservation::AlreadyConfirmed
        );
    }

    #[test]
    fn unfocused_presents_do_not_consume_frame_generation_warmup() {
        let mut controller = FrameGenerationController::new(FrameGenerationMode::On, true);
        controller.accept_complete_frame().unwrap();
        for _ in 0..FRAME_GENERATION_WARMUP_PRESENT_LIMIT * 2 {
            assert_eq!(
                controller.observe_present(1, false),
                FrameGenerationObservation::Ineligible
            );
        }
        assert_eq!(controller.warmup_presents(), 0);
        assert_eq!(
            controller.observe_present(2, false),
            FrameGenerationObservation::Confirmed
        );
    }

    #[test]
    fn unsupported_feature_is_unavailable_even_when_requested_off() {
        let unavailable = FrameGenerationController::new(FrameGenerationMode::Off, false);
        assert_eq!(
            unavailable.lifecycle(),
            FrameGenerationLifecycle::Unavailable
        );
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
