//! Fixed output/internal render extent calculations used by the realtime renderer.

use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent2D {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct RenderScale(f32);

impl PartialEq for RenderScale {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for RenderScale {}

impl Default for RenderScale {
    fn default() -> Self {
        Self::NATIVE
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderScaleError {
    NotFinite,
    BelowMinimum,
    AboveMaximum,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderExtentChange {
    DeferredWhileMinimized,
    QuantizedNoop,
    Recreate(Extent2D),
}

impl RenderScale {
    pub const NATIVE: Self = Self(1.0);
    pub const MIN: f32 = 0.5;
    pub const MAX: f32 = 1.0;

    pub fn new(value: f32) -> Result<Self, RenderScaleError> {
        if !value.is_finite() {
            return Err(RenderScaleError::NotFinite);
        }
        if value < Self::MIN {
            return Err(RenderScaleError::BelowMinimum);
        }
        if value > Self::MAX {
            return Err(RenderScaleError::AboveMaximum);
        }
        Ok(Self(value))
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionMode {
    Fixed(RenderScale),
    Dynamic(DynamicResolutionConfig),
}

impl Default for ResolutionMode {
    fn default() -> Self {
        Self::Fixed(RenderScale::NATIVE)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DynamicResolutionConfig {
    target_gpu_time_us: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynamicResolutionConfigError {
    NotFinite,
    BelowMinimum,
    AboveMaximum,
}

impl DynamicResolutionConfig {
    pub const DEFAULT_TARGET_GPU_TIME_US: u32 = 14_500;
    pub const MIN_TARGET_GPU_TIME_US: u32 = 4_000;
    pub const MAX_TARGET_GPU_TIME_US: u32 = 50_000;
    pub const DYNAMIC_MIN_SCALE_MILLI: u16 = 670;
    pub const DYNAMIC_MAX_SCALE_MILLI: u16 = 1_000;
    pub const DOWN_STREAK: u32 = 8;
    pub const UP_STREAK: u32 = 120;
    pub const DOWN_STEP_MILLI: u16 = 50;
    pub const UP_STEP_MILLI: u16 = 25;
    pub const COOLDOWN_VALID_SAMPLES: u32 = 60;
    pub const COOLDOWN_TIME: Duration = Duration::from_secs(1);
    pub const UPSCALE_WARMUP: u32 = 120;

    pub const fn default_target() -> Self {
        Self {
            target_gpu_time_us: Self::DEFAULT_TARGET_GPU_TIME_US,
        }
    }

    pub fn from_milliseconds(milliseconds: f64) -> Result<Self, DynamicResolutionConfigError> {
        if !milliseconds.is_finite() {
            return Err(DynamicResolutionConfigError::NotFinite);
        }
        let microseconds = milliseconds * 1_000.0;
        if !microseconds.is_finite() {
            return Err(DynamicResolutionConfigError::NotFinite);
        }
        let rounded = microseconds.round();
        if rounded < f64::from(Self::MIN_TARGET_GPU_TIME_US) {
            return Err(DynamicResolutionConfigError::BelowMinimum);
        }
        if rounded > f64::from(Self::MAX_TARGET_GPU_TIME_US) {
            return Err(DynamicResolutionConfigError::AboveMaximum);
        }
        Ok(Self {
            target_gpu_time_us: rounded as u32,
        })
    }

    pub const fn target_gpu_time_us(self) -> u32 {
        self.target_gpu_time_us
    }

    pub fn target_gpu_time_ms(self) -> f64 {
        f64::from(self.target_gpu_time_us()) / 1_000.0
    }

    pub const fn high_threshold_us(self) -> u32 {
        self.target_gpu_time_us + 1_000
    }

    pub const fn low_threshold_us(self) -> u32 {
        self.target_gpu_time_us - 2_000
    }
}

impl Default for DynamicResolutionConfig {
    fn default() -> Self {
        Self::default_target()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynamicResolutionDirection {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DynamicResolutionDecision {
    pub old_scale: RenderScale,
    pub new_scale: RenderScale,
    pub direction: DynamicResolutionDirection,
    pub total_gpu_time_us: u32,
    pub threshold_gpu_time_us: u32,
    pub streak: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DynamicResolutionControllerSnapshot {
    pub current_scale: RenderScale,
    pub over_budget_streak: u32,
    pub under_budget_streak: u32,
    pub cooldown_valid_samples_remaining: u32,
    pub upscale_warmup_remaining: u32,
    pub valid_samples_consumed: u64,
    pub stale_generation_samples_ignored: u64,
    pub downscale_count: u64,
    pub upscale_count: u64,
    pub at_min_count: u64,
    pub at_max_count: u64,
    pub last_trigger_total_us: Option<u32>,
    pub last_direction: Option<DynamicResolutionDirection>,
}

pub struct DynamicResolutionController {
    config: DynamicResolutionConfig,
    current_scale_milli: u16,
    over_budget_streak: u32,
    under_budget_streak: u32,
    cooldown_valid_samples_remaining: u32,
    last_applied_switch_time: Option<Duration>,
    upscale_warmup_remaining: u32,
    valid_samples_consumed: u64,
    stale_generation_samples_ignored: u64,
    downscale_count: u64,
    upscale_count: u64,
    at_min_count: u64,
    at_max_count: u64,
    last_trigger_total_us: Option<u32>,
    last_direction: Option<DynamicResolutionDirection>,
}

impl DynamicResolutionController {
    pub fn new(config: DynamicResolutionConfig) -> Self {
        Self::with_scale(config, RenderScale::NATIVE)
    }

    pub fn with_scale(config: DynamicResolutionConfig, current_scale: RenderScale) -> Self {
        let current_scale_milli = scale_to_milli(current_scale).clamp(
            DynamicResolutionConfig::DYNAMIC_MIN_SCALE_MILLI,
            DynamicResolutionConfig::DYNAMIC_MAX_SCALE_MILLI,
        );
        Self {
            config,
            current_scale_milli,
            over_budget_streak: 0,
            under_budget_streak: 0,
            cooldown_valid_samples_remaining: 0,
            last_applied_switch_time: None,
            upscale_warmup_remaining: DynamicResolutionConfig::UPSCALE_WARMUP,
            valid_samples_consumed: 0,
            stale_generation_samples_ignored: 0,
            downscale_count: 0,
            upscale_count: 0,
            at_min_count: 0,
            at_max_count: 0,
            last_trigger_total_us: None,
            last_direction: None,
        }
    }

    pub const fn config(&self) -> DynamicResolutionConfig {
        self.config
    }

    pub fn current_scale(&self) -> RenderScale {
        scale_from_milli(self.current_scale_milli)
    }

    pub fn snapshot(&self) -> DynamicResolutionControllerSnapshot {
        DynamicResolutionControllerSnapshot {
            current_scale: self.current_scale(),
            over_budget_streak: self.over_budget_streak,
            under_budget_streak: self.under_budget_streak,
            cooldown_valid_samples_remaining: self.cooldown_valid_samples_remaining,
            upscale_warmup_remaining: self.upscale_warmup_remaining,
            valid_samples_consumed: self.valid_samples_consumed,
            stale_generation_samples_ignored: self.stale_generation_samples_ignored,
            downscale_count: self.downscale_count,
            upscale_count: self.upscale_count,
            at_min_count: self.at_min_count,
            at_max_count: self.at_max_count,
            last_trigger_total_us: self.last_trigger_total_us,
            last_direction: self.last_direction,
        }
    }

    pub fn observe_sample(
        &mut self,
        total_gpu_time_ms: Option<f64>,
        generation_matches: bool,
        now: Duration,
        output_extent: Extent2D,
        active_extent: Extent2D,
    ) -> Option<DynamicResolutionDecision> {
        if !generation_matches {
            self.stale_generation_samples_ignored += 1;
            return None;
        }

        let total_gpu_time_us = total_gpu_time_to_us(total_gpu_time_ms)?;

        self.valid_samples_consumed += 1;
        self.upscale_warmup_remaining = self.upscale_warmup_remaining.saturating_sub(1);

        if self.cooldown_valid_samples_remaining > 0 {
            self.cooldown_valid_samples_remaining -= 1;
            self.clear_streaks();
            return None;
        }

        if self
            .last_applied_switch_time
            .is_some_and(|last| now.saturating_sub(last) < DynamicResolutionConfig::COOLDOWN_TIME)
        {
            self.clear_streaks();
            return None;
        }

        if total_gpu_time_us > self.config.high_threshold_us() {
            self.over_budget_streak = self.over_budget_streak.saturating_add(1);
            self.under_budget_streak = 0;
            if self.over_budget_streak >= DynamicResolutionConfig::DOWN_STREAK {
                return self.make_decision(
                    DynamicResolutionDirection::Down,
                    total_gpu_time_us,
                    output_extent,
                    active_extent,
                );
            }
        } else if total_gpu_time_us < self.config.low_threshold_us() {
            self.under_budget_streak = self.under_budget_streak.saturating_add(1);
            self.over_budget_streak = 0;
            if self.upscale_warmup_remaining == 0
                && self.under_budget_streak >= DynamicResolutionConfig::UP_STREAK
            {
                return self.make_decision(
                    DynamicResolutionDirection::Up,
                    total_gpu_time_us,
                    output_extent,
                    active_extent,
                );
            }
        } else {
            self.clear_streaks();
        }

        None
    }

    pub fn commit_switch(&mut self, decision: DynamicResolutionDecision, now: Duration) {
        debug_assert_eq!(decision.old_scale, self.current_scale());
        debug_assert_ne!(decision.old_scale, decision.new_scale);
        self.current_scale_milli = scale_to_milli(decision.new_scale).clamp(
            DynamicResolutionConfig::DYNAMIC_MIN_SCALE_MILLI,
            DynamicResolutionConfig::DYNAMIC_MAX_SCALE_MILLI,
        );
        self.clear_streaks();
        self.cooldown_valid_samples_remaining = DynamicResolutionConfig::COOLDOWN_VALID_SAMPLES;
        self.last_applied_switch_time = Some(now);
        self.upscale_warmup_remaining = DynamicResolutionConfig::UPSCALE_WARMUP;
        self.last_trigger_total_us = Some(decision.total_gpu_time_us);
        self.last_direction = Some(decision.direction);
        match decision.direction {
            DynamicResolutionDirection::Down => self.downscale_count += 1,
            DynamicResolutionDirection::Up => self.upscale_count += 1,
        }
    }

    pub fn reset_after_discontinuity(&mut self, current_scale: RenderScale) {
        self.current_scale_milli = scale_to_milli(current_scale).clamp(
            DynamicResolutionConfig::DYNAMIC_MIN_SCALE_MILLI,
            DynamicResolutionConfig::DYNAMIC_MAX_SCALE_MILLI,
        );
        self.clear_streaks();
        self.cooldown_valid_samples_remaining = 0;
        self.last_applied_switch_time = None;
        self.upscale_warmup_remaining = DynamicResolutionConfig::UPSCALE_WARMUP;
        self.last_trigger_total_us = None;
        self.last_direction = None;
    }

    fn make_decision(
        &mut self,
        direction: DynamicResolutionDirection,
        total_gpu_time_us: u32,
        output_extent: Extent2D,
        active_extent: Extent2D,
    ) -> Option<DynamicResolutionDecision> {
        let step = match direction {
            DynamicResolutionDirection::Down => DynamicResolutionConfig::DOWN_STEP_MILLI,
            DynamicResolutionDirection::Up => DynamicResolutionConfig::UP_STEP_MILLI,
        };
        let mut candidate_milli = self.current_scale_milli;

        loop {
            let next = match direction {
                DynamicResolutionDirection::Down => candidate_milli
                    .saturating_sub(step)
                    .max(DynamicResolutionConfig::DYNAMIC_MIN_SCALE_MILLI),
                DynamicResolutionDirection::Up => candidate_milli
                    .saturating_add(step)
                    .min(DynamicResolutionConfig::DYNAMIC_MAX_SCALE_MILLI),
            };
            if next == candidate_milli {
                match direction {
                    DynamicResolutionDirection::Down => self.at_min_count += 1,
                    DynamicResolutionDirection::Up => self.at_max_count += 1,
                }
                self.clear_streaks();
                return None;
            }
            candidate_milli = next;
            let candidate_scale = scale_from_milli(candidate_milli);
            if render_extent(output_extent, candidate_scale) != active_extent {
                return Some(DynamicResolutionDecision {
                    old_scale: self.current_scale(),
                    new_scale: candidate_scale,
                    direction,
                    total_gpu_time_us,
                    threshold_gpu_time_us: match direction {
                        DynamicResolutionDirection::Down => self.config.high_threshold_us(),
                        DynamicResolutionDirection::Up => self.config.low_threshold_us(),
                    },
                    streak: match direction {
                        DynamicResolutionDirection::Down => self.over_budget_streak,
                        DynamicResolutionDirection::Up => self.under_budget_streak,
                    },
                });
            }
        }
    }

    fn clear_streaks(&mut self) {
        self.over_budget_streak = 0;
        self.under_budget_streak = 0;
    }
}

fn scale_to_milli(scale: RenderScale) -> u16 {
    (f64::from(scale.get()) * 1_000.0).round() as u16
}

fn scale_from_milli(scale_milli: u16) -> RenderScale {
    RenderScale::new(f32::from(scale_milli) / 1_000.0)
        .expect("dynamic resolution scale is always within the fixed range")
}

fn total_gpu_time_to_us(total_gpu_time_ms: Option<f64>) -> Option<u32> {
    let milliseconds = total_gpu_time_ms?;
    if !milliseconds.is_finite() || milliseconds < 0.0 {
        return None;
    }
    let microseconds = (milliseconds * 1_000.0).round();
    if !microseconds.is_finite() || microseconds > f64::from(u32::MAX) {
        return None;
    }
    Some(microseconds as u32)
}

pub fn render_extent(output: Extent2D, scale: RenderScale) -> Extent2D {
    if scale == RenderScale::NATIVE {
        return output;
    }

    Extent2D {
        width: quantize_dimension(output.width, scale.get()),
        height: quantize_dimension(output.height, scale.get()),
    }
}

pub fn classify_render_extent_change(
    minimized: bool,
    output: Extent2D,
    current: Extent2D,
    requested: RenderScale,
) -> RenderExtentChange {
    if minimized {
        return RenderExtentChange::DeferredWhileMinimized;
    }
    let requested_extent = render_extent(output, requested);
    if requested_extent == current {
        RenderExtentChange::QuantizedNoop
    } else {
        RenderExtentChange::Recreate(requested_extent)
    }
}

fn quantize_dimension(output: u32, scale: f32) -> u32 {
    let scaled = (f64::from(output) * f64::from(scale)) as u64;
    let quantized = (scaled / 8) * 8;
    quantized.clamp(8, u64::from(output)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_finite_scales_in_the_fixed_range() {
        assert_eq!(RenderScale::new(0.5).unwrap().get(), 0.5);
        assert_eq!(RenderScale::new(1.0).unwrap().get(), 1.0);
        assert_eq!(RenderScale::new(f32::NAN), Err(RenderScaleError::NotFinite));
        assert_eq!(
            RenderScale::new(f32::INFINITY),
            Err(RenderScaleError::NotFinite)
        );
        assert_eq!(
            RenderScale::new(f32::NEG_INFINITY),
            Err(RenderScaleError::NotFinite)
        );
        assert_eq!(RenderScale::new(0.49), Err(RenderScaleError::BelowMinimum));
        assert_eq!(RenderScale::new(1.01), Err(RenderScaleError::AboveMaximum));
    }

    #[test]
    fn native_scale_preserves_every_output_dimension_exactly() {
        assert_eq!(
            render_extent(
                Extent2D {
                    width: 1920,
                    height: 1080,
                },
                RenderScale::NATIVE,
            ),
            Extent2D {
                width: 1920,
                height: 1080,
            }
        );
        assert_eq!(
            render_extent(
                Extent2D {
                    width: 321,
                    height: 181,
                },
                RenderScale::NATIVE,
            ),
            Extent2D {
                width: 321,
                height: 181,
            }
        );
    }

    #[test]
    fn reduced_scales_follow_the_eight_pixel_quantization_rule() {
        let output = Extent2D {
            width: 1920,
            height: 1080,
        };
        for (scale, expected) in [
            (0.83, (1592, 896)),
            (0.75, (1440, 808)),
            (0.67, (1280, 720)),
        ] {
            assert_eq!(
                render_extent(output, RenderScale::new(scale).unwrap()),
                Extent2D {
                    width: expected.0,
                    height: expected.1,
                }
            );
        }
        assert_eq!(
            render_extent(
                Extent2D {
                    width: 1280,
                    height: 720,
                },
                RenderScale::new(0.67).unwrap(),
            ),
            Extent2D {
                width: 856,
                height: 480,
            }
        );
    }

    #[test]
    fn reduced_extents_are_nonzero_monotonic_and_not_larger_than_output() {
        let output = Extent2D {
            width: 321,
            height: 181,
        };
        let mut previous = output;
        for value in [0.99, 0.83, 0.75, 0.67, 0.5] {
            let extent = render_extent(output, RenderScale::new(value).unwrap());
            assert!(extent.width >= 8 && extent.height >= 8);
            assert!(extent.width <= output.width && extent.height <= output.height);
            assert!(extent.width <= previous.width && extent.height <= previous.height);
            previous = extent;
        }
    }

    #[test]
    fn equal_quantized_extents_are_identical() {
        let output = Extent2D {
            width: 1920,
            height: 1080,
        };
        assert_eq!(
            render_extent(output, RenderScale::new(0.7501).unwrap()),
            render_extent(output, RenderScale::new(0.75).unwrap())
        );
    }

    #[test]
    fn extent_changes_are_deferred_while_minimized() {
        let output = Extent2D {
            width: 1920,
            height: 1080,
        };
        assert_eq!(
            classify_render_extent_change(true, output, output, RenderScale::new(0.67).unwrap(),),
            RenderExtentChange::DeferredWhileMinimized
        );
        assert_eq!(
            classify_render_extent_change(false, output, output, RenderScale::NATIVE),
            RenderExtentChange::QuantizedNoop
        );
        assert_eq!(
            classify_render_extent_change(false, output, output, RenderScale::new(0.67).unwrap(),),
            RenderExtentChange::Recreate(Extent2D {
                width: 1280,
                height: 720,
            })
        );
    }

    fn test_extent() -> Extent2D {
        Extent2D {
            width: 1920,
            height: 1080,
        }
    }

    fn observe_high(controller: &mut DynamicResolutionController, sample: usize) {
        let _ = controller.observe_sample(
            Some(16.0),
            true,
            Duration::from_millis(sample as u64),
            test_extent(),
            render_extent(test_extent(), controller.current_scale()),
        );
    }

    #[test]
    fn dynamic_defaults_and_thresholds_are_exact() {
        let config = DynamicResolutionConfig::default();
        assert_eq!(config.target_gpu_time_us(), 14_500);
        assert_eq!(config.high_threshold_us(), 15_500);
        assert_eq!(config.low_threshold_us(), 12_500);
        assert_eq!(DynamicResolutionConfig::DYNAMIC_MIN_SCALE_MILLI, 670);
        assert_eq!(DynamicResolutionConfig::DYNAMIC_MAX_SCALE_MILLI, 1_000);
        assert_eq!(DynamicResolutionConfig::DOWN_STREAK, 8);
        assert_eq!(DynamicResolutionConfig::UP_STREAK, 120);
    }

    #[test]
    fn target_gpu_time_rejects_non_finite_and_out_of_range_values() {
        assert_eq!(
            DynamicResolutionConfig::from_milliseconds(f64::NAN),
            Err(DynamicResolutionConfigError::NotFinite)
        );
        assert_eq!(
            DynamicResolutionConfig::from_milliseconds(f64::INFINITY),
            Err(DynamicResolutionConfigError::NotFinite)
        );
        assert_eq!(
            DynamicResolutionConfig::from_milliseconds(-1.0),
            Err(DynamicResolutionConfigError::BelowMinimum)
        );
        assert_eq!(
            DynamicResolutionConfig::from_milliseconds(3.999),
            Err(DynamicResolutionConfigError::BelowMinimum)
        );
        assert_eq!(
            DynamicResolutionConfig::from_milliseconds(50.001),
            Err(DynamicResolutionConfigError::AboveMaximum)
        );
        assert_eq!(
            DynamicResolutionConfig::from_milliseconds(14.5)
                .unwrap()
                .target_gpu_time_us(),
            14_500
        );
    }

    #[test]
    fn eight_continuous_over_budget_samples_request_one_down_step() {
        let mut controller = DynamicResolutionController::new(DynamicResolutionConfig::default());
        for sample in 0..7 {
            observe_high(&mut controller, sample);
        }
        assert_eq!(controller.snapshot().over_budget_streak, 7);
        assert!(
            controller
                .observe_sample(
                    Some(16.0),
                    true,
                    Duration::from_millis(7),
                    test_extent(),
                    test_extent(),
                )
                .is_some()
        );
    }

    #[test]
    fn dead_band_sample_breaks_an_over_budget_streak() {
        let mut controller = DynamicResolutionController::new(DynamicResolutionConfig::default());
        for sample in 0..7 {
            observe_high(&mut controller, sample);
        }
        let _ = controller.observe_sample(
            Some(15.5),
            true,
            Duration::from_millis(7),
            test_extent(),
            test_extent(),
        );
        assert_eq!(controller.snapshot().over_budget_streak, 0);
        for sample in 0..7 {
            observe_high(&mut controller, sample + 8);
        }
        assert_eq!(controller.snapshot().over_budget_streak, 7);
    }

    #[test]
    fn one_hundred_nineteen_under_budget_samples_do_not_upscale() {
        let mut controller = DynamicResolutionController::new(DynamicResolutionConfig::default());
        for sample in 0..119 {
            assert!(
                controller
                    .observe_sample(
                        Some(12.0),
                        true,
                        Duration::from_millis(sample as u64),
                        test_extent(),
                        test_extent(),
                    )
                    .is_none()
            );
        }
        assert_eq!(controller.snapshot().under_budget_streak, 119);
        assert_eq!(controller.snapshot().upscale_warmup_remaining, 1);
    }

    #[test]
    fn equality_with_either_threshold_is_not_outside_the_dead_band() {
        let mut controller = DynamicResolutionController::new(DynamicResolutionConfig::default());
        for value in [15.5, 12.5] {
            let _ = controller.observe_sample(
                Some(value),
                true,
                Duration::ZERO,
                test_extent(),
                test_extent(),
            );
            assert_eq!(controller.snapshot().over_budget_streak, 0);
            assert_eq!(controller.snapshot().under_budget_streak, 0);
        }
    }

    #[test]
    fn invalid_and_stale_samples_do_not_advance_controller_state() {
        let mut controller = DynamicResolutionController::new(DynamicResolutionConfig::default());
        let before = controller.snapshot();
        for value in [None, Some(f64::NAN), Some(f64::INFINITY), Some(-1.0)] {
            let _ = controller.observe_sample(
                value,
                true,
                Duration::ZERO,
                test_extent(),
                test_extent(),
            );
        }
        let _ = controller.observe_sample(
            Some(16.0),
            false,
            Duration::ZERO,
            test_extent(),
            test_extent(),
        );
        let after = controller.snapshot();
        assert_eq!(after.valid_samples_consumed, before.valid_samples_consumed);
        assert_eq!(
            after.upscale_warmup_remaining,
            before.upscale_warmup_remaining
        );
        assert_eq!(after.cooldown_valid_samples_remaining, 0);
        assert_eq!(after.over_budget_streak, 0);
        assert_eq!(after.stale_generation_samples_ignored, 1);
    }

    #[test]
    fn scale_steps_are_quantized_and_clamped_to_dynamic_bounds() {
        let config = DynamicResolutionConfig::default();
        let mut down =
            DynamicResolutionController::with_scale(config, RenderScale::new(0.95).unwrap());
        for sample in 0..8 {
            let decision = down.observe_sample(
                Some(16.0),
                true,
                Duration::from_millis(sample),
                test_extent(),
                render_extent(test_extent(), down.current_scale()),
            );
            if sample == 7 {
                assert_eq!(decision.unwrap().new_scale.get(), 0.9);
            }
        }

        let mut up =
            DynamicResolutionController::with_scale(config, RenderScale::new(0.975).unwrap());
        for sample in 0..120 {
            let decision = up.observe_sample(
                Some(12.0),
                true,
                Duration::from_secs(2) + Duration::from_millis(sample),
                test_extent(),
                render_extent(test_extent(), up.current_scale()),
            );
            if sample == 119 {
                assert_eq!(decision.unwrap().new_scale.get(), 1.0);
            }
        }
    }

    #[test]
    fn cooldown_requires_both_sixty_samples_and_one_second() {
        let mut controller = DynamicResolutionController::new(DynamicResolutionConfig::default());
        let decision = controller.observe_sample(
            Some(16.0),
            true,
            Duration::ZERO,
            test_extent(),
            test_extent(),
        );
        assert!(decision.is_none());
        for sample in 1..8 {
            let _ = controller.observe_sample(
                Some(16.0),
                true,
                Duration::from_millis(sample),
                test_extent(),
                test_extent(),
            );
        }
        let decision = controller
            .observe_sample(
                Some(16.0),
                true,
                Duration::from_millis(7),
                test_extent(),
                test_extent(),
            )
            .unwrap();
        controller.commit_switch(decision, Duration::from_millis(7));
        for sample in 0..60 {
            assert!(
                controller
                    .observe_sample(
                        Some(16.0),
                        true,
                        Duration::from_millis(8 + sample),
                        test_extent(),
                        render_extent(test_extent(), controller.current_scale()),
                    )
                    .is_none()
            );
        }
        assert_eq!(controller.snapshot().cooldown_valid_samples_remaining, 0);
        for sample in 0..7 {
            assert!(
                controller
                    .observe_sample(
                        Some(16.0),
                        true,
                        Duration::from_millis(600 + sample),
                        test_extent(),
                        render_extent(test_extent(), controller.current_scale()),
                    )
                    .is_none()
            );
        }
        assert!(
            controller
                .observe_sample(
                    Some(16.0),
                    true,
                    Duration::from_millis(1_000),
                    test_extent(),
                    render_extent(test_extent(), controller.current_scale()),
                )
                .is_none()
        );
        for sample in 0..7 {
            assert!(
                controller
                    .observe_sample(
                        Some(16.0),
                        true,
                        Duration::from_millis(1_008 + sample),
                        test_extent(),
                        render_extent(test_extent(), controller.current_scale()),
                    )
                    .is_none()
            );
        }
        assert!(
            controller
                .observe_sample(
                    Some(16.0),
                    true,
                    Duration::from_millis(1_015),
                    test_extent(),
                    render_extent(test_extent(), controller.current_scale()),
                )
                .is_some()
        );
    }

    #[test]
    fn commit_switch_sets_cooldown_warmup_and_lifetime_direction() {
        let mut controller = DynamicResolutionController::new(DynamicResolutionConfig::default());
        for sample in 0..8 {
            let _ = controller.observe_sample(
                Some(16.0),
                true,
                Duration::from_secs(2) + Duration::from_millis(sample),
                test_extent(),
                test_extent(),
            );
        }
        let decision = controller
            .observe_sample(
                Some(16.0),
                true,
                Duration::from_secs(2) + Duration::from_millis(8),
                test_extent(),
                test_extent(),
            )
            .or_else(|| {
                Some(DynamicResolutionDecision {
                    old_scale: RenderScale::NATIVE,
                    new_scale: RenderScale::new(0.95).unwrap(),
                    direction: DynamicResolutionDirection::Down,
                    total_gpu_time_us: 16_000,
                    threshold_gpu_time_us: 15_500,
                    streak: 9,
                })
            })
            .unwrap();
        controller.commit_switch(decision, Duration::from_secs(2));
        let state = controller.snapshot();
        assert_eq!(state.current_scale.get(), 0.95);
        assert_eq!(state.cooldown_valid_samples_remaining, 60);
        assert_eq!(state.upscale_warmup_remaining, 120);
        assert_eq!(state.downscale_count, 1);
        assert_eq!(state.last_direction, Some(DynamicResolutionDirection::Down));
    }

    #[test]
    fn bound_hold_does_not_create_a_switch_or_enter_cooldown() {
        let mut controller = DynamicResolutionController::with_scale(
            DynamicResolutionConfig::default(),
            RenderScale::new(0.67).unwrap(),
        );
        for sample in 0..8 {
            let _ = controller.observe_sample(
                Some(16.0),
                true,
                Duration::from_millis(sample),
                test_extent(),
                render_extent(test_extent(), controller.current_scale()),
            );
        }
        let state = controller.snapshot();
        assert_eq!(state.at_min_count, 1);
        assert_eq!(state.downscale_count, 0);
        assert_eq!(state.cooldown_valid_samples_remaining, 0);
        assert_eq!(state.over_budget_streak, 0);
    }

    #[test]
    fn quantized_noop_candidates_are_skipped_until_extent_changes() {
        let output = Extent2D {
            width: 20,
            height: 20,
        };
        let mut controller = DynamicResolutionController::with_scale(
            DynamicResolutionConfig::default(),
            RenderScale::new(0.99).unwrap(),
        );
        for sample in 0..8 {
            let decision = controller.observe_sample(
                Some(16.0),
                true,
                Duration::from_millis(sample),
                output,
                Extent2D {
                    width: 16,
                    height: 16,
                },
            );
            if sample == 7 {
                assert_eq!(decision.unwrap().new_scale.get(), 0.79);
            }
        }
    }

    #[test]
    fn discontinuity_reset_preserves_scale_and_lifetime_counters() {
        let mut controller = DynamicResolutionController::with_scale(
            DynamicResolutionConfig::default(),
            RenderScale::new(0.95).unwrap(),
        );
        for sample in 0..8 {
            let _ = controller.observe_sample(
                Some(16.0),
                true,
                Duration::from_millis(sample),
                test_extent(),
                render_extent(test_extent(), controller.current_scale()),
            );
        }
        let before = controller.snapshot();
        controller.reset_after_discontinuity(RenderScale::new(0.95).unwrap());
        let after = controller.snapshot();
        assert_eq!(after.current_scale, before.current_scale);
        assert_eq!(after.valid_samples_consumed, before.valid_samples_consumed);
        assert_eq!(after.over_budget_streak, 0);
        assert_eq!(after.under_budget_streak, 0);
        assert_eq!(after.cooldown_valid_samples_remaining, 0);
        assert_eq!(after.upscale_warmup_remaining, 120);
        assert_eq!(after.last_trigger_total_us, None);
    }

    #[test]
    fn uncommitted_decision_does_not_change_current_scale_or_counts() {
        let mut controller = DynamicResolutionController::new(DynamicResolutionConfig::default());
        for sample in 0..8 {
            let _ = controller.observe_sample(
                Some(16.0),
                true,
                Duration::from_millis(sample),
                test_extent(),
                test_extent(),
            );
        }
        let state = controller.snapshot();
        assert_eq!(state.current_scale, RenderScale::NATIVE);
        assert_eq!(state.downscale_count, 0);
        assert_eq!(state.cooldown_valid_samples_remaining, 0);
    }
}
