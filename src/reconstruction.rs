//! Stable reconstruction input and backend contracts shared by SVGF, NRD and
//! future reconstruction integrations.

use std::fmt;

pub const RECONSTRUCTION_CONTRACT_VERSION: u32 = 1;
pub const NRD_BRIDGE_ABI_VERSION: u32 = 1;
pub const NRD_VERSION: &str = "4.17.3";
pub const NRD_COMMIT_PREFIX: &str = "792eff1";
pub const NRD_NRI_VERSION: &str = "v179";
pub const NRD_MATHLIB_VERSION: &str = "v11";
pub const NRD_SHADERMAKE_COMMIT: &str = "18f5a344e7ca8fa65daaf079d07bc8ce38453e05";
pub const RECONSTRUCTION_FRAMES_IN_FLIGHT: u32 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DenoiserBackend {
    #[default]
    Svgf,
    NrdReblur,
}

impl DenoiserBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Svgf => "svgf",
            Self::NrdReblur => "nrd-reblur",
        }
    }

    pub const fn nrd_compiled(self) -> bool {
        cfg!(feature = "nrd")
    }

    pub fn requested_startup_error(self) -> Option<String> {
        match self {
            Self::Svgf => None,
            Self::NrdReblur if !cfg!(feature = "nrd") => Some(
                "--denoiser nrd-reblur 需要使用 `cargo run --features nrd --` 重新构建；当前构建未包含 NRD"
                    .to_string(),
            ),
            Self::NrdReblur => Some(
                "--denoiser nrd-reblur 当前仅完成可选构建契约，NRD 后端尚未接入；请等待阶段 9C/9D"
                    .to_string(),
            ),
        }
    }
}

impl fmt::Display for DenoiserBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Non-jittered camera and frame state passed to every reconstruction adapter.
/// Matrices are column-major `glam::Mat4::to_cols_array()` values. The C ABI
/// bridge receives this exact Rust layout and performs no camera reconstruction.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReconstructionFrameState {
    pub world_to_view: [f32; 16],
    pub world_to_view_prev: [f32; 16],
    pub view_to_clip: [f32; 16],
    pub view_to_clip_prev: [f32; 16],
    pub camera_position: [f32; 3],
    pub frame_index: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub previous_render_width: u32,
    pub previous_render_height: u32,
    pub camera_jitter_px: [f32; 2],
    pub camera_jitter_prev_px: [f32; 2],
    pub reset: u32,
    pub delta_time_ms: f32,
}

impl Default for ReconstructionFrameState {
    fn default() -> Self {
        Self {
            world_to_view: glam::Mat4::IDENTITY.to_cols_array(),
            world_to_view_prev: glam::Mat4::IDENTITY.to_cols_array(),
            view_to_clip: glam::Mat4::IDENTITY.to_cols_array(),
            view_to_clip_prev: glam::Mat4::IDENTITY.to_cols_array(),
            camera_position: [0.0; 3],
            frame_index: 0,
            render_width: 1,
            render_height: 1,
            previous_render_width: 1,
            previous_render_height: 1,
            camera_jitter_px: [0.0; 2],
            camera_jitter_prev_px: [0.0; 2],
            reset: 1,
            delta_time_ms: 0.0,
        }
    }
}

impl ReconstructionFrameState {
    pub fn reset_for_extent(mut self, render_width: u32, render_height: u32) -> Self {
        self.render_width = render_width.max(1);
        self.render_height = render_height.max(1);
        self.previous_render_width = self.render_width;
        self.previous_render_height = self.render_height;
        self.reset = 1;
        self.camera_jitter_px = [0.0; 2];
        self.camera_jitter_prev_px = [0.0; 2];
        self
    }
}

/// Fixed POD create description mirrored by `native/nrd_bridge/include`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NrdBridgeCreateDesc {
    pub abi_version: u32,
    pub resource_width: u32,
    pub resource_height: u32,
    pub queued_frames: u32,
    pub device: usize,
}

#[repr(C)]
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NrdBridgeVersion {
    pub abi_version: u32,
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    pub normal_encoding: u32,
    pub roughness_encoding: u32,
    pub commit: [u8; 41],
}

impl Default for NrdBridgeVersion {
    fn default() -> Self {
        Self {
            abi_version: 0,
            major: 0,
            minor: 0,
            build: 0,
            normal_encoding: 0,
            roughness_encoding: 0,
            commit: [0; 41],
        }
    }
}

pub const fn nrd_bridge_create_desc_is_valid(desc: NrdBridgeCreateDesc) -> bool {
    desc.abi_version == NRD_BRIDGE_ABI_VERSION
        && desc.resource_width > 0
        && desc.resource_height > 0
        && desc.queued_frames == RECONSTRUCTION_FRAMES_IN_FLIGHT
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};

    use super::*;

    #[test]
    fn backend_names_and_default_are_stable() {
        assert_eq!(DenoiserBackend::default(), DenoiserBackend::Svgf);
        assert_eq!(DenoiserBackend::Svgf.as_str(), "svgf");
        assert_eq!(DenoiserBackend::NrdReblur.as_str(), "nrd-reblur");
        assert_eq!(DenoiserBackend::Svgf.nrd_compiled(), cfg!(feature = "nrd"));
    }

    #[cfg(not(feature = "nrd"))]
    #[test]
    fn nrd_request_explains_the_required_feature_when_uncompiled() {
        let error = DenoiserBackend::NrdReblur
            .requested_startup_error()
            .expect("feature-off NRD request must fail");
        assert!(error.contains("--features nrd"));
        assert!(error.contains("未包含 NRD"));
    }

    #[test]
    fn contract_constants_are_locked() {
        assert_eq!(NRD_VERSION, "4.17.3");
        assert!(NRD_COMMIT_PREFIX.starts_with("792eff1"));
        assert_eq!(NRD_NRI_VERSION, "v179");
        assert_eq!(NRD_MATHLIB_VERSION, "v11");
        assert_eq!(NRD_SHADERMAKE_COMMIT.len(), 40);
        assert_eq!(RECONSTRUCTION_FRAMES_IN_FLIGHT, 3);
    }

    #[test]
    fn frame_state_is_c_compatible_and_reset_is_explicit() {
        assert_eq!(align_of::<ReconstructionFrameState>(), 4);
        assert_eq!(offset_of!(ReconstructionFrameState, frame_index), 268);
        assert_eq!(offset_of!(ReconstructionFrameState, reset), 304);
        assert_eq!(size_of::<ReconstructionFrameState>(), 312);

        let reset = ReconstructionFrameState::default().reset_for_extent(1280, 720);
        assert_eq!((reset.render_width, reset.render_height), (1280, 720));
        assert_eq!(
            (reset.previous_render_width, reset.previous_render_height),
            (1280, 720)
        );
        assert_eq!(reset.reset, 1);
        assert_eq!(reset.camera_jitter_px, [0.0; 2]);
    }

    #[test]
    fn bridge_create_desc_requires_three_queued_frames() {
        assert_eq!(align_of::<NrdBridgeCreateDesc>(), 8);
        assert_eq!(offset_of!(NrdBridgeCreateDesc, device), 16);
        assert_eq!(size_of::<NrdBridgeCreateDesc>(), 24);
        assert_eq!(align_of::<NrdBridgeVersion>(), 4);
        assert_eq!(size_of::<NrdBridgeVersion>(), 68);

        let valid = NrdBridgeCreateDesc {
            abi_version: NRD_BRIDGE_ABI_VERSION,
            resource_width: 1280,
            resource_height: 720,
            queued_frames: RECONSTRUCTION_FRAMES_IN_FLIGHT,
            device: 1,
        };
        assert!(nrd_bridge_create_desc_is_valid(valid));
        assert!(!nrd_bridge_create_desc_is_valid(NrdBridgeCreateDesc {
            queued_frames: 2,
            ..valid
        }));
        assert!(!nrd_bridge_create_desc_is_valid(NrdBridgeCreateDesc {
            abi_version: 0,
            ..valid
        }));
    }
}
