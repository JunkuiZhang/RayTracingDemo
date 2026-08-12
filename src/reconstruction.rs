//! Stable reconstruction input and backend contracts shared by SVGF, NRD and
//! future reconstruction integrations.

#[cfg(feature = "nrd")]
use std::ffi::c_void;
use std::fmt;

pub const RECONSTRUCTION_CONTRACT_VERSION: u32 = 1;
pub const NRD_BRIDGE_ABI_VERSION: u32 = 1;
pub const NRD_BRIDGE_STATUS_OK: u32 = 0;
pub const NRD_BRIDGE_STATUS_INVALID_ARGUMENT: u32 = 1;
pub const NRD_VERSION: &str = "4.17.3";
pub const NRD_COMMIT_PREFIX: &str = "792eff1";
pub const NRD_NRI_VERSION: &str = "v179";
pub const NRD_MATHLIB_VERSION: &str = "v11";
pub const NRD_SHADERMAKE_COMMIT: &str = "18f5a344e7ca8fa65daaf079d07bc8ce38453e05";
pub const RECONSTRUCTION_FRAMES_IN_FLIGHT: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraPose {
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReconstructionFrameInput {
    pub current_camera: CameraPose,
    pub previous_camera: CameraPose,
    pub render_extent: [u32; 2],
    pub previous_render_extent: [u32; 2],
    pub frame_index: u32,
    pub reset: bool,
    pub delta_time_ms: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DenoiserBackend {
    #[default]
    Svgf,
    NrdReblur,
    DlssRayReconstruction,
}

impl DenoiserBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Svgf => "svgf",
            Self::NrdReblur => "nrd-reblur",
            Self::DlssRayReconstruction => "dlss-rr",
        }
    }

    pub const fn nrd_compiled(self) -> bool {
        // This reports build capability, not the active path. RR keeps NRD
        // inactive at runtime even when the optional NRD bridge is compiled.
        cfg!(feature = "nrd")
    }

    pub fn requested_startup_error(self) -> Option<String> {
        match self {
            Self::Svgf => None,
            Self::NrdReblur => (!cfg!(feature = "nrd")).then(|| {
                "--denoiser nrd-reblur 需要使用 `cargo run --features nrd --` 重新构建；当前构建未包含 NRD"
                    .to_string()
            }),
            Self::DlssRayReconstruction => (!cfg!(feature = "streamline-rr")).then(|| {
                "--denoiser dlss-rr 需要使用 `cargo run --features streamline-rr --` 重新构建；当前构建未包含 DLSS Ray Reconstruction"
                    .to_string()
            }),
        }
    }
}

/// The reconstruction path is a strategy, not a post-process toggle. RR is a
/// fused noisy-HDR denoiser/upscaler and therefore cannot share the SVGF/NRD
/// intermediate path or be chained into DLSS SR.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReconstructionPath {
    #[default]
    Svgf,
    NrdReblur,
    DlssRayReconstruction,
}

impl ReconstructionPath {
    pub const fn from_backend(backend: DenoiserBackend) -> Self {
        match backend {
            DenoiserBackend::Svgf => Self::Svgf,
            DenoiserBackend::NrdReblur => Self::NrdReblur,
            DenoiserBackend::DlssRayReconstruction => Self::DlssRayReconstruction,
        }
    }

    pub const fn is_svgf(self) -> bool {
        matches!(self, Self::Svgf)
    }

    pub const fn is_nrd(self) -> bool {
        matches!(self, Self::NrdReblur)
    }

    pub const fn is_rr(self) -> bool {
        matches!(self, Self::DlssRayReconstruction)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Svgf => "svgf",
            Self::NrdReblur => "nrd-reblur",
            Self::DlssRayReconstruction => "dlss-rr",
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
    pub fn from_camera(input: ReconstructionFrameInput) -> Self {
        let [render_width, render_height] = input.render_extent;
        let [previous_render_width, previous_render_height] = input.previous_render_extent;
        let aspect = render_width.max(1) as f32 / render_height.max(1) as f32;
        let (world_to_view, view_to_clip) = camera_matrices(input.current_camera, aspect);
        let (world_to_view_prev, view_to_clip_prev) = camera_matrices(
            input.previous_camera,
            previous_render_width.max(1) as f32 / previous_render_height.max(1) as f32,
        );
        Self {
            world_to_view,
            world_to_view_prev,
            view_to_clip,
            view_to_clip_prev,
            camera_position: input.current_camera.position,
            frame_index: input.frame_index,
            render_width: render_width.max(1),
            render_height: render_height.max(1),
            previous_render_width: previous_render_width.max(1),
            previous_render_height: previous_render_height.max(1),
            camera_jitter_px: [0.0; 2],
            camera_jitter_prev_px: [0.0; 2],
            reset: u32::from(input.reset),
            delta_time_ms: input.delta_time_ms.clamp(0.0, 1_000.0),
        }
    }

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

/// Build the non-jittered camera matrices used by all reconstruction adapters.
/// View space is right-handed in the renderer's convention: +Z points forward,
/// matching the ray-generation camera basis and positive linear viewZ.
pub fn camera_matrices(camera: CameraPose, aspect: f32) -> ([f32; 16], [f32; 16]) {
    let forward = glam::Vec3::new(
        camera.yaw.sin() * camera.pitch.cos(),
        camera.pitch.sin(),
        camera.yaw.cos() * camera.pitch.cos(),
    )
    .normalize();
    let right = glam::Vec3::Y.cross(forward).normalize();
    let up = forward.cross(right).normalize();
    let position = glam::Vec3::from_array(camera.position);
    let view = glam::Mat4::from_cols(
        glam::Vec4::new(right.x, up.x, forward.x, 0.0),
        glam::Vec4::new(right.y, up.y, forward.y, 0.0),
        glam::Vec4::new(right.z, up.z, forward.z, 0.0),
        glam::Vec4::new(
            -right.dot(position),
            -up.dot(position),
            -forward.dot(position),
            1.0,
        ),
    );
    let focal_length = 1.0 / (40.0_f32.to_radians() * 0.5).tan();
    let near = 0.001;
    let far = 1_000.0;
    let projection = glam::Mat4::from_cols(
        glam::Vec4::new(focal_length / aspect.max(1.0e-6), 0.0, 0.0, 0.0),
        glam::Vec4::new(0.0, focal_length, 0.0, 0.0),
        glam::Vec4::new(0.0, 0.0, far / (far - near), 1.0),
        glam::Vec4::new(0.0, 0.0, -near * far / (far - near), 0.0),
    );
    (view.to_cols_array(), projection.to_cols_array())
}

pub fn project_world_to_uv(
    world_position: [f32; 3],
    world_to_view: [f32; 16],
    view_to_clip: [f32; 16],
) -> Option<([f32; 2], f32)> {
    let view = glam::Mat4::from_cols_array(&world_to_view)
        * glam::Vec4::from((glam::Vec3::from_array(world_position), 1.0));
    let clip = glam::Mat4::from_cols_array(&view_to_clip) * view;
    if !clip.w.is_finite() || clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    let uv = [ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5];
    if uv.iter().all(|value| value.is_finite()) && view.z.is_finite() && view.z > 0.0 {
        Some((uv, view.z))
    } else {
        None
    }
}

pub fn nrd_motion_pixels(
    current_uv: [f32; 2],
    previous_uv: [f32; 2],
    width: u32,
    height: u32,
    reset: bool,
) -> [f32; 2] {
    if reset {
        return [0.0; 2];
    }
    [
        (previous_uv[0] - current_uv[0]) * width.max(1) as f32,
        (previous_uv[1] - current_uv[1]) * height.max(1) as f32,
    ]
}

pub fn nrd_view_z_motion(view_z: f32, previous_view_z: f32, reset: bool) -> f32 {
    if reset { 0.0 } else { previous_view_z - view_z }
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

#[cfg(feature = "nrd")]
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NrdBridgeResource {
    pub resource: usize,
    pub state: u32,
    pub format: u32,
}

#[cfg(feature = "nrd")]
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NrdBridgeResources {
    pub motion: NrdBridgeResource,
    pub normal_roughness: NrdBridgeResource,
    pub view_z: NrdBridgeResource,
    pub diffuse_radiance_hit_distance: NrdBridgeResource,
    pub specular_radiance_hit_distance: NrdBridgeResource,
    pub diffuse_output: NrdBridgeResource,
    pub specular_output: NrdBridgeResource,
    pub validation_output: NrdBridgeResource,
}

#[cfg(feature = "nrd")]
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NrdBridgeFrameDesc {
    pub state: *const ReconstructionFrameState,
    pub enable_validation: u32,
}

#[cfg(feature = "nrd")]
#[repr(C)]
pub struct NrdBridgeOpaque {
    _private: [u8; 0],
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

#[cfg(feature = "nrd")]
unsafe extern "C" {
    fn nrd_bridge_query_version(out_version: *mut NrdBridgeVersion) -> u32;
    pub fn nrd_bridge_create(
        desc: *const NrdBridgeCreateDesc,
        out_bridge: *mut *mut NrdBridgeOpaque,
    ) -> u32;
    pub fn nrd_bridge_denoise(
        bridge: *mut NrdBridgeOpaque,
        frame: *const NrdBridgeFrameDesc,
        resources: *mut NrdBridgeResources,
        command_list: *mut c_void,
    ) -> u32;
    pub fn nrd_bridge_destroy(bridge: *mut NrdBridgeOpaque);
    pub fn nrd_bridge_copy_last_error(
        bridge: *const NrdBridgeOpaque,
        destination: *mut u8,
        capacity: usize,
    ) -> usize;
}

#[cfg(feature = "nrd")]
pub fn query_nrd_version() -> Result<NrdBridgeVersion, String> {
    let mut version = NrdBridgeVersion::default();
    let status = unsafe { nrd_bridge_query_version(&mut version) };
    if status != 0 {
        return Err(format!(
            "NRD bridge version query failed with status {status}"
        ));
    }
    if version.abi_version != NRD_BRIDGE_ABI_VERSION
        || version.major != 4
        || version.minor != 17
        || version.build != 3
        || version.normal_encoding != 2
        || version.roughness_encoding != 1
    {
        return Err("NRD bridge reported an incompatible version or encoding".to_string());
    }
    Ok(version)
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
        assert_eq!(DenoiserBackend::DlssRayReconstruction.as_str(), "dlss-rr");
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
    fn reconstruction_strategy_keeps_rr_fused_and_nrd_independent() {
        assert!(ReconstructionPath::from_backend(DenoiserBackend::Svgf).is_svgf());
        assert!(ReconstructionPath::from_backend(DenoiserBackend::NrdReblur).is_nrd());
        assert!(ReconstructionPath::from_backend(DenoiserBackend::DlssRayReconstruction).is_rr());
        assert_eq!(
            DenoiserBackend::DlssRayReconstruction.nrd_compiled(),
            cfg!(feature = "nrd")
        );
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

    #[cfg(feature = "nrd")]
    #[test]
    fn bridge_resource_layout_is_c_compatible() {
        assert_eq!(align_of::<NrdBridgeResource>(), 8);
        assert_eq!(size_of::<NrdBridgeResource>(), 16);
        assert_eq!(size_of::<NrdBridgeResources>(), 128);
        assert_eq!(align_of::<NrdBridgeFrameDesc>(), 8);
        assert_eq!(offset_of!(NrdBridgeFrameDesc, enable_validation), 8);
        assert_eq!(size_of::<NrdBridgeFrameDesc>(), 16);
    }

    #[cfg(feature = "nrd")]
    #[test]
    fn feature_build_queries_the_locked_nrd_library() {
        let version = query_nrd_version().expect("NRD bridge must report its locked version");
        assert_eq!((version.major, version.minor, version.build), (4, 17, 3));
        assert_eq!(version.normal_encoding, 2);
        assert_eq!(version.roughness_encoding, 1);
        let commit = std::str::from_utf8(&version.commit[..40]).expect("NRD commit is ASCII");
        assert!(commit.starts_with(NRD_COMMIT_PREFIX));
    }

    #[test]
    fn camera_projection_and_view_z_follow_the_renderer_convention() {
        let (view, projection) = camera_matrices(
            CameraPose {
                position: [0.0, 0.0, 0.0],
                yaw: 0.0,
                pitch: 0.0,
            },
            16.0 / 9.0,
        );
        let (uv, view_z) = project_world_to_uv([0.0, 0.0, 1.0], view, projection).unwrap();
        assert!((uv[0] - 0.5).abs() < 1.0e-6);
        assert!((uv[1] - 0.5).abs() < 1.0e-6);
        assert!((view_z - 1.0).abs() < 1.0e-6);

        let (translated_view, translated_projection) = camera_matrices(
            CameraPose {
                position: [1.0, 0.0, 0.0],
                yaw: 0.0,
                pitch: 0.0,
            },
            16.0 / 9.0,
        );
        let (translated_uv, translated_z) =
            project_world_to_uv([1.0, 0.0, 1.0], translated_view, translated_projection).unwrap();
        assert!((translated_uv[0] - 0.5).abs() < 1.0e-6);
        assert!((translated_z - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn nrd_motion_has_previous_equals_current_plus_motion_direction() {
        let current = [0.4, 0.5];
        let previous = [0.5, 0.25];
        let motion = nrd_motion_pixels(current, previous, 100, 200, false);
        assert!((current[0] + motion[0] / 100.0 - previous[0]).abs() < 1.0e-6);
        assert!((current[1] + motion[1] / 200.0 - previous[1]).abs() < 1.0e-6);
        assert_eq!(
            nrd_motion_pixels(current, previous, 100, 200, true),
            [0.0; 2]
        );
        assert_eq!(nrd_view_z_motion(3.0, 4.0, false), 1.0);
        assert_eq!(nrd_view_z_motion(3.0, 4.0, true), 0.0);
    }

    #[test]
    fn static_subpixel_surface_has_zero_2_5d_motion() {
        let (view, projection) = camera_matrices(
            CameraPose {
                position: [0.0, 0.0, -2.0],
                yaw: 0.0,
                pitch: 0.0,
            },
            16.0 / 9.0,
        );
        let (current_uv, current_view_z) =
            project_world_to_uv([0.137, -0.083, 1.25], view, projection).unwrap();
        let (previous_uv, previous_view_z) =
            project_world_to_uv([0.137, -0.083, 1.25], view, projection).unwrap();
        assert_eq!(
            nrd_motion_pixels(current_uv, previous_uv, 1280, 720, false),
            [0.0; 2]
        );
        assert_eq!(
            nrd_view_z_motion(current_view_z, previous_view_z, false),
            0.0
        );
    }

    #[test]
    fn frame_snapshot_reset_makes_previous_extent_and_jitter_explicit() {
        let state = ReconstructionFrameState::from_camera(ReconstructionFrameInput {
            current_camera: CameraPose {
                position: [0.0, 0.0, 0.0],
                yaw: 0.2,
                pitch: -0.1,
            },
            previous_camera: CameraPose {
                position: [0.5, 0.0, 0.0],
                yaw: 0.1,
                pitch: -0.1,
            },
            render_extent: [1280, 720],
            previous_render_extent: [960, 540],
            frame_index: 7,
            reset: true,
            delta_time_ms: 16.0,
        });
        assert_eq!(state.frame_index, 7);
        assert_eq!((state.render_width, state.render_height), (1280, 720));
        assert_eq!(
            (state.previous_render_width, state.previous_render_height),
            (960, 540)
        );
        assert_eq!(state.camera_jitter_px, [0.0; 2]);
        assert_eq!(state.reset, 1);
        assert_eq!(state.delta_time_ms, 16.0);
    }
}
