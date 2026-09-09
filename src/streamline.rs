//! Streamline C ABI and the small Rust-side lifetime wrapper.

use std::{ffi::c_void, ptr::NonNull};

pub const SDK_VERSION: &str = "2.14.1";
pub const ABI_VERSION: u32 = 9;
pub const STATUS_OK: u32 = 0;
pub const STATUS_INVALID_ARGUMENT: u32 = 1;
pub const STATUS_SDK_ERROR: u32 = 2;
pub const STATUS_EXCEPTION: u32 = 3;
pub const STATUS_NOT_INITIALIZED: u32 = 4;
pub const STATUS_UNSUPPORTED: u32 = 5;
pub const STATUS_ALREADY_UPGRADED: u32 = 6;
pub const FRAME_GENERATION_OFF: u32 = 0;
pub const FRAME_GENERATION_ON: u32 = 1;
pub const RESOURCE_LIFECYCLE_VALID_UNTIL_PRESENT: u32 = 1;
pub const RESOURCE_LIFECYCLE_ONLY_VALID_NOW: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct InitDesc {
    pub struct_size: u32,
    pub abi_version: u32,
    pub development: u32,
    pub enable_dlss: u32,
    pub application_id: u32,
    pub enable_dlss_rr: u32,
    pub enable_dlss_fg: u32,
    pub plugin_path: *const u16,
    pub log_path: *const u16,
    pub project_id: *const i8,
    pub engine_version: *const i8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Support {
    pub struct_size: u32,
    pub abi_version: u32,
    pub dlss_supported: u32,
    pub reflex_supported: u32,
    pub pcl_supported: u32,
    pub rr_supported: u32,
    pub fg_supported: u32,
    pub dlss_result: u32,
    pub reflex_result: u32,
    pub pcl_result: u32,
    pub rr_result: u32,
    pub fg_result: u32,
    pub adapter_luid: u64,
    pub sdk_version: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct OptimalSettings {
    pub struct_size: u32,
    pub abi_version: u32,
    pub optimal_render_width: u32,
    pub optimal_render_height: u32,
    pub render_width_min: u32,
    pub render_height_min: u32,
    pub render_width_max: u32,
    pub render_height_max: u32,
    pub optimal_sharpness: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameToken {
    pub struct_size: u32,
    pub abi_version: u32,
    pub token: *mut c_void,
    pub frame_index: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Viewport {
    pub struct_size: u32,
    pub abi_version: u32,
    pub id: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DlssOptions {
    pub struct_size: u32,
    pub abi_version: u32,
    pub mode: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub sharpness: f32,
    pub pre_exposure: f32,
    pub exposure_scale: f32,
    pub color_buffers_hdr: u32,
    pub use_auto_exposure: u32,
    pub alpha_upscaling_enabled: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RrOptions {
    pub struct_size: u32,
    pub abi_version: u32,
    pub mode: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub sharpness: f32,
    pub pre_exposure: f32,
    pub exposure_scale: f32,
    pub color_buffers_hdr: u32,
    pub indicator_invert_axis_x: u32,
    pub indicator_invert_axis_y: u32,
    pub normal_roughness_mode: u32,
    pub world_to_camera_view: [f32; 16],
    pub camera_view_to_world: [f32; 16],
    pub alpha_upscaling_enabled: u32,
    pub dlaa_preset: u32,
    pub quality_preset: u32,
    pub balanced_preset: u32,
    pub performance_preset: u32,
    pub ultra_performance_preset: u32,
    pub ultra_quality_preset: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RrOptimalSettings {
    pub struct_size: u32,
    pub abi_version: u32,
    pub optimal_render_width: u32,
    pub optimal_render_height: u32,
    pub render_width_min: u32,
    pub render_height_min: u32,
    pub render_width_max: u32,
    pub render_height_max: u32,
    pub optimal_sharpness: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RrState {
    pub struct_size: u32,
    pub abi_version: u32,
    pub estimated_vram_usage_bytes: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameGenerationOptions {
    pub struct_size: u32,
    pub abi_version: u32,
    pub mode: u32,
    pub num_frames_to_generate: u32,
    pub flags: u32,
    pub num_back_buffers: u32,
    pub mvec_depth_width: u32,
    pub mvec_depth_height: u32,
    pub color_width: u32,
    pub color_height: u32,
    pub color_buffer_format: u32,
    pub mvec_buffer_format: u32,
    pub depth_buffer_format: u32,
    pub hud_less_buffer_format: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameGenerationState {
    pub struct_size: u32,
    pub abi_version: u32,
    pub status_raw: u32,
    pub min_width_or_height: u32,
    pub num_frames_actually_presented: u32,
    pub num_frames_to_generate_max: u32,
    pub estimated_vram_usage_bytes: u64,
    pub vsync_support_available: u32,
    pub dynamic_mfg_supported: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Constants {
    pub struct_size: u32,
    pub abi_version: u32,
    pub camera_view_to_clip: [f32; 16],
    pub clip_to_camera_view: [f32; 16],
    pub clip_to_prev_clip: [f32; 16],
    pub prev_clip_to_clip: [f32; 16],
    pub jitter_offset: [f32; 2],
    pub mvec_scale: [f32; 2],
    pub camera_pinhole_offset: [f32; 2],
    pub camera_position: [f32; 3],
    pub camera_up: [f32; 3],
    pub camera_right: [f32; 3],
    pub camera_forward: [f32; 3],
    pub camera_near: f32,
    pub camera_far: f32,
    pub camera_fov: f32,
    pub camera_aspect_ratio: f32,
    pub depth_inverted: u32,
    pub camera_motion_included: u32,
    pub motion_vectors_3d: u32,
    pub reset: u32,
    pub motion_vectors_jittered: u32,
    pub min_relative_linear_depth_object_separation: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ResourceTag {
    pub struct_size: u32,
    pub abi_version: u32,
    pub resource: *mut c_void,
    pub state: u32,
    pub buffer_type: u32,
    pub lifecycle: u32,
    pub top: u32,
    pub left: u32,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ReflexState {
    pub struct_size: u32,
    pub abi_version: u32,
    pub low_latency_available: u32,
    pub latency_report_available: u32,
    pub flash_indicator_driver_controlled: u32,
}

#[repr(C)]
pub struct RawBridge {
    _private: [u8; 0],
}

unsafe extern "C" {
    pub fn streamline_bridge_create(desc: *const InitDesc, out: *mut *mut RawBridge) -> u32;
    pub fn streamline_bridge_set_d3d_device(
        bridge: *mut RawBridge,
        d3d_device: *mut c_void,
        adapter_luid: *const u8,
        adapter_luid_size: usize,
    ) -> u32;
    pub fn streamline_bridge_query_support(bridge: *mut RawBridge, out: *mut Support) -> u32;
    pub fn streamline_bridge_get_optimal_settings(
        bridge: *mut RawBridge,
        options: *const DlssOptions,
        out: *mut OptimalSettings,
    ) -> u32;
    pub fn streamline_bridge_dlss_set_options(
        bridge: *mut RawBridge,
        viewport: *const Viewport,
        options: *const DlssOptions,
    ) -> u32;
    pub fn streamline_bridge_rr_get_optimal_settings(
        bridge: *mut RawBridge,
        options: *const RrOptions,
        out: *mut RrOptimalSettings,
    ) -> u32;
    pub fn streamline_bridge_rr_set_options(
        bridge: *mut RawBridge,
        viewport: *const Viewport,
        options: *const RrOptions,
    ) -> u32;
    pub fn streamline_bridge_rr_get_state(
        bridge: *mut RawBridge,
        viewport: *const Viewport,
        out: *mut RrState,
    ) -> u32;
    pub fn streamline_bridge_allocate_resources(
        bridge: *mut RawBridge,
        viewport: *const Viewport,
        command_list: *mut c_void,
    ) -> u32;
    pub fn streamline_bridge_free_resources(
        bridge: *mut RawBridge,
        viewport: *const Viewport,
    ) -> u32;
    pub fn streamline_bridge_rr_free_resources(
        bridge: *mut RawBridge,
        viewport: *const Viewport,
    ) -> u32;
    pub fn streamline_bridge_fg_set_options(
        bridge: *mut RawBridge,
        viewport: *const Viewport,
        options: *const FrameGenerationOptions,
    ) -> u32;
    pub fn streamline_bridge_fg_get_state(
        bridge: *mut RawBridge,
        viewport: *const Viewport,
        estimate_options: *const FrameGenerationOptions,
        out: *mut FrameGenerationState,
    ) -> u32;
    pub fn streamline_bridge_fg_set_loaded(bridge: *mut RawBridge, loaded: u32) -> u32;
    pub fn streamline_bridge_fg_is_loaded(bridge: *mut RawBridge, out_loaded: *mut u32) -> u32;
    pub fn streamline_bridge_get_frame_token(
        bridge: *mut RawBridge,
        frame_index: u32,
        out: *mut FrameToken,
    ) -> u32;
    pub fn streamline_bridge_set_constants(
        bridge: *mut RawBridge,
        token: *const FrameToken,
        viewport: *const Viewport,
        constants: *const Constants,
    ) -> u32;
    pub fn streamline_bridge_set_tags(
        bridge: *mut RawBridge,
        token: *const FrameToken,
        viewport: *const Viewport,
        tags: *const ResourceTag,
        tag_count: u32,
        command_list: *mut c_void,
    ) -> u32;
    pub fn streamline_bridge_evaluate_dlss(
        bridge: *mut RawBridge,
        token: *const FrameToken,
        viewport: *const Viewport,
        command_list: *mut c_void,
    ) -> u32;
    pub fn streamline_bridge_evaluate_rr(
        bridge: *mut RawBridge,
        token: *const FrameToken,
        viewport: *const Viewport,
        command_list: *mut c_void,
    ) -> u32;
    pub fn streamline_bridge_reflex_set_mode(bridge: *mut RawBridge, mode: u32) -> u32;
    pub fn streamline_bridge_reflex_sleep(bridge: *mut RawBridge, token: *const FrameToken) -> u32;
    pub fn streamline_bridge_pcl_marker(
        bridge: *mut RawBridge,
        token: *const FrameToken,
        marker: u32,
    ) -> u32;
    pub fn streamline_bridge_reflex_get_state(bridge: *mut RawBridge, out: *mut ReflexState)
    -> u32;
    pub fn streamline_bridge_upgrade_interface(
        bridge: *mut RawBridge,
        interface_ptr: *mut *mut c_void,
    ) -> u32;
    pub fn streamline_bridge_get_native_interface(
        bridge: *mut RawBridge,
        proxy_interface: *mut c_void,
        out_native_interface: *mut *mut c_void,
    ) -> u32;
    pub fn streamline_bridge_shutdown(bridge: *mut RawBridge) -> u32;
    pub fn streamline_bridge_copy_last_error(
        bridge: *const RawBridge,
        destination: *mut i8,
        capacity: usize,
    ) -> usize;
}

pub struct Bridge {
    raw: NonNull<RawBridge>,
}

impl Bridge {
    pub fn create(desc: &InitDesc) -> Result<Self, u32> {
        let mut raw = std::ptr::null_mut();
        let status = unsafe { streamline_bridge_create(desc, &mut raw) };
        NonNull::new(raw)
            .map(Self::from_raw)
            .ok_or(if status == STATUS_OK {
                STATUS_INVALID_ARGUMENT
            } else {
                status
            })
    }

    pub fn from_raw(raw: NonNull<RawBridge>) -> Self {
        Self { raw }
    }

    pub fn as_raw(&self) -> *mut RawBridge {
        self.raw.as_ptr()
    }

    pub fn last_error(&self) -> String {
        let mut buffer = [0_i8; 256];
        let length = unsafe {
            streamline_bridge_copy_last_error(self.raw.as_ptr(), buffer.as_mut_ptr(), buffer.len())
        };
        let bytes = buffer[..length.min(buffer.len())]
            .iter()
            .map(|value| *value as u8)
            .collect::<Vec<_>>();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn shutdown(self) -> u32 {
        let raw = self.raw.as_ptr();
        let status = unsafe { streamline_bridge_shutdown(raw) };
        std::mem::forget(self);
        status
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        // This is the constructor-failure guard. The completed renderer consumes
        // Bridge through `shutdown`, which forgets it after the fence-ordered
        // native shutdown; any earlier `?` now still balances a successful init.
        let _ = unsafe { streamline_bridge_shutdown(self.raw.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn locks_streamline_version() {
        assert_eq!(SDK_VERSION, "2.14.1");
    }

    #[test]
    fn abi_struct_layout_is_fixed_width() {
        assert_eq!(size_of::<InitDesc>(), 64);
        assert_eq!(offset_of!(InitDesc, enable_dlss_fg), 24);
        assert_eq!(offset_of!(InitDesc, plugin_path), 32);
        assert_eq!(size_of::<Support>(), 88);
        assert_eq!(offset_of!(Support, fg_supported), 24);
        assert_eq!(offset_of!(Support, fg_result), 44);
        assert_eq!(offset_of!(Support, adapter_luid), 48);
        assert_eq!(size_of::<OptimalSettings>(), 36);
        assert_eq!(size_of::<FrameToken>(), 24);
        assert_eq!(size_of::<Viewport>(), 16);
        assert_eq!(size_of::<DlssOptions>(), 44);
        assert_eq!(size_of::<RrOptions>(), 204);
        assert_eq!(size_of::<RrOptimalSettings>(), 36);
        assert_eq!(size_of::<RrState>(), 16);
        assert_eq!(size_of::<FrameGenerationOptions>(), 56);
        assert_eq!(offset_of!(FrameGenerationOptions, flags), 16);
        assert_eq!(offset_of!(FrameGenerationOptions, color_width), 32);
        assert_eq!(size_of::<FrameGenerationState>(), 40);
        assert_eq!(
            offset_of!(FrameGenerationState, estimated_vram_usage_bytes),
            24
        );
        assert_eq!(size_of::<Constants>(), 376);
        assert_eq!(size_of::<ResourceTag>(), 48);
        assert_eq!(size_of::<ReflexState>(), 20);
    }

    #[test]
    fn invalid_bridge_statuses_are_stable() {
        assert_eq!(ABI_VERSION, 9);
        assert_eq!(STATUS_OK, 0);
        assert_eq!(STATUS_INVALID_ARGUMENT, 1);
        assert_eq!(size_of::<RawBridge>(), 0);
    }

    #[test]
    fn bridge_rejects_invalid_create_without_loading_the_sdk() {
        let mut raw = std::ptr::null_mut();
        let status = unsafe { streamline_bridge_create(std::ptr::null(), &mut raw) };
        assert_eq!(status, STATUS_INVALID_ARGUMENT);
        assert!(raw.is_null());
    }

    #[cfg(not(feature = "streamline-fg"))]
    #[test]
    fn feature_off_fg_entry_points_return_unsupported_without_sdk_state() {
        let set_status = unsafe {
            streamline_bridge_fg_set_options(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        let get_status = unsafe {
            streamline_bridge_fg_get_state(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(set_status, STATUS_UNSUPPORTED);
        assert_eq!(get_status, STATUS_UNSUPPORTED);
    }

    #[cfg(not(feature = "streamline-fg"))]
    #[test]
    fn feature_off_fg_create_request_returns_unsupported_before_sdk_init() {
        let description = InitDesc {
            struct_size: size_of::<InitDesc>() as u32,
            abi_version: ABI_VERSION,
            development: 0,
            enable_dlss: 0,
            application_id: 1,
            enable_dlss_rr: 0,
            enable_dlss_fg: 1,
            plugin_path: std::ptr::null(),
            log_path: std::ptr::null(),
            project_id: std::ptr::null(),
            engine_version: std::ptr::null(),
        };
        let mut raw = std::ptr::null_mut();
        let status = unsafe { streamline_bridge_create(&description, &mut raw) };
        assert_eq!(status, STATUS_UNSUPPORTED);
        assert!(raw.is_null());
    }

    #[test]
    fn frame_generation_source_contract_is_feature_isolated() {
        let bridge = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/native/streamline_bridge/src/streamline_bridge.cpp"
        ));
        let header = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/native/streamline_bridge/include/streamline_bridge.h"
        ));
        let cmake = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/native/streamline_bridge/CMakeLists.txt"
        ));
        let build = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/build.rs"));
        assert!(bridge.contains("#if STREAMLINE_ENABLE_FG\n#include <sl_dlss_g.h>"));
        assert!(bridge.contains("input.mode != STREAMLINE_BRIDGE_FRAME_GENERATION_ON"));
        assert!(bridge.contains("input.num_frames_to_generate != 1"));
        assert!(bridge.contains("input.flags != 0"));
        assert!(bridge.contains("valid_fg_estimate_options(*estimate_input)"));
        assert!(bridge.contains("valid_fg_resource_description(input)"));
        assert!(bridge.contains("sl::DLSSGMode::eOff"));
        assert!(bridge.contains("sl::DLSSGMode::eOn"));
        assert!(bridge.contains("options.flags = request_vram_estimate"));
        assert!(bridge.contains("options.hudLessBufferFormat = input.hud_less_buffer_format"));
        assert!(!bridge.contains("input->rendering_game_frames"));
        assert!(!header.contains("rendering_game_frames"));
        assert!(bridge.contains("slDLSSGGetState"));
        assert!(bridge.contains("*out_native_interface = nullptr"));
        assert!(bridge.contains("slGetNativeInterface"));
        assert!(bridge.contains("sl::PreferenceFlags::eAllowOTA"));
        assert!(bridge.contains("sl::PreferenceFlags::eLoadDownloadedPlugins"));
        assert!(cmake.contains("option(STREAMLINE_ENABLE_FG"));
        assert!(build.contains("optional_feature"));
        assert!(build.contains("未知 optional_feature"));
        assert!(build.contains("CARGO_FEATURE_STREAMLINE_FG"));
        assert!(build.contains("streamline_plugin_subdirectory"));
        assert!(build.contains("remove_legacy_adjacent_streamline_plugins"));
        assert!(build.contains("remove_deployed_file"));
    }

    #[test]
    fn frame_generation_feature_graph_and_api_boundaries_are_locked() {
        let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        let bridge = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/native/streamline_bridge/src/streamline_bridge.cpp"
        ));
        let lock = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/third_party/streamline/version.lock.json"
        ));

        // RR and FG must be opt-in independently: otherwise a build intended
        // to validate one optional plugin can silently deploy/load the other.
        assert!(manifest.contains("streamline-fg = [\"streamline\"]"));
        assert!(manifest.contains("streamline-rr = [\"streamline\"]"));
        assert!(!manifest.contains("streamline-rr = [\"streamline\", \"streamline-fg\"]"));
        assert!(!manifest.contains("streamline-fg = [\"streamline\", \"streamline-rr\"]"));

        // FG on/off is an options update, while state is queried through the
        // dedicated API; this first-round ABI intentionally has no evaluate,
        // tag, or feature-loaded hook that could start 11G-B early.
        assert!(bridge.contains("slDLSSGSetOptions"));
        assert!(bridge.contains("slDLSSGGetState"));
        assert!(bridge.contains("sl::kFeatureDLSS_G"));
        assert!(!bridge.contains("slDLSSGEvaluateFeature"));
        assert!(lock.contains("include/sl_dlss_g.h"));
        assert!(lock.contains("bin/x64/sl.dlss_g.dll"));
        assert!(lock.contains("bin/x64/nvngx_dlssg.dll"));
    }

    #[cfg(not(feature = "streamline-fg"))]
    #[test]
    fn feature_off_fg_loaded_entry_points_return_unsupported_without_sdk_state() {
        let set_status = unsafe {
            streamline_bridge_fg_set_loaded(std::ptr::null_mut(), 1)
        };
        let mut loaded = 0xFFFF_FFFF;
        let get_status = unsafe {
            streamline_bridge_fg_is_loaded(std::ptr::null_mut(), &mut loaded)
        };
        assert_eq!(set_status, STATUS_UNSUPPORTED);
        assert_eq!(get_status, STATUS_UNSUPPORTED);
        assert_eq!(loaded, 0xFFFF_FFFF);
    }

    #[test]
    fn frame_generation_loaded_state_source_contract_is_narrow() {
        let bridge = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/native/streamline_bridge/src/streamline_bridge.cpp"
        ));
        assert!(bridge.contains(
            "slSetFeatureLoaded(\n            sl::kFeatureDLSS_G, loaded != 0)"
        ));
        assert!(bridge.contains(
            "slIsFeatureLoaded(\n            sl::kFeatureDLSS_G, verified_loaded)"
        ));
        assert!(bridge.contains("loaded > 1"));
        assert!(bridge.contains("*out_loaded = 0"));
        assert!(bridge.contains("#if STREAMLINE_ENABLE_FG\nStreamlineBridgeStatus streamline_bridge_fg_set_loaded"));
        assert!(bridge.contains("#else\nStreamlineBridgeStatus streamline_bridge_fg_set_loaded"));
        assert!(!bridge.contains(
            "slSetFeatureLoaded(\n            sl::kFeatureDLSS_RR"
        ));
    }
}
