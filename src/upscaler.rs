//! Contract and CPU-side policy for the optional DLSS SR/DLAA adapter.
//!
//! The renderer deliberately keeps this contract separate from the NRD
//! reconstruction contract.  In particular, DLSS consumes row-major,
//! non-jittered matrices and a pixel-motion buffer with its own documented
//! sign convention.  The NRD motion helpers must not be reused as an
//! accidental DLSS ABI.

use glam::{Mat4, Vec3, Vec4};

use crate::{reconstruction::CameraPose, resolution::Extent2D};

pub const DLSS_CONTRACT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpscalerMode {
    #[default]
    Native,
    Dlaa,
    DlssQuality,
    DlssBalanced,
    DlssPerformance,
}

impl UpscalerMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Dlaa => "dlaa",
            Self::DlssQuality => "dlss-quality",
            Self::DlssBalanced => "dlss-balanced",
            Self::DlssPerformance => "dlss-performance",
        }
    }

    pub const fn is_native(self) -> bool {
        matches!(self, Self::Native)
    }

    pub const fn uses_streamline(self) -> bool {
        !self.is_native()
    }

    pub const fn next_mode(self) -> Self {
        match self {
            Self::Native => Self::Dlaa,
            Self::Dlaa => Self::DlssQuality,
            Self::DlssQuality => Self::DlssBalanced,
            Self::DlssBalanced => Self::DlssPerformance,
            Self::DlssPerformance => Self::Native,
        }
    }

    /// Return a stable mode number for the later Streamline adapter.
    pub const fn dlss_mode(self) -> Option<u32> {
        match self {
            Self::Dlaa => Some(1),
            Self::DlssQuality => Some(2),
            Self::DlssBalanced => Some(3),
            Self::DlssPerformance => Some(4),
            Self::Native => None,
        }
    }

    pub fn requested_startup_error(self) -> Option<String> {
        if self.uses_streamline() && !cfg!(feature = "streamline") {
            Some(format!(
                "--upscaler {} 需要使用 `cargo run --features streamline --` 重新构建；当前构建未包含 Streamline",
                self.as_str()
            ))
        } else {
            None
        }
    }
}

impl std::fmt::Display for UpscalerMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Results returned by `slDLSSGetOptimalSettings` and consumed by the
/// generation allocator.  The actual Streamline bridge is added in 10D; this
/// type keeps the extent decision explicit and testable before a GPU call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DlssOptimalSettings {
    pub render_width: u32,
    pub render_height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DlssExtentError {
    Zero,
    LargerThanOutput,
    DlaaMustMatchOutput,
}

pub fn render_extent_from_optimal(
    mode: UpscalerMode,
    output: Extent2D,
    optimal: DlssOptimalSettings,
) -> Result<Extent2D, DlssExtentError> {
    if optimal.render_width == 0 || optimal.render_height == 0 {
        return Err(DlssExtentError::Zero);
    }
    if mode == UpscalerMode::Dlaa {
        if optimal.render_width != output.width || optimal.render_height != output.height {
            return Err(DlssExtentError::DlaaMustMatchOutput);
        }
    } else if optimal.render_width > output.width || optimal.render_height > output.height {
        return Err(DlssExtentError::LargerThanOutput);
    }
    Ok(Extent2D {
        width: optimal.render_width,
        height: optimal.render_height,
    })
}

/// CPU representation of the DLSS input contract.  Matrices are flattened
/// row-major because that is the convention used by the Streamline DLSS
/// constants.  They contain no projection jitter; `jitter_px` is supplied as
/// a separate field and is never baked into either matrix.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DlssFrameInput {
    pub contract_version: u32,
    pub world_to_view: [f32; 16],
    pub world_to_view_prev: [f32; 16],
    pub view_to_clip: [f32; 16],
    pub view_to_clip_prev: [f32; 16],
    pub jitter_px: [f32; 2],
    pub jitter_prev_px: [f32; 2],
    pub motion_vector_scale: [f32; 2],
    pub render_width: u32,
    pub render_height: u32,
    pub previous_render_width: u32,
    pub previous_render_height: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub reset: u32,
    pub depth_inverted: u32,
    pub motion_vectors_jittered: u32,
    pub pre_exposure: f32,
}

impl DlssFrameInput {
    pub fn from_cameras(
        current_camera: CameraPose,
        previous_camera: CameraPose,
        render_extent: Extent2D,
        previous_render_extent: Extent2D,
        output_extent: Extent2D,
        frame_index: u32,
        reset: bool,
    ) -> Self {
        let render_aspect = render_extent.width.max(1) as f32 / render_extent.height.max(1) as f32;
        let previous_aspect = previous_render_extent.width.max(1) as f32
            / previous_render_extent.height.max(1) as f32;
        let (world_to_view, view_to_clip) =
            crate::reconstruction::camera_matrices(current_camera, render_aspect);
        let (world_to_view_prev, view_to_clip_prev) =
            crate::reconstruction::camera_matrices(previous_camera, previous_aspect);
        let jitter_px = projection_jitter(frame_index);
        let jitter_prev_px = if frame_index == 0 {
            [0.0; 2]
        } else {
            projection_jitter(frame_index - 1)
        };
        Self {
            contract_version: DLSS_CONTRACT_VERSION,
            world_to_view: to_row_major(world_to_view),
            world_to_view_prev: to_row_major(world_to_view_prev),
            view_to_clip: to_row_major(view_to_clip),
            view_to_clip_prev: to_row_major(view_to_clip_prev),
            jitter_px,
            jitter_prev_px,
            // Streamline's mvecScale converts pixel motion to normalized
            // motion; it is based on the input/render extent, not output.
            motion_vector_scale: [
                1.0 / render_extent.width.max(1) as f32,
                1.0 / render_extent.height.max(1) as f32,
            ],
            render_width: render_extent.width.max(1),
            render_height: render_extent.height.max(1),
            previous_render_width: previous_render_extent.width.max(1),
            previous_render_height: previous_render_extent.height.max(1),
            output_width: output_extent.width.max(1),
            output_height: output_extent.height.max(1),
            reset: u32::from(reset),
            // The renderer's depth is ordinary positive linear viewZ and the
            // later DLSS tag will set the matching non-inverted flag.
            depth_inverted: 0,
            motion_vectors_jittered: 0,
            pre_exposure: 1.0,
        }
    }

    pub fn reset_for_extent(mut self, render_extent: Extent2D, output_extent: Extent2D) -> Self {
        self.render_width = render_extent.width.max(1);
        self.render_height = render_extent.height.max(1);
        self.previous_render_width = self.render_width;
        self.previous_render_height = self.render_height;
        self.output_width = output_extent.width.max(1);
        self.output_height = output_extent.height.max(1);
        self.motion_vector_scale = [
            1.0 / self.render_width as f32,
            1.0 / self.render_height as f32,
        ];
        self.jitter_px = [0.0; 2];
        self.jitter_prev_px = [0.0; 2];
        self.reset = 1;
        self
    }
}

/// Deterministic pixel-space projection jitter.  It is intentionally
/// separate from the matrices and therefore does not contaminate motion
/// reconstruction.  The first sample is phase one of a Halton(2,3) sequence.
pub fn projection_jitter(frame_index: u32) -> [f32; 2] {
    let phase = frame_index.saturating_add(1);
    [
        radical_inverse(phase, 2) - 0.5,
        radical_inverse(phase, 3) - 0.5,
    ]
}

fn radical_inverse(mut value: u32, base: u32) -> f32 {
    let reciprocal = 1.0 / base as f32;
    let mut factor = reciprocal;
    let mut result = 0.0;
    while value != 0 {
        result += (value % base) as f32 * factor;
        value /= base;
        factor *= reciprocal;
    }
    result
}

/// Convert `glam`'s column-major array into the row-major flattening required
/// by DLSS.  This conversion is deliberately local to this module so the NRD
/// ABI remains column-major and unchanged.
pub fn to_row_major(column_major: [f32; 16]) -> [f32; 16] {
    Mat4::from_cols_array(&column_major)
        .transpose()
        .to_cols_array()
}

fn from_row_major(row_major: [f32; 16]) -> Mat4 {
    Mat4::from_cols_array(&row_major).transpose()
}

fn project_world_to_pixel(
    world_position: [f32; 3],
    world_to_view: [f32; 16],
    view_to_clip: [f32; 16],
    extent: Extent2D,
) -> Option<([f32; 2], f32)> {
    let view = from_row_major(world_to_view) * Vec4::from((Vec3::from_array(world_position), 1.0));
    let clip = from_row_major(view_to_clip) * view;
    if !clip.w.is_finite() || clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    let pixel = [
        (ndc.x * 0.5 + 0.5) * extent.width.max(1) as f32,
        (0.5 - ndc.y * 0.5) * extent.height.max(1) as f32,
    ];
    if pixel.iter().all(|value| value.is_finite()) && view.z.is_finite() && view.z > 0.0 {
        Some((pixel, view.z))
    } else {
        None
    }
}

/// Return pixel motion in the DLSS buffer convention: previous pixel minus
/// current pixel.  This is intentionally not `reconstruction::nrd_motion_pixels`.
pub fn dlss_motion_pixels(
    current_pixel: [f32; 2],
    previous_pixel: [f32; 2],
    reset: bool,
) -> [f32; 2] {
    if reset {
        [0.0; 2]
    } else {
        [
            previous_pixel[0] - current_pixel[0],
            previous_pixel[1] - current_pixel[1],
        ]
    }
}

/// Compute motion for a rigidly moving world point.  The current and previous
/// point are separate on purpose: ordinary object motion must not be turned
/// into a full-frame history reset.
pub fn dlss_motion_for_world_point(
    current_world_position: [f32; 3],
    previous_world_position: [f32; 3],
    input: &DlssFrameInput,
) -> Option<[f32; 2]> {
    let (current_pixel, _) = project_world_to_pixel(
        current_world_position,
        input.world_to_view,
        input.view_to_clip,
        Extent2D {
            width: input.render_width,
            height: input.render_height,
        },
    )?;
    let (previous_pixel, _) = project_world_to_pixel(
        previous_world_position,
        input.world_to_view_prev,
        input.view_to_clip_prev,
        Extent2D {
            width: input.previous_render_width,
            height: input.previous_render_height,
        },
    )?;
    Some(dlss_motion_pixels(
        current_pixel,
        previous_pixel,
        input.reset != 0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> CameraPose {
        CameraPose {
            position: [0.0, 0.0, -2.0],
            yaw: 0.0,
            pitch: 0.0,
        }
    }

    fn input(current: CameraPose, previous: CameraPose, reset: bool) -> DlssFrameInput {
        DlssFrameInput::from_cameras(
            current,
            previous,
            Extent2D {
                width: 1280,
                height: 720,
            },
            Extent2D {
                width: 1280,
                height: 720,
            },
            Extent2D {
                width: 1920,
                height: 1080,
            },
            8,
            reset,
        )
    }

    #[test]
    fn modes_parse_and_are_feature_independent() {
        assert_eq!(UpscalerMode::default(), UpscalerMode::Native);
        assert!(UpscalerMode::Native.is_native());
        assert!(UpscalerMode::Dlaa.uses_streamline());
        assert_eq!(UpscalerMode::DlssBalanced.dlss_mode(), Some(3));
        assert_eq!(UpscalerMode::Native.dlss_mode(), None);
    }

    #[test]
    fn mode_cycle_is_deterministic_and_wraps_without_gpu_state() {
        let mut mode = UpscalerMode::Native;
        let expected = [
            UpscalerMode::Dlaa,
            UpscalerMode::DlssQuality,
            UpscalerMode::DlssBalanced,
            UpscalerMode::DlssPerformance,
            UpscalerMode::Native,
        ];
        for next in expected {
            mode = mode.next_mode();
            assert_eq!(mode, next);
        }
    }

    #[test]
    fn optimal_settings_are_the_only_dlss_extent_source() {
        let output = Extent2D {
            width: 1920,
            height: 1080,
        };
        assert_eq!(
            render_extent_from_optimal(
                UpscalerMode::DlssQuality,
                output,
                DlssOptimalSettings {
                    render_width: 1280,
                    render_height: 720,
                },
            ),
            Ok(Extent2D {
                width: 1280,
                height: 720,
            })
        );
        assert_eq!(
            render_extent_from_optimal(
                UpscalerMode::Dlaa,
                output,
                DlssOptimalSettings {
                    render_width: 1280,
                    render_height: 720,
                },
            ),
            Err(DlssExtentError::DlaaMustMatchOutput)
        );
        assert_eq!(
            render_extent_from_optimal(
                UpscalerMode::DlssQuality,
                output,
                DlssOptimalSettings {
                    render_width: 0,
                    render_height: 720,
                },
            ),
            Err(DlssExtentError::Zero)
        );
    }

    #[test]
    fn contract_uses_row_major_non_jittered_matrices_and_render_scale() {
        let state = input(camera(), camera(), true);
        assert_eq!(state.contract_version, DLSS_CONTRACT_VERSION);
        assert_eq!(state.reset, 1);
        assert_eq!(state.motion_vectors_jittered, 0);
        assert_eq!(state.depth_inverted, 0);
        assert_eq!(state.output_width, 1920);
        assert_eq!(state.output_height, 1080);
        assert!((state.motion_vector_scale[0] - 1.0 / 1280.0).abs() < 1.0e-7);
        assert!((state.motion_vector_scale[1] - 1.0 / 720.0).abs() < 1.0e-7);
        assert_ne!(state.jitter_px, [0.0; 2]);
        assert_ne!(state.jitter_px, state.jitter_prev_px);
        assert_eq!(state.view_to_clip[1], 0.0);
    }

    #[test]
    fn static_camera_and_three_static_points_have_zero_motion() {
        let state = input(camera(), camera(), false);
        for point in [[-0.5, 0.0, 0.5], [0.0, 0.2, 1.0], [0.4, -0.1, 2.0]] {
            let motion = dlss_motion_for_world_point(point, point, &state).unwrap();
            assert!(motion.iter().all(|value| value.abs() < 1.0e-5));
        }
    }

    #[test]
    fn rigid_object_translation_is_motion_not_global_reset() {
        let state = input(camera(), camera(), false);
        let motion = dlss_motion_for_world_point([0.2, 0.0, 1.0], [0.0, 0.0, 1.0], &state).unwrap();
        assert!(motion[0].abs() > 0.1);
        assert_eq!(state.reset, 0);
    }

    #[test]
    fn camera_translation_and_rotation_produce_analytical_motion() {
        let current = CameraPose {
            position: [0.1, 0.0, -2.0],
            yaw: 0.04,
            pitch: 0.02,
        };
        let state = input(current, camera(), false);
        let motion = dlss_motion_for_world_point([0.0, 0.0, 1.0], [0.0, 0.0, 1.0], &state).unwrap();
        assert!(motion.iter().any(|value| value.abs() > 0.01));
        assert_eq!(
            dlss_motion_pixels([5.0, 7.0], [8.0, 3.0], false),
            [3.0, -4.0]
        );
        assert_eq!(dlss_motion_pixels([5.0, 7.0], [8.0, 3.0], true), [0.0, 0.0]);
    }

    #[test]
    fn reset_and_extent_discontinuity_zero_motion() {
        let state = input(camera(), camera(), true);
        let motion = dlss_motion_for_world_point([0.2, 0.0, 1.0], [0.0, 0.0, 1.0], &state).unwrap();
        assert_eq!(motion, [0.0; 2]);
        let reset = state.reset_for_extent(
            Extent2D {
                width: 960,
                height: 540,
            },
            Extent2D {
                width: 1920,
                height: 1080,
            },
        );
        assert_eq!(reset.reset, 1);
        assert_eq!(reset.previous_render_width, 960);
        assert_eq!(reset.motion_vector_scale, [1.0 / 960.0, 1.0 / 540.0]);
    }

    #[test]
    fn halton_jitter_is_deterministic_and_separate_from_matrices() {
        let jitter = projection_jitter(0);
        assert!(jitter[0].abs() < 1.0e-7);
        assert!((jitter[1] + 1.0 / 6.0).abs() < 1.0e-6);
        assert_eq!(projection_jitter(0), projection_jitter(0));
        assert_ne!(projection_jitter(0), projection_jitter(1));
    }
}
