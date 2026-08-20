#[cfg(feature = "streamline")]
use glam::Mat4;
#[cfg(feature = "streamline")]
use std::ffi::CString;
#[cfg(feature = "streamline")]
use std::os::windows::ffi::OsStrExt;
#[cfg(feature = "nrd")]
use std::ptr::null_mut;
use std::{
    collections::VecDeque,
    ffi::c_void,
    mem::{ManuallyDrop, size_of},
    path::PathBuf,
    time::Instant,
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND},
        Graphics::{
            Direct3D::D3D_FEATURE_LEVEL_12_0,
            Direct3D12::*,
            Dxgi::{Common::*, *},
        },
        System::Threading::{CreateEventW, INFINITE, WaitForSingleObject},
    },
    core::{Error as WindowsError, Interface, Result},
};
use winit::{
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::Window,
};

#[cfg(feature = "nrd")]
use crate::reconstruction;
#[cfg(feature = "streamline")]
use crate::upscaler::DlssFrameInput;
use crate::{
    as_policy::AccelerationStructureStats,
    debug_view::DebugView,
    path_space::{ActivePathSpace, PathSpaceMode, resolve_path_space},
    realtime::{AtrousMode, CommandRecordingMode, RealtimeConfig, ReflexMode},
    reconstruction::{
        CameraPose, DenoiserBackend, NRD_COMMIT_PREFIX, NRD_VERSION,
        RECONSTRUCTION_CONTRACT_VERSION, ReconstructionFrameInput, ReconstructionFrameState,
        ReconstructionPath,
    },
    resolution::{
        DynamicResolutionConfig, DynamicResolutionController, DynamicResolutionDirection, Extent2D,
        RenderExtentChange, RenderScale, ResolutionMode, classify_render_extent_change,
        render_extent,
    },
    scene::{MAX_SCENE_SAMPLERS, SceneAsset, gltf_loader},
    upscaler::UpscalerMode,
};

use self::{
    capture::{CaptureMetadata, capture_json_line, unpack_rgba8_rows, write_png_atomic},
    descriptor::DescriptorHeap,
    memory::{
        VideoMemoryMeasurement, VideoMemorySnapshot, VideoMemoryStatus, VideoMemoryTelemetry,
    },
    pipeline::ComputePipeline,
    profiler::{CommandRecordingFrameStats, GpuPass, GpuProfiler},
    render_resources::{RenderGenerationDesc, RenderResourceGeneration},
    resource::{BarrierSubmissionMode, TrackedResource, TransitionBatch},
    shader::{ReloadedShaders, ShaderReloader},
    texture::TextureSet,
};
use raytracing::{AccelerationStructures, RaytracingPipeline, SceneGeometry};
#[cfg(feature = "nrd")]
use render_resources::NrdDenoiserResources;

struct RetiredRenderResourceGeneration {
    retire_fence: u64,
    resources: RenderResourceGeneration,
    #[cfg(feature = "streamline")]
    streamline_viewport: Option<StreamlineViewport>,
}

#[cfg(feature = "nrd")]
struct NrdBackend {
    handle: *mut reconstruction::NrdBridgeOpaque,
    extent: Extent2D,
}

#[cfg(feature = "nrd")]
impl NrdBackend {
    fn new(device: &ID3D12Device, extent: Extent2D) -> Result<Self> {
        let description = reconstruction::NrdBridgeCreateDesc {
            abi_version: reconstruction::NRD_BRIDGE_ABI_VERSION,
            resource_width: extent.width,
            resource_height: extent.height,
            queued_frames: reconstruction::RECONSTRUCTION_FRAMES_IN_FLIGHT,
            device: device.as_raw() as usize,
        };
        let mut handle = null_mut();
        let status = unsafe { reconstruction::nrd_bridge_create(&description, &mut handle) };
        if status != reconstruction::NRD_BRIDGE_STATUS_OK || handle.is_null() {
            eprintln!(
                "nrd_generation_create status=failed extent={}x{} version={} error=status={status}",
                extent.width, extent.height, NRD_VERSION
            );
            return Err(WindowsError::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                format!("创建 NRD bridge 失败，status={status}"),
            ));
        }
        eprintln!(
            "nrd_generation_create status=ok extent={}x{} version={} commit={}",
            extent.width, extent.height, NRD_VERSION, NRD_COMMIT_PREFIX
        );
        Ok(Self { handle, extent })
    }

    unsafe fn denoise(
        &mut self,
        frame_state: &ReconstructionFrameState,
        resources: &mut reconstruction::NrdBridgeResources,
        command_list: &ID3D12GraphicsCommandList,
        enable_validation: bool,
    ) -> Result<()> {
        debug_assert_eq!(
            (frame_state.render_width, frame_state.render_height),
            (self.extent.width, self.extent.height)
        );
        let frame = reconstruction::NrdBridgeFrameDesc {
            state: frame_state,
            enable_validation: u32::from(enable_validation),
        };
        let status = unsafe {
            reconstruction::nrd_bridge_denoise(
                self.handle,
                &frame,
                resources,
                command_list.as_raw(),
            )
        };
        if status != reconstruction::NRD_BRIDGE_STATUS_OK {
            let mut message = [0_u8; 256];
            let length = unsafe {
                reconstruction::nrd_bridge_copy_last_error(
                    self.handle,
                    message.as_mut_ptr(),
                    message.len(),
                )
            };
            let message = String::from_utf8_lossy(&message[..length.min(message.len())]);
            return Err(WindowsError::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                format!("NRD denoise 失败，status={status}: {message}"),
            ));
        }
        Ok(())
    }
}

#[cfg(feature = "nrd")]
impl Drop for NrdBackend {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { reconstruction::nrd_bridge_destroy(self.handle) };
            self.handle = null_mut();
        }
    }
}

#[cfg(feature = "streamline")]
struct StreamlineRuntime {
    bridge: crate::streamline::Bridge,
    _support: crate::streamline::Support,
    dlss_configured: bool,
    _rr_configured: bool,
}

#[cfg(feature = "streamline")]
struct StreamlineViewport {
    viewport: crate::streamline::Viewport,
    options: crate::streamline::DlssOptions,
    optimal: crate::streamline::OptimalSettings,
    #[cfg(feature = "streamline-rr")]
    rr_options: Option<crate::streamline::RrOptions>,
    #[cfg(feature = "streamline-rr")]
    rr_optimal: Option<crate::streamline::RrOptimalSettings>,
    #[cfg(feature = "streamline-rr")]
    is_rr: bool,
    resources_allocated: bool,
}

#[cfg(feature = "streamline")]
impl StreamlineRuntime {
    fn create_before_dxgi(
        application_id: Option<u32>,
        enable_rr: bool,
        enable_fg: bool,
    ) -> Result<crate::streamline::Bridge> {
        let executable_directory = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(PathBuf::from))
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| streamline_error("定位 Streamline plugin 目录", 0))?;
        // Only the interposer loader lives beside the executable. Plugins are
        // immutable per feature set so cached Cargo builds cannot scan stale
        // RR/FG DLLs left by a different executable configuration.
        let plugin_directory =
            executable_directory.join(env!("RAY_TRACING_STREAMLINE_PLUGIN_SUBDIR"));
        let plugin_path = plugin_directory
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let project_id = CString::new(crate::realtime::STREAMLINE_PROJECT_ID)
            .expect("built-in Streamline Project ID cannot contain NUL");
        let engine_version = CString::new(crate::realtime::STREAMLINE_ENGINE_VERSION)
            .expect("built-in Streamline engine version cannot contain NUL");
        let description = crate::streamline::InitDesc {
            struct_size: size_of::<crate::streamline::InitDesc>() as u32,
            abi_version: crate::streamline::ABI_VERSION,
            development: u32::from(cfg!(debug_assertions)),
            enable_dlss: 1,
            application_id: application_id.unwrap_or(0),
            enable_dlss_rr: u32::from(enable_rr),
            enable_dlss_fg: u32::from(enable_fg),
            plugin_path: plugin_path.as_ptr(),
            log_path: std::ptr::null(),
            project_id: project_id.as_ptr(),
            engine_version: engine_version.as_ptr(),
        };
        crate::streamline::Bridge::create(&description)
            .map_err(|status| streamline_error("初始化 Streamline（必须早于 DXGI）", status))
    }

    unsafe fn attach(
        bridge: crate::streamline::Bridge,
        device: &ID3D12Device,
        adapter: &IDXGIAdapter1,
        reflex_mode: crate::realtime::ReflexMode,
        dlss_configured: bool,
        rr_configured: bool,
    ) -> Result<Self> {
        let adapter_description = unsafe { adapter.GetDesc1()? };
        let adapter_luid = u64::from(adapter_description.AdapterLuid.LowPart)
            | (u64::from(adapter_description.AdapterLuid.HighPart as u32) << 32);
        let adapter_bytes = adapter_luid.to_ne_bytes();
        let status = unsafe {
            crate::streamline::streamline_bridge_set_d3d_device(
                bridge.as_raw(),
                device.as_raw(),
                adapter_bytes.as_ptr(),
                adapter_bytes.len(),
            )
        };
        if status != crate::streamline::STATUS_OK {
            let error = bridge.last_error();
            return Err(streamline_error_with_detail(
                "绑定 Streamline D3D12 device",
                status,
                error,
            ));
        }
        let mut support = crate::streamline::Support {
            struct_size: size_of::<crate::streamline::Support>() as u32,
            abi_version: crate::streamline::ABI_VERSION,
            ..Default::default()
        };
        let status = unsafe {
            crate::streamline::streamline_bridge_query_support(bridge.as_raw(), &mut support)
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "查询 Streamline support",
                status,
                bridge.last_error(),
            ));
        }
        eprintln!(
            "Streamline support: dlss={}({}), reflex={}({}), pcl={}({}), rr={}({}), fg={}({}), adapter_luid=0x{:016x}",
            support.dlss_supported,
            support.dlss_result,
            support.reflex_supported,
            support.reflex_result,
            support.pcl_supported,
            support.pcl_result,
            support.rr_supported,
            support.rr_result,
            support.fg_supported,
            support.fg_result,
            support.adapter_luid,
        );
        if support.reflex_supported != 0 {
            let status = unsafe {
                crate::streamline::streamline_bridge_reflex_set_mode(
                    bridge.as_raw(),
                    reflex_mode.sdk_mode(),
                )
            };
            if status != crate::streamline::STATUS_OK {
                return Err(streamline_error_with_detail(
                    "设置 Reflex mode",
                    status,
                    bridge.last_error(),
                ));
            }
        }
        Ok(Self {
            bridge,
            _support: support,
            dlss_configured,
            _rr_configured: rr_configured,
        })
    }

    /// Upgrade a COM interface immediately after creation for the manual
    /// hooking path. The bridge proxy AddRefs the original interface; the
    /// temporary Rust clone owns and releases the input reference, while the
    /// returned interface owns the proxy reference.
    unsafe fn upgrade_interface<T: Interface>(&self, interface: T) -> Result<T> {
        let original = interface.into_raw();
        let mut upgraded = original;
        let status = unsafe {
            crate::streamline::streamline_bridge_upgrade_interface(
                self.bridge.as_raw(),
                &mut upgraded,
            )
        };
        if status == crate::streamline::STATUS_ALREADY_UPGRADED {
            return Ok(unsafe { T::from_raw(original) });
        }
        if status != crate::streamline::STATUS_OK {
            unsafe { drop(T::from_raw(original)) };
            return Err(streamline_error_with_detail(
                "升级 Streamline manual-hooking interface",
                status,
                self.bridge.last_error(),
            ));
        }
        if !std::ptr::eq(upgraded, original) {
            unsafe { drop(T::from_raw(original)) };
        }
        Ok(unsafe { T::from_raw(upgraded.cast()) })
    }

    fn reflex_supported(&self) -> bool {
        self._support.reflex_supported != 0
    }

    fn pcl_supported(&self) -> bool {
        self._support.pcl_supported != 0
    }

    unsafe fn get_frame_token(&self, frame_index: u32) -> Result<crate::streamline::FrameToken> {
        let mut token = crate::streamline::FrameToken {
            struct_size: size_of::<crate::streamline::FrameToken>() as u32,
            abi_version: crate::streamline::ABI_VERSION,
            ..Default::default()
        };
        let status = unsafe {
            crate::streamline::streamline_bridge_get_frame_token(
                self.bridge.as_raw(),
                frame_index,
                &mut token,
            )
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "获取 Streamline frame token",
                status,
                self.bridge.last_error(),
            ));
        }
        Ok(token)
    }

    unsafe fn reflex_sleep(&self, token: &crate::streamline::FrameToken) -> Result<()> {
        let status = unsafe {
            crate::streamline::streamline_bridge_reflex_sleep(self.bridge.as_raw(), token)
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "调用 Reflex sleep",
                status,
                self.bridge.last_error(),
            ));
        }
        Ok(())
    }

    unsafe fn pcl_marker(&self, token: &crate::streamline::FrameToken, marker: u32) -> Result<()> {
        let status = unsafe {
            crate::streamline::streamline_bridge_pcl_marker(self.bridge.as_raw(), token, marker)
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "提交 PCL marker",
                status,
                self.bridge.last_error(),
            ));
        }
        Ok(())
    }

    fn dlss_options(
        mode: crate::upscaler::UpscalerMode,
        output_extent: Extent2D,
    ) -> Result<crate::streamline::DlssOptions> {
        Ok(crate::streamline::DlssOptions {
            struct_size: size_of::<crate::streamline::DlssOptions>() as u32,
            abi_version: crate::streamline::ABI_VERSION,
            mode: mode.dlss_mode().ok_or_else(|| {
                streamline_error("DLSS 模式映射", crate::streamline::STATUS_INVALID_ARGUMENT)
            })?,
            output_width: output_extent.width,
            output_height: output_extent.height,
            sharpness: 0.0,
            pre_exposure: 1.0,
            exposure_scale: 1.0,
            color_buffers_hdr: 1,
            use_auto_exposure: 0,
            alpha_upscaling_enabled: 0,
        })
    }

    #[cfg(feature = "streamline-rr")]
    fn rr_options(
        mode: crate::upscaler::UpscalerMode,
        output_extent: Extent2D,
    ) -> Result<crate::streamline::RrOptions> {
        Ok(crate::streamline::RrOptions {
            struct_size: size_of::<crate::streamline::RrOptions>() as u32,
            abi_version: crate::streamline::ABI_VERSION,
            mode: mode.dlss_mode().ok_or_else(|| {
                streamline_error(
                    "DLSS RR 模式映射",
                    crate::streamline::STATUS_INVALID_ARGUMENT,
                )
            })?,
            output_width: output_extent.width,
            output_height: output_extent.height,
            sharpness: 0.0,
            pre_exposure: 1.0,
            exposure_scale: 1.0,
            color_buffers_hdr: 1,
            indicator_invert_axis_x: 0,
            indicator_invert_axis_y: 0,
            normal_roughness_mode: 1,
            world_to_camera_view: glam::Mat4::IDENTITY.to_cols_array(),
            camera_view_to_world: glam::Mat4::IDENTITY.to_cols_array(),
            alpha_upscaling_enabled: 0,
            // Apply one validated model to every performance mode. eDefault
            // may change after an OTA and is not a stable validation baseline.
            dlaa_preset: DLSS_RR_RENDER_PRESET,
            quality_preset: DLSS_RR_RENDER_PRESET,
            balanced_preset: DLSS_RR_RENDER_PRESET,
            performance_preset: DLSS_RR_RENDER_PRESET,
            ultra_performance_preset: DLSS_RR_RENDER_PRESET,
            ultra_quality_preset: DLSS_RR_RENDER_PRESET,
        })
    }

    unsafe fn create_viewport(
        &self,
        mode: crate::upscaler::UpscalerMode,
        output_extent: Extent2D,
        id: u32,
    ) -> Result<StreamlineViewport> {
        if !self.dlss_configured {
            return Err(streamline_error_with_detail(
                "DLSS application identity",
                crate::streamline::STATUS_NOT_INITIALIZED,
                "DLSS 未加载；请提供 NVIDIA 分配的 --streamline-application-id",
            ));
        }
        if self._support.dlss_supported == 0 {
            return Err(streamline_error_with_detail(
                "DLSS support",
                self._support.dlss_result,
                format!("GPU 不支持 DLSS，SDK result={}", self._support.dlss_result),
            ));
        }
        let options = Self::dlss_options(mode, output_extent)?;
        let mut optimal = crate::streamline::OptimalSettings {
            struct_size: size_of::<crate::streamline::OptimalSettings>() as u32,
            abi_version: crate::streamline::ABI_VERSION,
            ..Default::default()
        };
        let status = unsafe {
            crate::streamline::streamline_bridge_get_optimal_settings(
                self.bridge.as_raw(),
                &options,
                &mut optimal,
            )
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "查询 DLSS optimal settings",
                status,
                self.bridge.last_error(),
            ));
        }
        if optimal.optimal_render_width == 0 || optimal.optimal_render_height == 0 {
            return Err(streamline_error(
                "DLSS optimal settings 返回零尺寸",
                crate::streamline::STATUS_SDK_ERROR,
            ));
        }
        let optimal_mode = mode;
        crate::upscaler::render_extent_from_optimal(
            optimal_mode,
            output_extent,
            crate::upscaler::DlssOptimalSettings {
                render_width: optimal.optimal_render_width,
                render_height: optimal.optimal_render_height,
            },
        )
        .map_err(|error| {
            let detail = match error {
                crate::upscaler::DlssExtentError::Zero => "零尺寸",
                crate::upscaler::DlssExtentError::LargerThanOutput => "内部尺寸大于输出尺寸",
                crate::upscaler::DlssExtentError::DlaaMustMatchOutput => {
                    "DLAA 内部尺寸必须等于输出尺寸"
                }
            };
            WindowsError::new(
                windows::core::HRESULT(0x80070057_u32 as i32),
                format!(
                    "校验 DLSS optimal settings 失败：{detail}: {}x{}",
                    optimal.optimal_render_width, optimal.optimal_render_height
                ),
            )
        })?;
        let viewport = crate::streamline::Viewport {
            struct_size: size_of::<crate::streamline::Viewport>() as u32,
            abi_version: crate::streamline::ABI_VERSION,
            id,
            reserved: 0,
        };
        let status = unsafe {
            crate::streamline::streamline_bridge_dlss_set_options(
                self.bridge.as_raw(),
                &viewport,
                &options,
            )
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "设置 DLSS options",
                status,
                self.bridge.last_error(),
            ));
        }
        Ok(StreamlineViewport {
            viewport,
            options,
            optimal,
            #[cfg(feature = "streamline-rr")]
            rr_options: None,
            #[cfg(feature = "streamline-rr")]
            rr_optimal: None,
            #[cfg(feature = "streamline-rr")]
            is_rr: false,
            resources_allocated: false,
        })
    }

    #[cfg(feature = "streamline-rr")]
    unsafe fn create_rr_viewport(
        &self,
        mode: crate::upscaler::UpscalerMode,
        output_extent: Extent2D,
        id: u32,
    ) -> Result<StreamlineViewport> {
        if !self.dlss_configured {
            return Err(streamline_error_with_detail(
                "DLSS RR application identity",
                crate::streamline::STATUS_NOT_INITIALIZED,
                "DLSS RR 未加载；请提供 NVIDIA 分配的 --streamline-application-id",
            ));
        }
        if !self._rr_configured {
            return Err(streamline_error_with_detail(
                "DLSS RR plugin lifecycle",
                crate::streamline::STATUS_NOT_INITIALIZED,
                "当前 Streamline 会话未加载 RR；只能从以 --denoiser dlss-rr 启动的会话进行 RR A/B",
            ));
        }
        if self._support.rr_supported == 0 {
            return Err(streamline_error_with_detail(
                "DLSS RR support",
                self._support.rr_result,
                format!(
                    "GPU/driver 不支持 DLSS RR，SDK result={}",
                    self._support.rr_result
                ),
            ));
        }
        let dlss_options = Self::dlss_options(mode, output_extent)?;
        let rr_options = Self::rr_options(mode, output_extent)?;
        let mut optimal = crate::streamline::RrOptimalSettings {
            struct_size: size_of::<crate::streamline::RrOptimalSettings>() as u32,
            abi_version: crate::streamline::ABI_VERSION,
            ..Default::default()
        };
        let status = unsafe {
            crate::streamline::streamline_bridge_rr_get_optimal_settings(
                self.bridge.as_raw(),
                &rr_options,
                &mut optimal,
            )
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "查询 DLSS RR optimal settings",
                status,
                self.bridge.last_error(),
            ));
        }
        crate::upscaler::render_extent_from_optimal(
            mode,
            output_extent,
            crate::upscaler::DlssOptimalSettings {
                render_width: optimal.optimal_render_width,
                render_height: optimal.optimal_render_height,
            },
        )
        .map_err(|error| {
            WindowsError::new(
                windows::core::HRESULT(0x80070057_u32 as i32),
                format!("DLSS RR optimal settings 无效：{error:?}"),
            )
        })?;
        let viewport = crate::streamline::Viewport {
            struct_size: size_of::<crate::streamline::Viewport>() as u32,
            abi_version: crate::streamline::ABI_VERSION,
            id,
            reserved: 0,
        };
        // DLSSDOptions extends, rather than replaces, the compatible DLSS
        // options. Submit the base contract first on the same viewport so the
        // plugin sees one coherent mode/output/HDR configuration.
        let status = unsafe {
            crate::streamline::streamline_bridge_dlss_set_options(
                self.bridge.as_raw(),
                &viewport,
                &dlss_options,
            )
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "设置 DLSS RR compatible DLSS options",
                status,
                self.bridge.last_error(),
            ));
        }
        let status = unsafe {
            crate::streamline::streamline_bridge_rr_set_options(
                self.bridge.as_raw(),
                &viewport,
                &rr_options,
            )
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "设置 DLSS RR options",
                status,
                self.bridge.last_error(),
            ));
        }
        Ok(StreamlineViewport {
            viewport,
            options: dlss_options,
            optimal: crate::streamline::OptimalSettings::default(),
            rr_options: Some(rr_options),
            rr_optimal: Some(optimal),
            is_rr: true,
            resources_allocated: false,
        })
    }

    unsafe fn create_reconstruction_viewport(
        &self,
        backend: DenoiserBackend,
        mode: crate::upscaler::UpscalerMode,
        output_extent: Extent2D,
        id: u32,
    ) -> Result<StreamlineViewport> {
        if backend == DenoiserBackend::DlssRayReconstruction {
            #[cfg(feature = "streamline-rr")]
            {
                return unsafe { self.create_rr_viewport(mode, output_extent, id) };
            }
            #[cfg(not(feature = "streamline-rr"))]
            {
                return Err(streamline_error(
                    "DLSS RR feature",
                    crate::streamline::STATUS_UNSUPPORTED,
                ));
            }
        }
        unsafe { self.create_viewport(mode, output_extent, id) }
    }

    unsafe fn begin_frame(
        &self,
        viewport: &StreamlineViewport,
        token: &crate::streamline::FrameToken,
        input: &DlssFrameInput,
        camera: CameraPose,
    ) -> Result<crate::streamline::FrameToken> {
        let constants = dlss_streamline_constants(input, camera);
        let status = unsafe {
            crate::streamline::streamline_bridge_set_constants(
                self.bridge.as_raw(),
                token,
                &viewport.viewport,
                &constants,
            )
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "提交 DLSS constants",
                status,
                self.bridge.last_error(),
            ));
        }
        Ok(*token)
    }

    #[cfg(feature = "streamline-rr")]
    unsafe fn begin_rr_frame(
        &self,
        viewport: &mut StreamlineViewport,
        token: &crate::streamline::FrameToken,
        input: &DlssFrameInput,
        camera: CameraPose,
    ) -> Result<crate::streamline::FrameToken> {
        let _ = unsafe { self.begin_frame(viewport, token, input, camera)? };
        // Keep the v2.12 DLSS + DLSSD option pair ordered on every RR frame.
        // Most fields are stable, but resubmitting both avoids retaining a
        // partially updated plugin state when RR matrices change below.
        let status = unsafe {
            crate::streamline::streamline_bridge_dlss_set_options(
                self.bridge.as_raw(),
                &viewport.viewport,
                &viewport.options,
            )
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "提交 DLSS RR compatible DLSS frame options",
                status,
                self.bridge.last_error(),
            ));
        }
        let mut options = viewport.rr_options.ok_or_else(|| {
            streamline_error(
                "DLSS RR viewport options",
                crate::streamline::STATUS_NOT_INITIALIZED,
            )
        })?;
        options.world_to_camera_view = input.world_to_view;
        options.camera_view_to_world = row_major_inverse(input.world_to_view);
        let status = unsafe {
            crate::streamline::streamline_bridge_rr_set_options(
                self.bridge.as_raw(),
                &viewport.viewport,
                &options,
            )
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "提交 DLSS RR frame options",
                status,
                self.bridge.last_error(),
            ));
        }
        viewport.rr_options = Some(options);
        Ok(*token)
    }

    unsafe fn begin_reconstruction_frame(
        &self,
        backend: DenoiserBackend,
        viewport: &mut StreamlineViewport,
        token: &crate::streamline::FrameToken,
        input: &DlssFrameInput,
        camera: CameraPose,
    ) -> Result<crate::streamline::FrameToken> {
        if backend == DenoiserBackend::DlssRayReconstruction {
            #[cfg(feature = "streamline-rr")]
            {
                return unsafe { self.begin_rr_frame(viewport, token, input, camera) };
            }
            #[cfg(not(feature = "streamline-rr"))]
            {
                return Err(streamline_error(
                    "DLSS RR feature",
                    crate::streamline::STATUS_UNSUPPORTED,
                ));
            }
        }
        unsafe { self.begin_frame(viewport, token, input, camera) }
    }

    unsafe fn set_tags_and_evaluate(
        &self,
        viewport: &mut StreamlineViewport,
        token: &crate::streamline::FrameToken,
        tags: &[crate::streamline::ResourceTag],
        command_list: &ID3D12GraphicsCommandList,
    ) -> Result<()> {
        let status = unsafe {
            crate::streamline::streamline_bridge_set_tags(
                self.bridge.as_raw(),
                token,
                &viewport.viewport,
                tags.as_ptr(),
                tags.len() as u32,
                command_list.as_raw(),
            )
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "提交 DLSS resource tags",
                status,
                self.bridge.last_error(),
            ));
        }
        // Do not call slAllocateResources here. Its v2.12.0 API has no frame
        // token and therefore looks up frame 0, which is incompatible with our
        // frame-based tags. The first evaluate is the documented lazy-allocation
        // path and consumes the correct token/constants/tags atomically.
        let status = unsafe {
            crate::streamline::streamline_bridge_evaluate_dlss(
                self.bridge.as_raw(),
                token,
                &viewport.viewport,
                command_list.as_raw(),
            )
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "执行 DLSS evaluate",
                status,
                self.bridge.last_error(),
            ));
        }
        viewport.resources_allocated = true;
        Ok(())
    }

    #[cfg(feature = "streamline-rr")]
    unsafe fn set_rr_tags_and_evaluate(
        &self,
        viewport: &mut StreamlineViewport,
        token: &crate::streamline::FrameToken,
        tags: &[crate::streamline::ResourceTag],
        command_list: &ID3D12GraphicsCommandList,
    ) -> Result<()> {
        let status = unsafe {
            crate::streamline::streamline_bridge_set_tags(
                self.bridge.as_raw(),
                token,
                &viewport.viewport,
                tags.as_ptr(),
                tags.len() as u32,
                command_list.as_raw(),
            )
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "提交 DLSS RR resource tags",
                status,
                self.bridge.last_error(),
            ));
        }
        let status = unsafe {
            crate::streamline::streamline_bridge_evaluate_rr(
                self.bridge.as_raw(),
                token,
                &viewport.viewport,
                command_list.as_raw(),
            )
        };
        if status != crate::streamline::STATUS_OK {
            return Err(streamline_error_with_detail(
                "执行 DLSS RR evaluate",
                status,
                self.bridge.last_error(),
            ));
        }
        viewport.resources_allocated = true;
        Ok(())
    }

    unsafe fn free_resources(&self, viewport: &mut StreamlineViewport) {
        if !viewport.resources_allocated {
            return;
        }
        #[cfg(feature = "streamline-rr")]
        let is_rr = viewport.is_rr;
        #[cfg(not(feature = "streamline-rr"))]
        let is_rr = false;
        let status = if is_rr {
            #[cfg(feature = "streamline-rr")]
            {
                unsafe {
                    crate::streamline::streamline_bridge_rr_free_resources(
                        self.bridge.as_raw(),
                        &viewport.viewport,
                    )
                }
            }
            #[cfg(not(feature = "streamline-rr"))]
            {
                crate::streamline::STATUS_UNSUPPORTED
            }
        } else {
            unsafe {
                crate::streamline::streamline_bridge_free_resources(
                    self.bridge.as_raw(),
                    &viewport.viewport,
                )
            }
        };
        if status != crate::streamline::STATUS_OK {
            eprintln!("Streamline free resources 失败：status={status}");
        } else {
            viewport.resources_allocated = false;
        }
    }

    unsafe fn shutdown_after_gpu(self) {
        let status = self.bridge.shutdown();
        if status != crate::streamline::STATUS_OK {
            eprintln!("Streamline shutdown 失败：status={status}");
        }
    }
}

#[cfg(feature = "streamline")]
impl StreamlineViewport {
    fn optimal_extent(&self) -> Extent2D {
        #[cfg(feature = "streamline-rr")]
        if self.is_rr
            && let Some(optimal) = self.rr_optimal
        {
            return Extent2D {
                width: optimal.optimal_render_width,
                height: optimal.optimal_render_height,
            };
        }
        Extent2D {
            width: self.optimal.optimal_render_width,
            height: self.optimal.optimal_render_height,
        }
    }
}

#[cfg(feature = "streamline")]
fn streamline_error(stage: &str, status: u32) -> WindowsError {
    WindowsError::new(
        windows::core::HRESULT(0x80004005_u32 as i32),
        format!("{stage} 失败，status={status}"),
    )
}

#[cfg(feature = "streamline")]
fn streamline_error_with_detail(
    stage: &str,
    status: u32,
    detail: impl std::fmt::Display,
) -> WindowsError {
    WindowsError::new(
        windows::core::HRESULT(0x80004005_u32 as i32),
        format!("{stage} 失败，status={status}: {detail}"),
    )
}

#[cfg(feature = "streamline")]
fn dlss_streamline_constants(
    input: &DlssFrameInput,
    camera: CameraPose,
) -> crate::streamline::Constants {
    let current_view = Mat4::from_cols_array(&input.world_to_view).transpose();
    let previous_view = Mat4::from_cols_array(&input.world_to_view_prev).transpose();
    let current_projection = Mat4::from_cols_array(&input.view_to_clip).transpose();
    let previous_projection = Mat4::from_cols_array(&input.view_to_clip_prev).transpose();
    let current_clip = current_projection * current_view;
    let previous_clip = previous_projection * previous_view;
    let clip_to_prev_clip = previous_clip * current_clip.inverse();
    let prev_clip_to_clip = clip_to_prev_clip.inverse();
    let forward = glam::Vec3::new(
        camera.yaw.sin() * camera.pitch.cos(),
        camera.pitch.sin(),
        camera.yaw.cos() * camera.pitch.cos(),
    )
    .normalize();
    let right = glam::Vec3::Y.cross(forward).normalize();
    let up = forward.cross(right).normalize();
    crate::streamline::Constants {
        struct_size: size_of::<crate::streamline::Constants>() as u32,
        abi_version: crate::streamline::ABI_VERSION,
        camera_view_to_clip: input.view_to_clip,
        clip_to_camera_view: crate::upscaler::to_row_major(
            current_projection.inverse().to_cols_array(),
        ),
        clip_to_prev_clip: crate::upscaler::to_row_major(clip_to_prev_clip.to_cols_array()),
        prev_clip_to_clip: crate::upscaler::to_row_major(prev_clip_to_clip.to_cols_array()),
        jitter_offset: input.jitter_px,
        mvec_scale: input.motion_vector_scale,
        camera_position: camera.position,
        camera_up: up.to_array(),
        camera_right: right.to_array(),
        camera_forward: forward.to_array(),
        camera_near: 0.001,
        camera_far: 1_000.0,
        camera_fov: 40.0_f32.to_radians(),
        camera_aspect_ratio: input.render_width.max(1) as f32 / input.render_height.max(1) as f32,
        depth_inverted: input.depth_inverted,
        camera_motion_included: 1,
        motion_vectors_3d: 0,
        reset: input.reset,
        motion_vectors_jittered: input.motion_vectors_jittered,
    }
}

#[cfg(feature = "streamline-rr")]
fn row_major_inverse(row_major: [f32; 16]) -> [f32; 16] {
    // `glam` stores column-major arrays. Transpose at the ABI boundary so the
    // matrix sent to DLSSD remains row-major and is the exact inverse of the
    // same-frame world-to-view matrix, without projection jitter.
    Mat4::from_cols_array(&row_major)
        .transpose()
        .inverse()
        .transpose()
        .to_cols_array()
}

#[cfg(feature = "streamline")]
fn streamline_resource_tag(
    resource: &TrackedResource,
    buffer_type: u32,
    lifecycle: u32,
    extent: Extent2D,
) -> crate::streamline::ResourceTag {
    crate::streamline::ResourceTag {
        struct_size: size_of::<crate::streamline::ResourceTag>() as u32,
        abi_version: crate::streamline::ABI_VERSION,
        resource: resource.resource().as_raw(),
        state: resource.state().0 as u32,
        buffer_type,
        lifecycle,
        top: 0,
        left: 0,
        width: extent.width,
        height: extent.height,
    }
}

mod capture;
mod descriptor;
mod memory;
mod pipeline;
mod pix;
pub(crate) mod profiler;
mod raytracing;
mod render_resources;
mod resource;
mod shader;
mod texture;

const FRAME_COUNT: usize = 3;
// Four SRVs followed by six UAVs. Keep this as the single source of truth for
// every feature-specific table that begins immediately after BuildStablePlanes.
const STABLE_BUILD_DESCRIPTOR_COUNT: usize = 10;
#[cfg(not(any(feature = "streamline", feature = "nrd")))]
const STABLE_BUILD_TABLE_BASE: usize = 319;
#[cfg(not(any(feature = "streamline", feature = "nrd")))]
const SHADER_DESCRIPTOR_COUNT: usize = 329;
#[cfg(all(feature = "nrd", not(feature = "streamline")))]
const STABLE_BUILD_TABLE_BASE: usize = 368;
#[cfg(all(feature = "nrd", not(feature = "streamline")))]
const SHADER_DESCRIPTOR_COUNT: usize = 435;
#[cfg(all(
    feature = "streamline",
    not(feature = "streamline-rr"),
    not(feature = "nrd")
))]
const STABLE_BUILD_TABLE_BASE: usize = 391;
#[cfg(all(
    feature = "streamline",
    not(feature = "streamline-rr"),
    not(feature = "nrd")
))]
const SHADER_DESCRIPTOR_COUNT: usize = 401;
#[cfg(all(
    feature = "streamline",
    not(feature = "streamline-rr"),
    feature = "nrd"
))]
const STABLE_BUILD_TABLE_BASE: usize = 391;
#[cfg(all(
    feature = "streamline",
    not(feature = "streamline-rr"),
    feature = "nrd"
))]
const SHADER_DESCRIPTOR_COUNT: usize = 458;
#[cfg(all(feature = "streamline-rr", not(feature = "nrd")))]
const STABLE_BUILD_TABLE_BASE: usize = 501;
#[cfg(all(feature = "streamline-rr", not(feature = "nrd")))]
const SHADER_DESCRIPTOR_COUNT: usize = 524;
#[cfg(all(feature = "streamline-rr", feature = "nrd"))]
const STABLE_BUILD_TABLE_BASE: usize = 501;
#[cfg(all(feature = "streamline-rr", feature = "nrd"))]
const SHADER_DESCRIPTOR_COUNT: usize = 581;
const DXR_UAV_REGISTER_COUNT: usize = 40;
const RECONSTRUCTION_DIFFUSE_HIT_DISTANCE_UAV_REGISTER: usize = 15;
const RECONSTRUCTION_SPECULAR_HIT_DISTANCE_UAV_REGISTER: usize = 16;
const RECONSTRUCTION_PRIMARY_EMISSIVE_UAV_REGISTER: usize = 17;
const DLSS_DEPTH_UAV_REGISTER: usize = 18;
const DLSS_MOTION_UAV_REGISTER: usize = 19;
const DLSS_SPECULAR_MOTION_UAV_REGISTER: usize = 31;
const TRANSMISSION_RAW_DIFFUSE_UAV_REGISTER: usize = 20;
const TRANSMISSION_VIEW_PROXY_UAV_REGISTER: usize = 30;
const STABLE_PLANE_RECORD_UAV_REGISTER: usize = 32;
const STABLE_PLANE_HEADER_UAV_REGISTER: usize = 33;
const STABLE_PLANE_DIFFUSE_UAV_REGISTER: usize = 34;
const STABLE_PLANE_SPECULAR_UAV_REGISTER: usize = 35;
const STABLE_RADIANCE_UAV_REGISTER: usize = 36;
const STABLE_DIFFUSE_ALBEDO_UAV_REGISTER: usize = 37;
const STABLE_SPECULAR_ALBEDO_UAV_REGISTER: usize = 38;
const STABLE_PLANE_COUNTER_UAV_REGISTER: usize = 39;
#[cfg(feature = "streamline")]
const PCL_SIMULATION_START: u32 = 0;
#[cfg(feature = "streamline")]
const PCL_SIMULATION_END: u32 = 1;
#[cfg(feature = "streamline")]
const PCL_RENDER_SUBMIT_START: u32 = 2;
#[cfg(feature = "streamline")]
const PCL_RENDER_SUBMIT_END: u32 = 3;
#[cfg(feature = "streamline")]
const PCL_PRESENT_START: u32 = 4;
#[cfg(feature = "streamline")]
const PCL_PRESENT_END: u32 = 5;
const STAGE3_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage3_triangle.dxil"));
const TEMPORAL_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage6_temporal.dxil"));
const ATROUS_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage6_atrous.dxil"));
const ATROUS_SHARED_SHADER: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/stage8_atrous_shared.dxil"));
const TONEMAP_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage6_tonemap.dxil"));
const STABLE_PLANE_BUILD_SHADER: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/stage11_stable_plane_build.dxil"));
#[cfg(feature = "nrd")]
const NRD_PREP_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage9_nrd_prep.dxil"));
#[cfg(feature = "nrd")]
const NRD_COMPOSE_SHADER: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/stage9_nrd_compose.dxil"));
#[cfg(feature = "nrd")]
const NRD_STABLE_PREP_SHADER: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/stage11_nrd_stable_prep.dxil"));
#[cfg(feature = "nrd")]
const NRD_STABLE_COMPOSE_SHADER: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/stage11_nrd_stable_compose.dxil"));
#[cfg(feature = "streamline")]
const DLSS_COMPOSE_SHADER: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/stage10_dlss_input.dxil"));
#[cfg(feature = "streamline-rr")]
const RR_INPUT_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage11_rr_input.dxil"));
#[cfg(feature = "streamline-rr")]
const RR_STABLE_INPUT_SHADER: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/stage11_rr_stable_input.dxil"));
#[cfg(feature = "streamline-rr")]
const RR_EMISSIVE_SHADER: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/stage11_rr_emissive.dxil"));
#[cfg(feature = "streamline-rr")]
const RR_PRIMARY_VISIBILITY_SHADER: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/stage11_rr_primary_visibility.dxil"
));
#[cfg(feature = "streamline-rr")]
const RR_BOUNDARY_RESOLVE_SHADER: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/stage11_rr_boundary_resolve.dxil"
));

const TONEMAP_INPUT_SVGF_SPLIT: u32 = 0;
const TONEMAP_INPUT_NRD_SPLIT: u32 = 1;
const TONEMAP_INPUT_COMPOSED_HDR: u32 = 2;
const TONEMAP_INPUT_RR_HDR: u32 = 3;
const DLSS_GUIDE_MODE_DISABLED: u32 = 0;
const DLSS_GUIDE_MODE_SR: u32 = 1;
const DLSS_GUIDE_MODE_RR: u32 = 2;
// Streamline 2.12's latest transformer. A bounded 121..128 static-frame A/B
// against eDefault and preset D selected E for substantially lower low-frequency
// RR variation. Pinning it also keeps image quality reproducible across OTA.
#[cfg(feature = "streamline-rr")]
const DLSS_RR_RENDER_PRESET: u32 = 5; // DLSSDPreset::ePresetE

fn dlss_guide_mode(dlss_sr_active: bool, rr_active: bool) -> u32 {
    debug_assert!(!(dlss_sr_active && rr_active));
    if rr_active {
        DLSS_GUIDE_MODE_RR
    } else if dlss_sr_active {
        DLSS_GUIDE_MODE_SR
    } else {
        DLSS_GUIDE_MODE_DISABLED
    }
}

fn tonemap_input_mode(denoiser: DenoiserBackend, dlss_active: bool) -> u32 {
    if denoiser == DenoiserBackend::DlssRayReconstruction {
        TONEMAP_INPUT_RR_HDR
    } else if dlss_active {
        TONEMAP_INPUT_COMPOSED_HDR
    } else if denoiser == DenoiserBackend::NrdReblur {
        TONEMAP_INPUT_NRD_SPLIT
    } else {
        TONEMAP_INPUT_SVGF_SPLIT
    }
}

const DXR_TABLE_BASE: usize = 0;
#[cfg(feature = "nrd")]
const NRD_PREP_TABLE_BASE: usize = 319;
#[cfg(feature = "nrd")]
const NRD_TRANSMISSION_PREP_TABLE_BASE: usize = 337;
#[cfg(feature = "nrd")]
const NRD_COMPOSE_TABLE_BASE: usize = 355;
#[cfg(feature = "nrd")]
const NRD_STABLE_PREP_TABLE_BASES: [usize; crate::path_space::STABLE_PLANE_COUNT] = [
    STABLE_BUILD_TABLE_BASE + STABLE_BUILD_DESCRIPTOR_COUNT,
    STABLE_BUILD_TABLE_BASE + STABLE_BUILD_DESCRIPTOR_COUNT + 11,
    STABLE_BUILD_TABLE_BASE + STABLE_BUILD_DESCRIPTOR_COUNT + 22,
];
#[cfg(feature = "nrd")]
const NRD_STABLE_COMPOSE_TABLE_BASES: [usize; crate::path_space::STABLE_PLANE_COUNT] = [
    STABLE_BUILD_TABLE_BASE + STABLE_BUILD_DESCRIPTOR_COUNT + 33,
    STABLE_BUILD_TABLE_BASE + STABLE_BUILD_DESCRIPTOR_COUNT + 41,
    STABLE_BUILD_TABLE_BASE + STABLE_BUILD_DESCRIPTOR_COUNT + 49,
];
const TEMPORAL_TABLE_BASES: [usize; 2] = [173, 201];
const ATROUS_HISTORY_TABLE_BASES: [usize; 2] = [229, 239];
const ATROUS_PING_TO_PONG_BASES: [usize; 2] = [249, 259];
const ATROUS_PONG_TO_PING_BASES: [usize; 2] = [269, 279];
const TONEMAP_TABLE_BASES: [usize; 2] = [289, 304];
#[cfg(feature = "streamline")]
const DLSS_COMPOSE_TABLE_BASES: [usize; 2] = [368, 372];
#[cfg(feature = "streamline")]
const DLSS_TONEMAP_TABLE_BASE: usize = 376;
#[cfg(feature = "streamline-rr")]
const RR_INPUT_TABLE_BASE: usize = 391;
#[cfg(all(feature = "streamline-rr", not(feature = "nrd")))]
const RR_STABLE_INPUT_TABLE_BASE: usize = STABLE_BUILD_TABLE_BASE + STABLE_BUILD_DESCRIPTOR_COUNT;
#[cfg(all(feature = "streamline-rr", feature = "nrd"))]
const RR_STABLE_INPUT_TABLE_BASE: usize =
    STABLE_BUILD_TABLE_BASE + STABLE_BUILD_DESCRIPTOR_COUNT + 57;
#[cfg(feature = "streamline-rr")]
const RR_EMISSIVE_TABLE_BASES: [usize; 2] = [397, 401];
#[cfg(feature = "streamline-rr")]
const RR_TONEMAP_TABLE_BASES: [usize; 2] = [405, 420];
#[cfg(feature = "streamline-rr")]
const RR_PRIMARY_VISIBILITY_TABLE_BASE: usize = 435;
#[cfg(feature = "streamline-rr")]
const RR_PRIMARY_VISIBILITY_TABLE_STRIDE: usize = 8;
#[cfg(feature = "streamline-rr")]
const RR_BOUNDARY_TABLE_BASES: [usize; 2] = [483, 492];

#[cfg(feature = "nrd")]
fn bridge_resource(resource: &TrackedResource) -> reconstruction::NrdBridgeResource {
    reconstruction::NrdBridgeResource {
        resource: resource.resource().as_raw() as usize,
        state: resource.state().0 as u32,
        format: resource.format().0 as u32,
    }
}

#[cfg(feature = "nrd")]
fn bridge_resources_for_layer(
    layer: &NrdDenoiserResources,
    validation_output: reconstruction::NrdBridgeResource,
) -> reconstruction::NrdBridgeResources {
    reconstruction::NrdBridgeResources {
        motion: bridge_resource(&layer.motion),
        normal_roughness: bridge_resource(&layer.normal_roughness),
        view_z: bridge_resource(&layer.view_z),
        diffuse_radiance_hit_distance: bridge_resource(&layer.diffuse_input),
        specular_radiance_hit_distance: bridge_resource(&layer.specular_input),
        diffuse_output: bridge_resource(&layer.diffuse_output),
        specular_output: bridge_resource(&layer.specular_output),
        validation_output,
    }
}

#[cfg(feature = "nrd")]
fn apply_bridge_resource_states(
    layer: &mut NrdDenoiserResources,
    resources: &reconstruction::NrdBridgeResources,
) -> Result<()> {
    set_bridge_resource_state(&mut layer.motion, resources.motion.state)?;
    set_bridge_resource_state(
        &mut layer.normal_roughness,
        resources.normal_roughness.state,
    )?;
    set_bridge_resource_state(&mut layer.view_z, resources.view_z.state)?;
    set_bridge_resource_state(
        &mut layer.diffuse_input,
        resources.diffuse_radiance_hit_distance.state,
    )?;
    set_bridge_resource_state(
        &mut layer.specular_input,
        resources.specular_radiance_hit_distance.state,
    )?;
    set_bridge_resource_state(&mut layer.diffuse_output, resources.diffuse_output.state)?;
    set_bridge_resource_state(&mut layer.specular_output, resources.specular_output.state)?;
    Ok(())
}

#[cfg(feature = "nrd")]
fn set_bridge_resource_state(resource: &mut TrackedResource, state: u32) -> Result<()> {
    let state = match state {
        value if value == D3D12_RESOURCE_STATE_UNORDERED_ACCESS.0 as u32 => {
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS
        }
        value
            if value
                == (D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE
                    | D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE)
                    .0 as u32 =>
        {
            D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE
                | D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE
        }
        value if value == D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE.0 as u32 => {
            D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE
        }
        _ => {
            return Err(WindowsError::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                format!("NRD 返回不支持的 D3D12 资源状态 0x{state:08x}"),
            ));
        }
    };
    resource.set_known_state_after_external_recording(state);
    Ok(())
}

fn active_gpu_passes(
    path: ReconstructionPath,
    dlss_sr_active: bool,
    rr_active: bool,
    rr_boundary_active: bool,
    stable_plane_active: bool,
) -> [bool; profiler::PASS_COUNT] {
    let mut active = [false; profiler::PASS_COUNT];
    for pass in [
        GpuPass::Total,
        GpuPass::AccelerationStructure,
        GpuPass::PathTrace,
        GpuPass::ToneMap,
    ] {
        active[pass as usize] = true;
    }
    match path {
        ReconstructionPath::Svgf => {
            for pass in [
                GpuPass::Temporal,
                GpuPass::Atrous,
                GpuPass::Atrous0,
                GpuPass::Atrous1,
                GpuPass::Atrous2,
                GpuPass::Atrous3,
            ] {
                active[pass as usize] = true;
            }
        }
        ReconstructionPath::NrdReblur => {
            for pass in [GpuPass::NrdPrep, GpuPass::NrdDenoise, GpuPass::NrdCompose] {
                active[pass as usize] = true;
            }
            if stable_plane_active {
                for pass in [
                    GpuPass::NrdStablePrep0,
                    GpuPass::NrdStablePrep1,
                    GpuPass::NrdStablePrep2,
                    GpuPass::NrdStableDenoise0,
                    GpuPass::NrdStableDenoise1,
                    GpuPass::NrdStableDenoise2,
                    GpuPass::NrdStableCompose0,
                    GpuPass::NrdStableCompose1,
                    GpuPass::NrdStableCompose2,
                ] {
                    active[pass as usize] = true;
                }
            }
        }
        ReconstructionPath::DlssRayReconstruction => {
            active[GpuPass::RrInputAdapter as usize] = rr_active;
            active[GpuPass::RrEvaluate as usize] = rr_active;
            active[GpuPass::RrPrimaryVisibility as usize] = rr_boundary_active;
            active[GpuPass::RrBoundaryResolve as usize] = rr_boundary_active;
            active[GpuPass::RrStableMerge as usize] = rr_active && stable_plane_active;
        }
    }
    if stable_plane_active {
        active[GpuPass::StablePlaneBuild as usize] = true;
        active[GpuPass::StablePlaneFill0 as usize] = true;
        active[GpuPass::StablePlaneFill1 as usize] = true;
        active[GpuPass::StablePlaneFill2 as usize] = true;
    }
    if dlss_sr_active {
        active[GpuPass::DlssCompose as usize] = true;
        active[GpuPass::DlssEvaluate as usize] = true;
    }
    active
}

struct FrameContext {
    allocator: ID3D12CommandAllocator,
    fence_value: u64,
    timing_valid: bool,
    timing_generation_id: u64,
    timing_passes: [bool; profiler::PASS_COUNT],
    stable_counter_pending: bool,
    stable_counter_valid: bool,
    stable_counter_generation_id: u64,
    stable_counter_extent: Extent2D,
}

struct CaptureRequest {
    path: PathBuf,
    after_spp: u32,
}

struct PendingCapture {
    readback: ID3D12Resource,
    footprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
    total_bytes: usize,
    width: u32,
    height: u32,
    metadata: CaptureMetadata,
    fence_value: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CameraConstants {
    frame_index: u32,
    position: [f32; 3],
    yaw: f32,
    pitch: f32,
    padding: [f32; 2],
    previous_position: [f32; 3],
    previous_yaw: f32,
    previous_pitch: f32,
    dlss_guide_mode: u32,
    nrd_enabled: u32,
    reset_history: u32,
}

fn camera_constant_words(camera: &CameraConstants) -> [u32; 16] {
    debug_assert_eq!(size_of::<CameraConstants>(), 16 * size_of::<u32>());
    // CameraConstants is the existing 16-DWORD root ABI. The visibility pass
    // deliberately receives the same block so its camera basis stays exactly
    // aligned with the path tracer while it ignores jitter/frame state.
    unsafe { std::ptr::read((camera as *const CameraConstants).cast::<[u32; 16]>()) }
}

fn submit_global_uav_barrier(command_list: &ID3D12GraphicsCommandList) {
    let barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            UAV: ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                pResource: ManuallyDrop::new(None),
            }),
        },
    };
    unsafe { command_list.ResourceBarrier(std::slice::from_ref(&barrier)) };
}

/// 阶段 1 的最小 DX12 后端：三缓冲交换链、清屏和逐帧 Fence。
pub struct Dx12Renderer {
    device: ID3D12Device,
    _adapter: IDXGIAdapter1,
    gpu_name: String,
    command_queue: ID3D12CommandQueue,
    swap_chain: IDXGISwapChain3,
    #[cfg(feature = "streamline")]
    streamline_swap_chain: Option<IDXGISwapChain3>,
    rtv_heap: DescriptorHeap,
    render_targets: [Option<TrackedResource>; FRAME_COUNT],
    active_generation: RenderResourceGeneration,
    retired_generations: VecDeque<RetiredRenderResourceGeneration>,
    resolution_mode: ResolutionMode,
    upscaler: UpscalerMode,
    reflex_mode: ReflexMode,
    #[cfg(feature = "streamline")]
    reflex_token_count: u64,
    #[cfg(feature = "streamline")]
    reflex_sleep_count: u64,
    #[cfg(feature = "streamline")]
    reflex_marker_counts: [u64; 6],
    #[cfg(feature = "streamline")]
    reflex_marker_order_errors: u64,
    #[cfg(feature = "streamline")]
    reflex_expected_marker: u32,
    #[cfg(feature = "streamline")]
    reflex_present_common_count: u64,
    #[cfg(feature = "streamline")]
    benchmark_reflex_token_baseline: u64,
    #[cfg(feature = "streamline")]
    benchmark_reflex_sleep_baseline: u64,
    #[cfg(feature = "streamline")]
    benchmark_reflex_marker_baseline: [u64; 6],
    #[cfg(feature = "streamline")]
    benchmark_reflex_present_common_baseline: u64,
    dynamic_resolution: Option<DynamicResolutionController>,
    resolution_clock: Instant,
    requested_render_scale: RenderScale,
    next_generation_id: u64,
    render_generation_create_count: u64,
    render_generation_switch_count: u64,
    render_generation_retired_count: u64,
    retired_generation_high_watermark: u64,
    render_scale_quantized_noop_count: u64,
    gpu_idle_wait_count: u64,
    gpu_profiler: GpuProfiler,
    stable_plane_counter_readback: ID3D12Resource,
    stable_plane_counter_telemetry: crate::path_space::StablePlaneCounterTelemetry,
    memory_telemetry: VideoMemoryTelemetry,
    shader_status: String,
    raytracing_status: String,
    _textures: TextureSet,
    _sampler_heap: DescriptorHeap,
    _scene_geometry: SceneGeometry,
    _acceleration_structures: AccelerationStructures,
    raytracing_pipeline: RaytracingPipeline,
    temporal_pipeline: ComputePipeline,
    atrous_baseline_pipeline: ComputePipeline,
    atrous_shared_pipeline: ComputePipeline,
    tonemap_pipeline: ComputePipeline,
    stable_plane_build_pipeline: ComputePipeline,
    #[cfg(feature = "streamline")]
    dlss_compose_pipeline: ComputePipeline,
    #[cfg(feature = "streamline-rr")]
    rr_input_pipeline: ComputePipeline,
    #[cfg(feature = "streamline-rr")]
    rr_stable_input_pipeline: ComputePipeline,
    #[cfg(feature = "streamline-rr")]
    rr_emissive_pipeline: ComputePipeline,
    #[cfg(feature = "streamline-rr")]
    rr_primary_visibility_pipeline: ComputePipeline,
    #[cfg(feature = "streamline-rr")]
    rr_boundary_resolve_pipeline: ComputePipeline,
    #[cfg(feature = "nrd")]
    nrd_prep_pipeline: ComputePipeline,
    #[cfg(feature = "nrd")]
    nrd_compose_pipeline: ComputePipeline,
    #[cfg(feature = "nrd")]
    nrd_stable_prep_pipeline: ComputePipeline,
    #[cfg(feature = "nrd")]
    nrd_stable_compose_pipeline: ComputePipeline,
    #[cfg(feature = "streamline")]
    streamline: Option<StreamlineRuntime>,
    #[cfg(feature = "streamline")]
    active_streamline_viewport: Option<StreamlineViewport>,
    #[cfg(feature = "streamline")]
    next_streamline_viewport_id: u32,
    shader_reloader: ShaderReloader,
    frames: Vec<FrameContext>,
    command_list: ID3D12GraphicsCommandList,
    transition_batch: TransitionBatch,
    fence: ID3D12Fence,
    next_fence_value: u64,
    fence_event: HANDLE,
    width: u32,
    height: u32,
    minimized: bool,
    frame_number: u32,
    accumulated_frames: u32,
    history_index: usize,
    reset_history: bool,
    debug_view: DebugView,
    capture_request: Option<CaptureRequest>,
    pending_capture: Option<PendingCapture>,
    capture_result: Option<String>,
    camera_position: [f32; 3],
    camera_yaw: f32,
    camera_pitch: f32,
    reconstruction_frame_state: ReconstructionFrameState,
    last_reconstruction_update: Instant,
    previous_camera_position: [f32; 3],
    previous_camera_yaw: f32,
    previous_camera_pitch: f32,
    animate_model: bool,
    atrous_mode: AtrousMode,
    command_recording_mode: CommandRecordingMode,
    requested_path_space: PathSpaceMode,
    active_path_space: ActivePathSpace,
    denoiser: DenoiserBackend,
    denoiser_switch_count: u64,
    animation_start: Instant,
    history_reset_count: u64,
    render_extent_change_count: u64,
    benchmark_history_reset_baseline: u64,
    benchmark_extent_change_baseline: u64,
    benchmark_generation_create_baseline: u64,
    benchmark_generation_switch_baseline: u64,
    benchmark_generation_retired_baseline: u64,
    benchmark_retired_high_watermark: u64,
    benchmark_gpu_idle_wait_baseline: u64,
    benchmark_render_scale_noop_baseline: u64,
    benchmark_dynamic_valid_samples_baseline: u64,
    benchmark_dynamic_stale_samples_baseline: u64,
    benchmark_dynamic_downscale_baseline: u64,
    benchmark_dynamic_upscale_baseline: u64,
    benchmark_dynamic_at_min_baseline: u64,
    benchmark_dynamic_at_max_baseline: u64,
    benchmark_denoiser_switch_baseline: u64,
    benchmark_render_min: Extent2D,
    benchmark_render_max: Extent2D,
    benchmark_measurement_active: bool,
}

impl Dx12Renderer {
    /// Reads only a fence-complete Frame Context slice. Counter readback is a
    /// diagnostic side channel: a generation/reset mismatch is discarded so
    /// it can never be mistaken for a sample from the current scene epoch.
    fn collect_stable_plane_counters(
        &mut self,
        frame_index: usize,
        fence_completed: bool,
        pending: bool,
        sample_valid: bool,
        generation_id: u64,
        extent: Extent2D,
    ) {
        if !pending || !fence_completed {
            return;
        }
        let counter_bytes = crate::path_space::STABLE_PLANE_COUNTER_COUNT * size_of::<u32>();
        let byte_start = frame_index * counter_bytes;
        let read_range = D3D12_RANGE {
            Begin: byte_start,
            End: byte_start + counter_bytes,
        };
        let mut values = [0_u32; crate::path_space::STABLE_PLANE_COUNTER_COUNT];
        unsafe {
            let mut mapped = std::ptr::null_mut::<c_void>();
            if self
                .stable_plane_counter_readback
                .Map(0, Some(&read_range), Some(&mut mapped))
                .is_ok()
            {
                values.copy_from_slice(std::slice::from_raw_parts(
                    mapped
                        .cast::<u32>()
                        .add(frame_index * crate::path_space::STABLE_PLANE_COUNTER_COUNT),
                    crate::path_space::STABLE_PLANE_COUNTER_COUNT,
                ));
                self.stable_plane_counter_readback
                    .Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
                let snapshot = crate::path_space::StablePlaneCounterSnapshot {
                    generation_id,
                    extent: [extent.width, extent.height],
                    values,
                };
                let _ = self.stable_plane_counter_telemetry.accept(
                    snapshot,
                    self.active_generation.id,
                    [
                        self.active_generation.render_extent.width,
                        self.active_generation.render_extent.height,
                    ],
                    sample_valid,
                );
            }
        }
    }

    pub fn new(window: &Window, width: u32, height: u32, config: &RealtimeConfig) -> Result<Self> {
        if let Some(error) = config.denoiser.requested_startup_error() {
            return Err(WindowsError::new(
                windows::core::HRESULT(0x80070057_u32 as i32),
                error,
            ));
        }
        if let Some(error) = config.upscaler.requested_startup_error() {
            return Err(WindowsError::new(
                windows::core::HRESULT(0x80070057_u32 as i32),
                error,
            ));
        }
        unsafe {
            enable_debug_interfaces();

            #[cfg(feature = "streamline")]
            let streamline_bridge = Some(StreamlineRuntime::create_before_dxgi(
                config.streamline_application_id,
                config.denoiser == DenoiserBackend::DlssRayReconstruction,
                cfg!(feature = "streamline-fg"),
            )?);

            let factory_flags = if cfg!(debug_assertions) {
                DXGI_CREATE_FACTORY_DEBUG
            } else {
                DXGI_CREATE_FACTORY_FLAGS(0)
            };
            let factory: IDXGIFactory6 = match CreateDXGIFactory2(factory_flags) {
                Ok(factory) => factory,
                Err(error) if cfg!(debug_assertions) => {
                    eprintln!("DXGI 调试 Factory 不可用，将使用普通 Factory：{error}");
                    CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))
                        .map_err(|error| dx_error("创建 DXGI Factory", error))?
                }
                Err(error) => return Err(dx_error("创建 DXGI Factory", error)),
            };
            let (device, adapter, gpu_name) =
                create_hardware_device(&factory).map_err(|error| dx_error("创建设备", error))?;
            #[cfg(feature = "streamline")]
            let streamline = if let Some(bridge) = streamline_bridge {
                Some(StreamlineRuntime::attach(
                    bridge,
                    &device,
                    &adapter,
                    config.reflex_mode,
                    true,
                    config.denoiser == DenoiserBackend::DlssRayReconstruction,
                )?)
            } else {
                None
            };
            let memory_telemetry = VideoMemoryTelemetry::new(&adapter);
            configure_info_queue(&device);

            let queue_description = D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                Priority: D3D12_COMMAND_QUEUE_PRIORITY_NORMAL.0,
                Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
                NodeMask: 0,
            };
            let command_queue: ID3D12CommandQueue =
                device
                    .CreateCommandQueue(&queue_description)
                    .map_err(|error| dx_error("创建命令队列", error))?;

            let hwnd = window_hwnd(window)?;
            let swap_chain_description = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: FRAME_COUNT as u32,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_UNSPECIFIED,
                Flags: 0,
            };
            let swap_chain: IDXGISwapChain3 = factory
                .CreateSwapChainForHwnd(&command_queue, hwnd, &swap_chain_description, None, None)
                .map_err(|error| dx_error("创建交换链", error))?
                .cast()
                .map_err(|error| dx_error("获取 IDXGISwapChain3", error))?;
            #[cfg(feature = "streamline")]
            let streamline_swap_chain = if let Some(runtime) = streamline.as_ref() {
                Some(runtime.upgrade_interface(swap_chain.clone())?)
            } else {
                None
            };
            factory
                .MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER)
                .map_err(|error| dx_error("设置窗口关联", error))?;

            let rtv_heap =
                DescriptorHeap::new(&device, D3D12_DESCRIPTOR_HEAP_TYPE_RTV, FRAME_COUNT, false)
                    .map_err(|error| dx_error("创建 RTV 描述符堆", error))?;
            let sampler_heap = DescriptorHeap::new(
                &device,
                D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER,
                MAX_SCENE_SAMPLERS,
                true,
            )
            .map_err(|error| dx_error("创建 sampler 描述符堆", error))?;
            let gpu_profiler = GpuProfiler::new(&device, &command_queue, FRAME_COUNT)
                .map_err(|error| dx_error("创建 GPU 计时器", error))?;
            let stable_plane_counter_readback = create_readback_buffer(
                &device,
                (FRAME_COUNT * crate::path_space::STABLE_PLANE_COUNTER_COUNT * size_of::<u32>())
                    as u64,
            )?;
            let raytracing_status = require_raytracing_tier_1_1(&device)?;
            let mut frames = Vec::with_capacity(FRAME_COUNT);
            for _ in 0..FRAME_COUNT {
                frames.push(FrameContext {
                    allocator: device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?,
                    fence_value: 0,
                    timing_valid: false,
                    timing_generation_id: 0,
                    timing_passes: [false; profiler::PASS_COUNT],
                    stable_counter_pending: false,
                    stable_counter_valid: false,
                    stable_counter_generation_id: 0,
                    stable_counter_extent: Extent2D {
                        width: 0,
                        height: 0,
                    },
                });
            }
            let command_list: ID3D12GraphicsCommandList = device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                &frames[0].allocator,
                None::<&ID3D12PipelineState>,
            )?;
            let mut scene = match config.scene {
                crate::realtime::SceneKind::Cornell => SceneAsset::cornell_box(),
                crate::realtime::SceneKind::NestedDielectric => {
                    SceneAsset::nested_dielectric_fixture()
                }
            };
            if let Some(model_path) = &config.model_path {
                let imported = gltf_loader::load(model_path).map_err(|error| {
                    dx_error(
                        "加载 glTF 场景",
                        WindowsError::new(
                            windows::core::HRESULT(0x80004005_u32 as i32),
                            error.to_string(),
                        ),
                    )
                })?;
                scene.append(imported);
            } else if config.animate_model {
                return Err(WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "--animate-model 需要同时提供 --model <path>",
                ));
            }
            let mut texture_set = TextureSet::new(
                &device,
                &command_list,
                &scene.images,
                &scene.materials,
                &scene.samplers,
            )
            .map_err(|error| dx_error("创建 glTF 纹理资源", error))?;
            texture_set.write_samplers(&device, &sampler_heap);
            let mut scene_geometry =
                SceneGeometry::new(&device, &command_list, &scene, &texture_set)
                    .map_err(|error| dx_error("创建场景网格", error))?;
            let pending_acceleration_structures = AccelerationStructures::build_phase_a(
                &device,
                &command_list,
                &scene_geometry,
                config.acceleration_structure_mode,
                config.animate_model,
            )
            .map_err(|error| dx_error("构建 DXR 加速结构 Phase A", error))?;
            let raytracing_pipeline = RaytracingPipeline::new(&device, STAGE3_SHADER)
                .map_err(|error| dx_error("创建 DXR State Object", error))?;
            let temporal_pipeline =
                ComputePipeline::new(&device, TEMPORAL_SHADER, 18, 10, 1, "阶段 6 时域重投影")
                    .map_err(|error| dx_error("创建时域重投影管线", error))?;
            let atrous_baseline_pipeline =
                ComputePipeline::new(&device, ATROUS_SHADER, 8, 2, 2, "阶段 8 À-Trous Baseline")
                    .map_err(|error| dx_error("创建 À-Trous baseline 管线", error))?;
            let atrous_shared_pipeline = ComputePipeline::new(
                &device,
                ATROUS_SHARED_SHADER,
                8,
                2,
                2,
                "阶段 8 À-Trous Shared 1/2",
            )
            .map_err(|error| dx_error("创建 À-Trous shared 管线", error))?;
            let tonemap_pipeline =
                ComputePipeline::new(&device, TONEMAP_SHADER, 14, 1, 3, "Tone Map 与调试视图")
                    .map_err(|error| dx_error("创建 Tone Map 管线", error))?;
            let stable_plane_build_pipeline = ComputePipeline::new_with_root_srv(
                &device,
                STABLE_PLANE_BUILD_SHADER,
                4,
                6,
                16,
                4,
                "阶段 11 RTXPT-style BuildStablePlanes",
            )
            .map_err(|error| dx_error("创建 stable-plane build 管线", error))?;
            #[cfg(feature = "streamline")]
            let dlss_compose_pipeline = ComputePipeline::new(
                &device,
                DLSS_COMPOSE_SHADER,
                2,
                2,
                1,
                "阶段 10 DLSS HDR 合成",
            )
            .map_err(|error| dx_error("创建 DLSS HDR 合成管线", error))?;
            #[cfg(feature = "streamline-rr")]
            let rr_input_pipeline = ComputePipeline::new(
                &device,
                RR_INPUT_SHADER,
                3,
                3,
                1,
                "阶段 11 DLSS RR 输入分层与 guide 适配",
            )
            .map_err(|error| dx_error("创建 DLSS RR 输入适配管线", error))?;
            #[cfg(feature = "streamline-rr")]
            let rr_stable_input_pipeline = ComputePipeline::new(
                &device,
                RR_STABLE_INPUT_SHADER,
                7,
                6,
                1,
                "阶段 11 stable-plane DLSS RR 单次合并输入",
            )
            .map_err(|error| dx_error("创建 stable-plane DLSS RR 输入管线", error))?;
            #[cfg(feature = "streamline-rr")]
            let rr_emissive_pipeline = ComputePipeline::new(
                &device,
                RR_EMISSIVE_SHADER,
                3,
                1,
                3,
                "阶段 11 DLSS RR primary emissive 稳定层",
            )
            .map_err(|error| dx_error("创建 DLSS RR emissive 稳定管线", error))?;
            #[cfg(feature = "streamline-rr")]
            let rr_primary_visibility_pipeline = ComputePipeline::new(
                &device,
                RR_PRIMARY_VISIBILITY_SHADER,
                5,
                3,
                16,
                "阶段 11 RR stable physical and virtual visibility",
            )
            .map_err(|error| dx_error("创建 DLSS RR primary visibility 管线", error))?;
            #[cfg(feature = "streamline-rr")]
            let rr_boundary_resolve_pipeline = ComputePipeline::new(
                &device,
                RR_BOUNDARY_RESOLVE_SHADER,
                7,
                2,
                1,
                "阶段 11 RR opaque boundary resolve",
            )
            .map_err(|error| dx_error("创建 DLSS RR boundary resolve 管线", error))?;
            #[cfg(feature = "nrd")]
            let nrd_prep_pipeline = ComputePipeline::new(
                &device,
                NRD_PREP_SHADER,
                11,
                7,
                3,
                "阶段 9 NRD REBLUR 输入准备",
            )
            .map_err(|error| dx_error("创建 NRD 输入准备管线", error))?;
            #[cfg(feature = "nrd")]
            let nrd_compose_pipeline = ComputePipeline::new(
                &device,
                NRD_COMPOSE_SHADER,
                11,
                2,
                1,
                "阶段 9 NRD REBLUR 输出合成",
            )
            .map_err(|error| dx_error("创建 NRD 输出合成管线", error))?;
            #[cfg(feature = "nrd")]
            let nrd_stable_prep_pipeline = ComputePipeline::new(
                &device,
                NRD_STABLE_PREP_SHADER,
                4,
                7,
                1,
                "阶段 11 stable-plane NRD 输入准备",
            )
            .map_err(|error| dx_error("创建 stable-plane NRD 输入准备管线", error))?;
            #[cfg(feature = "nrd")]
            let nrd_stable_compose_pipeline = ComputePipeline::new(
                &device,
                NRD_STABLE_COMPOSE_SHADER,
                6,
                2,
                2,
                "阶段 11 stable-plane NRD 反向合成",
            )
            .map_err(|error| dx_error("创建 stable-plane NRD 合成管线", error))?;
            command_list.Close()?;

            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let fence_event = CreateEventW(None, false, false, None)?;
            let initialization_list: ID3D12CommandList = command_list.cast()?;
            command_queue.ExecuteCommandLists(&[Some(initialization_list)]);
            command_queue.Signal(&fence, 1)?;
            fence.SetEventOnCompletion(1, fence_event)?;
            WaitForSingleObject(fence_event, INFINITE);
            frames[0].allocator.Reset()?;
            command_list.Reset(&frames[0].allocator, None::<&ID3D12PipelineState>)?;
            let mut acceleration_structures = pending_acceleration_structures
                .finish_phase_b(&device, &command_list, &scene_geometry)
                .map_err(|error| dx_error("构建 DXR 加速结构 Phase B", error))?;
            command_list.Close()?;
            let phase_b_list: ID3D12CommandList = command_list.cast()?;
            command_queue.ExecuteCommandLists(&[Some(phase_b_list)]);
            command_queue.Signal(&fence, 2)?;
            fence.SetEventOnCompletion(2, fence_event)?;
            WaitForSingleObject(fence_event, INFINITE);
            scene_geometry.release_uploads();
            texture_set.release_uploads();
            let as_stats = acceleration_structures.stats();
            eprintln!(
                "DXR AS：{}，BLAS {}/{} compacted，allocation {} -> {} KiB，TLAS update={}，retained scratch={} B required / {} KiB allocation",
                as_stats.mode.as_str(),
                as_stats.compacted_blas_count,
                as_stats.blas_count,
                as_stats.original_allocation_bytes / 1024,
                as_stats.final_allocation_bytes / 1024,
                as_stats.tlas_update_enabled,
                as_stats.retained_update_scratch_required_bytes,
                as_stats.retained_update_scratch_allocation_bytes / 1024,
            );
            acceleration_structures.release_build_resources();
            let output_extent = Extent2D { width, height };
            let initial_scale = match config.resolution_mode {
                ResolutionMode::Fixed(scale) => scale,
                ResolutionMode::Dynamic(_) => RenderScale::NATIVE,
            };
            #[cfg(feature = "streamline")]
            let mut next_streamline_viewport_id = 1;
            #[cfg(feature = "streamline")]
            let active_streamline_viewport = if config.denoiser
                == DenoiserBackend::DlssRayReconstruction
                || config.upscaler.uses_streamline()
            {
                let streamline = streamline.as_ref().ok_or_else(|| {
                    streamline_error("DLSS runtime", crate::streamline::STATUS_NOT_INITIALIZED)
                })?;
                let viewport = streamline.create_reconstruction_viewport(
                    config.denoiser,
                    config.upscaler,
                    output_extent,
                    next_streamline_viewport_id,
                )?;
                next_streamline_viewport_id = next_streamline_viewport_id.saturating_add(1);
                Some(viewport)
            } else {
                None
            };
            #[cfg(feature = "streamline")]
            let render_extent = active_streamline_viewport.as_ref().map_or_else(
                || render_extent(output_extent, initial_scale),
                StreamlineViewport::optimal_extent,
            );
            #[cfg(not(feature = "streamline"))]
            let render_extent = render_extent(output_extent, initial_scale);
            let initial_reconstruction_frame_state =
                ReconstructionFrameState::from_camera(ReconstructionFrameInput {
                    current_camera: CameraPose {
                        position: [0.0, 0.0, -2.666_666_7],
                        yaw: 0.0,
                        pitch: 0.0,
                    },
                    previous_camera: CameraPose {
                        position: [0.0, 0.0, -2.666_666_7],
                        yaw: 0.0,
                        pitch: 0.0,
                    },
                    render_extent: [render_extent.width, render_extent.height],
                    previous_render_extent: [render_extent.width, render_extent.height],
                    frame_index: 0,
                    reset: true,
                    delta_time_ms: 0.0,
                });
            let active_path_space = resolve_path_space(config.path_space_mode, config.denoiser);
            let active_generation = RenderResourceGeneration::new(
                &device,
                &texture_set,
                &scene_geometry,
                &acceleration_structures,
                RenderGenerationDesc {
                    output_extent,
                    render_extent,
                    id: 1,
                    with_nrd: config.denoiser == DenoiserBackend::NrdReblur,
                    with_dlss_sr: !config.upscaler.is_native()
                        && config.denoiser != DenoiserBackend::DlssRayReconstruction,
                    with_dlss_rr: config.denoiser == DenoiserBackend::DlssRayReconstruction,
                    with_stable_planes: active_path_space.uses_stable_planes(),
                },
            )
            .map_err(|error| dx_error("创建初始渲染资源代际", error))?;
            let mut renderer = Self {
                device,
                _adapter: adapter,
                gpu_name,
                command_queue,
                swap_chain,
                #[cfg(feature = "streamline")]
                streamline_swap_chain,
                rtv_heap,
                render_targets: [None, None, None],
                active_generation,
                retired_generations: VecDeque::new(),
                resolution_mode: config.resolution_mode,
                upscaler: config.upscaler,
                reflex_mode: config.reflex_mode,
                #[cfg(feature = "streamline")]
                reflex_token_count: 0,
                #[cfg(feature = "streamline")]
                reflex_sleep_count: 0,
                #[cfg(feature = "streamline")]
                reflex_marker_counts: [0; 6],
                #[cfg(feature = "streamline")]
                reflex_marker_order_errors: 0,
                #[cfg(feature = "streamline")]
                reflex_expected_marker: PCL_SIMULATION_START,
                #[cfg(feature = "streamline")]
                reflex_present_common_count: 0,
                #[cfg(feature = "streamline")]
                benchmark_reflex_token_baseline: 0,
                #[cfg(feature = "streamline")]
                benchmark_reflex_sleep_baseline: 0,
                #[cfg(feature = "streamline")]
                benchmark_reflex_marker_baseline: [0; 6],
                #[cfg(feature = "streamline")]
                benchmark_reflex_present_common_baseline: 0,
                dynamic_resolution: match config.resolution_mode {
                    ResolutionMode::Fixed(_) => None,
                    ResolutionMode::Dynamic(dynamic_config) => {
                        Some(DynamicResolutionController::new(dynamic_config))
                    }
                },
                resolution_clock: Instant::now(),
                requested_render_scale: initial_scale,
                next_generation_id: 2,
                render_generation_create_count: 1,
                render_generation_switch_count: 0,
                render_generation_retired_count: 0,
                retired_generation_high_watermark: 0,
                render_scale_quantized_noop_count: 0,
                gpu_idle_wait_count: 0,
                gpu_profiler,
                stable_plane_counter_readback,
                stable_plane_counter_telemetry:
                    crate::path_space::StablePlaneCounterTelemetry::default(),
                memory_telemetry,
                shader_status: format!(
                    "DXR/Temporal/À-Trous（{} KiB，scene={}）",
                    (STAGE3_SHADER.len()
                        + TEMPORAL_SHADER.len()
                        + ATROUS_SHADER.len()
                        + ATROUS_SHARED_SHADER.len()
                        + TONEMAP_SHADER.len())
                        / 1024,
                    config.scene.as_str(),
                ),
                raytracing_status,
                _textures: texture_set,
                _sampler_heap: sampler_heap,
                _scene_geometry: scene_geometry,
                _acceleration_structures: acceleration_structures,
                raytracing_pipeline,
                temporal_pipeline,
                atrous_baseline_pipeline,
                atrous_shared_pipeline,
                tonemap_pipeline,
                stable_plane_build_pipeline,
                #[cfg(feature = "streamline")]
                dlss_compose_pipeline,
                #[cfg(feature = "streamline-rr")]
                rr_input_pipeline,
                #[cfg(feature = "streamline-rr")]
                rr_stable_input_pipeline,
                #[cfg(feature = "streamline-rr")]
                rr_emissive_pipeline,
                #[cfg(feature = "streamline-rr")]
                rr_primary_visibility_pipeline,
                #[cfg(feature = "streamline-rr")]
                rr_boundary_resolve_pipeline,
                #[cfg(feature = "nrd")]
                nrd_prep_pipeline,
                #[cfg(feature = "nrd")]
                nrd_compose_pipeline,
                #[cfg(feature = "nrd")]
                nrd_stable_prep_pipeline,
                #[cfg(feature = "nrd")]
                nrd_stable_compose_pipeline,
                #[cfg(feature = "streamline")]
                streamline,
                #[cfg(feature = "streamline")]
                active_streamline_viewport,
                #[cfg(feature = "streamline")]
                next_streamline_viewport_id,
                shader_reloader: ShaderReloader::new(),
                frames,
                command_list,
                transition_batch: TransitionBatch::with_capacity(64),
                fence,
                next_fence_value: 3,
                fence_event,
                width,
                height,
                minimized: false,
                frame_number: 0,
                accumulated_frames: 0,
                history_index: 0,
                reset_history: true,
                debug_view: config.debug_view,
                capture_request: config.capture_output.as_ref().map(|path| CaptureRequest {
                    path: path.clone(),
                    after_spp: config.capture_after_spp.unwrap_or(128),
                }),
                pending_capture: None,
                capture_result: None,
                camera_position: [0.0, 0.0, -2.666_666_7],
                camera_yaw: 0.0,
                camera_pitch: 0.0,
                reconstruction_frame_state: initial_reconstruction_frame_state,
                last_reconstruction_update: Instant::now(),
                previous_camera_position: [0.0, 0.0, -2.666_666_7],
                previous_camera_yaw: 0.0,
                previous_camera_pitch: 0.0,
                animate_model: config.animate_model,
                atrous_mode: config.atrous_mode,
                command_recording_mode: config.command_recording_mode,
                requested_path_space: config.path_space_mode,
                active_path_space,
                denoiser: config.denoiser,
                denoiser_switch_count: 0,
                animation_start: Instant::now(),
                history_reset_count: 1,
                render_extent_change_count: 0,
                benchmark_history_reset_baseline: 1,
                benchmark_extent_change_baseline: 0,
                benchmark_generation_create_baseline: 1,
                benchmark_generation_switch_baseline: 0,
                benchmark_generation_retired_baseline: 0,
                benchmark_retired_high_watermark: 0,
                benchmark_gpu_idle_wait_baseline: 0,
                benchmark_render_scale_noop_baseline: 0,
                benchmark_dynamic_valid_samples_baseline: 0,
                benchmark_dynamic_stale_samples_baseline: 0,
                benchmark_dynamic_downscale_baseline: 0,
                benchmark_dynamic_upscale_baseline: 0,
                benchmark_dynamic_at_min_baseline: 0,
                benchmark_dynamic_at_max_baseline: 0,
                benchmark_denoiser_switch_baseline: 0,
                benchmark_render_min: render_extent,
                benchmark_render_max: render_extent,
                benchmark_measurement_active: false,
            };
            renderer
                .create_render_targets()
                .map_err(|error| dx_error("创建交换链渲染目标", error))?;
            Ok(renderer)
        }
    }

    pub fn render(&mut self) -> Result<()> {
        self.poll_pending_capture()?;
        // The capture fence must be the newest submitted fence when the
        // application drops the renderer after writing the PNG. Once a
        // capture is pending, poll it without submitting later frames that
        // could still reference renderer-owned resources at exit.
        if self.pending_capture.is_some() || self.capture_result.is_some() {
            return Ok(());
        }
        self.poll_shader_reload();
        if self.minimized || self.width == 0 || self.height == 0 {
            return Ok(());
        }
        self.memory_telemetry.poll(false);

        unsafe {
            let frame_index = self.active_swap_chain().GetCurrentBackBufferIndex() as usize;
            let previous_fence_value = self.frames[frame_index].fence_value;
            let previous_timing_valid = self.frames[frame_index].timing_valid;
            let previous_timing_generation_id = self.frames[frame_index].timing_generation_id;
            let previous_timing_passes = self.frames[frame_index].timing_passes;
            let previous_counter_pending = self.frames[frame_index].stable_counter_pending;
            let previous_counter_valid = self.frames[frame_index].stable_counter_valid;
            let previous_counter_generation_id =
                self.frames[frame_index].stable_counter_generation_id;
            let previous_counter_extent = self.frames[frame_index].stable_counter_extent;
            self.wait_for_frame(frame_index)?;
            let fence_completed =
                previous_fence_value != 0 && self.fence.GetCompletedValue() >= previous_fence_value;
            let timing_sample = self.gpu_profiler.collect(
                frame_index,
                fence_completed,
                previous_timing_valid,
                previous_timing_passes,
            )?;
            self.collect_stable_plane_counters(
                frame_index,
                fence_completed,
                previous_counter_pending,
                previous_counter_valid,
                previous_counter_generation_id,
                previous_counter_extent,
            );
            self.frames[frame_index].stable_counter_pending = false;
            if let Some(sample) = timing_sample {
                self.apply_dynamic_resolution_sample(
                    sample.total_ms,
                    previous_timing_generation_id == self.active_generation.id,
                )?;
            }
            self.reclaim_retired_generations();
            #[cfg(feature = "streamline")]
            let frame_token = self.begin_reflex_frame()?;
            let output_extent = self.active_generation.output_extent;
            let render_extent = self.active_generation.render_extent;
            let previous_render_extent = if self.reset_history {
                render_extent
            } else {
                Extent2D {
                    width: self.reconstruction_frame_state.render_width,
                    height: self.reconstruction_frame_state.render_height,
                }
            };
            let now = Instant::now();
            let delta_time_ms = now
                .duration_since(self.last_reconstruction_update)
                .as_secs_f32()
                * 1_000.0;
            self.last_reconstruction_update = now;
            self.reconstruction_frame_state =
                ReconstructionFrameState::from_camera(ReconstructionFrameInput {
                    current_camera: CameraPose {
                        position: self.camera_position,
                        yaw: self.camera_yaw,
                        pitch: self.camera_pitch,
                    },
                    previous_camera: CameraPose {
                        position: self.previous_camera_position,
                        yaw: self.previous_camera_yaw,
                        pitch: self.previous_camera_pitch,
                    },
                    render_extent: [render_extent.width, render_extent.height],
                    previous_render_extent: [
                        previous_render_extent.width,
                        previous_render_extent.height,
                    ],
                    frame_index: self.frame_number,
                    reset: self.reset_history,
                    delta_time_ms,
                });
            let rr_path = self.denoiser == DenoiserBackend::DlssRayReconstruction;
            let dlss_active =
                !rr_path && self.upscaler.uses_streamline() && self.debug_view == DebugView::Final;
            // RR guide views must inspect the exact frame contract consumed by
            // the plugin. Keep RR evaluation and its projection jitter active
            // for non-final debug views instead of silently falling back to an
            // unrelated native path.
            let rr_active = rr_path;
            debug_assert_eq!(
                self.active_generation.stable_planes.is_some(),
                self.active_path_space.uses_stable_planes(),
                "active path-space must match its render generation resources"
            );
            #[cfg(feature = "streamline")]
            let dlss_frame_input = if dlss_active || rr_active {
                Some(DlssFrameInput::from_cameras(
                    CameraPose {
                        position: self.camera_position,
                        yaw: self.camera_yaw,
                        pitch: self.camera_pitch,
                    },
                    CameraPose {
                        position: self.previous_camera_position,
                        yaw: self.previous_camera_yaw,
                        pitch: self.previous_camera_pitch,
                    },
                    render_extent,
                    previous_render_extent,
                    output_extent,
                    self.frame_number,
                    self.reset_history,
                ))
            } else {
                None
            };
            let acceleration_dirty = self
                ._scene_geometry
                .prepare_animation(self.animation_start.elapsed(), self.animate_model);
            #[cfg(feature = "streamline")]
            if let Some(token) = frame_token.as_ref() {
                self.submit_pcl_marker(token, PCL_SIMULATION_END)?;
            }
            let command_recording_started = Instant::now();
            let mut command_recording_stats = CommandRecordingFrameStats::default();
            let frame = &self.frames[frame_index];
            frame.allocator.Reset()?;
            self.command_list
                .Reset(&frame.allocator, None::<&ID3D12PipelineState>)?;

            #[cfg(feature = "streamline")]
            if let Some(token) = frame_token.as_ref() {
                self.submit_pcl_marker(token, PCL_RENDER_SUBMIT_START)?;
            }

            #[cfg(feature = "streamline")]
            let dlss_token = if let (Some(streamline), Some(viewport), Some(input)) = (
                self.streamline.as_ref(),
                self.active_streamline_viewport.as_mut(),
                dlss_frame_input.as_ref(),
            ) {
                let token = frame_token.as_ref().ok_or_else(|| {
                    streamline_error(
                        "DLSS frame token",
                        crate::streamline::STATUS_NOT_INITIALIZED,
                    )
                })?;
                Some(streamline.begin_reconstruction_frame(
                    self.denoiser,
                    viewport,
                    token,
                    input,
                    CameraPose {
                        position: self.camera_position,
                        yaw: self.camera_yaw,
                        pitch: self.camera_pitch,
                    },
                )?)
            } else {
                None
            };

            self.gpu_profiler
                .begin(&self.command_list, frame_index, GpuPass::Total);
            self.gpu_profiler
                .begin_event(&self.command_list, GpuPass::Total);
            self.gpu_profiler.begin(
                &self.command_list,
                frame_index,
                GpuPass::AccelerationStructure,
            );
            self.gpu_profiler
                .begin_event(&self.command_list, GpuPass::AccelerationStructure);
            if acceleration_dirty {
                self._acceleration_structures.update(
                    &self.command_list,
                    frame_index,
                    &self._scene_geometry,
                )?;
            }
            self.gpu_profiler.end(
                &self.command_list,
                frame_index,
                GpuPass::AccelerationStructure,
            );
            self.gpu_profiler.end_event(&self.command_list);

            self.command_list.SetDescriptorHeaps(&[
                Some(self.active_generation.shader_heap.heap().clone()),
                Some(self._sampler_heap.heap().clone()),
            ]);
            let command_list4: ID3D12GraphicsCommandList4 = self.command_list.cast()?;
            self.gpu_profiler
                .begin(&self.command_list, frame_index, GpuPass::PathTrace);
            self.gpu_profiler
                .begin_event(&self.command_list, GpuPass::PathTrace);
            self.collect_frame_input_transitions(D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.submit_transition_batch(&mut command_recording_stats);
            command_list4.SetComputeRootSignature(&self.raytracing_pipeline.root_signature);
            command_list4.SetComputeRootDescriptorTable(
                0,
                self.active_generation
                    .shader_heap
                    .gpu_handle(DXR_TABLE_BASE),
            );
            command_list4.SetComputeRootDescriptorTable(3, self._sampler_heap.gpu_handle(0));
            command_list4.SetComputeRootShaderResourceView(
                2,
                self._acceleration_structures
                    .instance_gpu_address(frame_index),
            );
            #[cfg(feature = "streamline")]
            let camera_jitter_px = dlss_frame_input
                .as_ref()
                .map_or([0.0; 2], |input| input.jitter_px);
            #[cfg(not(feature = "streamline"))]
            let camera_jitter_px = [0.0; 2];
            let camera = CameraConstants {
                frame_index: self.frame_number,
                position: self.camera_position,
                yaw: self.camera_yaw,
                pitch: self.camera_pitch,
                padding: camera_jitter_px,
                previous_position: self.previous_camera_position,
                previous_yaw: self.previous_camera_yaw,
                previous_pitch: self.previous_camera_pitch,
                // The DXR pass writes the dense DLSS guides for both SR and
                // RR. RR consumes the same direction/mvecScale contract, but
                // never enters the DLSS SR compose path.
                // RR consumes the same dense primary guides as SR, but also
                // requires the path shader to export a hit distance from the
                // actual sampled specular lobe. Preserve that distinction in
                // the existing 16-DWORD root-constant ABI.
                dlss_guide_mode: dlss_guide_mode(dlss_active, rr_active),
                nrd_enabled: u32::from(self.denoiser == DenoiserBackend::NrdReblur),
                reset_history: u32::from(self.reset_history),
            };
            debug_assert_eq!(size_of::<CameraConstants>(), 16 * size_of::<u32>());
            command_list4.SetComputeRoot32BitConstants(
                1,
                16,
                (&camera as *const CameraConstants).cast(),
                0,
            );

            // RTXPT-style stable planes use two separate phases. The compute
            // pass first follows only deterministic delta chains and records
            // restart points. Each DXR fill then resumes one recorded branch.
            // A stable NRD/RR consumer skips the monolithic raygen below.
            // Explicit stable-planes + SVGF still runs it after this pass so
            // the stable resources remain diagnostic-only for that override.
            if self.active_path_space.uses_stable_planes() {
                self.command_list.ClearUnorderedAccessViewUint(
                    self.active_generation
                        .shader_heap
                        .gpu_handle(STABLE_BUILD_TABLE_BASE + 9),
                    self.active_generation
                        .shader_heap
                        .cpu_handle(STABLE_BUILD_TABLE_BASE + 9),
                    self.active_generation
                        .stable_planes
                        .as_ref()
                        .expect("stable-plane generation validated above")
                        .counters
                        .resource(),
                    &[0, 0, 0, 0],
                    &[],
                );
                submit_global_uav_barrier(&self.command_list);
                let camera_words = camera_constant_words(&camera);
                self.stable_plane_build_pipeline.bind(
                    &self.command_list,
                    self.active_generation
                        .shader_heap
                        .gpu_handle(STABLE_BUILD_TABLE_BASE),
                    &camera_words,
                );
                self.stable_plane_build_pipeline
                    .set_root_shader_resource_view(
                        &self.command_list,
                        self._acceleration_structures
                            .instance_gpu_address(frame_index),
                    );
                self.gpu_profiler
                    .begin(&self.command_list, frame_index, GpuPass::StablePlaneBuild);
                self.gpu_profiler
                    .begin_event(&self.command_list, GpuPass::StablePlaneBuild);
                self.command_list.Dispatch(
                    render_extent.width.div_ceil(8),
                    render_extent.height.div_ceil(8),
                    1,
                );
                self.gpu_profiler
                    .end(&self.command_list, frame_index, GpuPass::StablePlaneBuild);
                self.gpu_profiler.end_event(&self.command_list);
                submit_global_uav_barrier(&self.command_list);

                let counter_resource = self
                    .active_generation
                    .stable_planes
                    .as_mut()
                    .expect("stable-plane generation validated above")
                    .counters
                    .resource()
                    .clone();
                self.active_generation
                    .stable_planes
                    .as_mut()
                    .expect("stable-plane generation validated above")
                    .counters
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                    );
                self.submit_transition_batch(&mut command_recording_stats);
                self.command_list.CopyBufferRegion(
                    &self.stable_plane_counter_readback,
                    (frame_index * crate::path_space::STABLE_PLANE_COUNTER_COUNT * size_of::<u32>())
                        as u64,
                    &counter_resource,
                    0,
                    (crate::path_space::STABLE_PLANE_COUNTER_COUNT * size_of::<u32>()) as u64,
                );
                self.active_generation
                    .stable_planes
                    .as_mut()
                    .expect("stable-plane generation validated above")
                    .counters
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                self.submit_transition_batch(&mut command_recording_stats);

                // A compute pipeline owns a different root signature, so all
                // DXR root arguments must be rebound before replaying planes.
                command_list4.SetComputeRootSignature(&self.raytracing_pipeline.root_signature);
                command_list4.SetComputeRootDescriptorTable(
                    0,
                    self.active_generation
                        .shader_heap
                        .gpu_handle(DXR_TABLE_BASE),
                );
                command_list4.SetComputeRoot32BitConstants(
                    1,
                    camera_words.len() as u32,
                    camera_words.as_ptr().cast(),
                    0,
                );
                command_list4.SetComputeRootShaderResourceView(
                    2,
                    self._acceleration_structures
                        .instance_gpu_address(frame_index),
                );
                command_list4.SetComputeRootDescriptorTable(3, self._sampler_heap.gpu_handle(0));
                command_list4.SetPipelineState1(&self.raytracing_pipeline.state_object);

                // Reverse plane order matches the later denoiser/compositor
                // contract: dependent branches are resolved before plane 0.
                for plane_index in (0..crate::path_space::STABLE_PLANE_COUNT).rev() {
                    let fill_pass = match plane_index {
                        0 => GpuPass::StablePlaneFill0,
                        1 => GpuPass::StablePlaneFill1,
                        _ => GpuPass::StablePlaneFill2,
                    };
                    self.gpu_profiler
                        .begin(&self.command_list, frame_index, fill_pass);
                    self.gpu_profiler.begin_event(&self.command_list, fill_pass);
                    let pass_constants = [1u32, plane_index as u32];
                    command_list4.SetComputeRoot32BitConstants(
                        4,
                        pass_constants.len() as u32,
                        pass_constants.as_ptr().cast(),
                        0,
                    );
                    let fill_dispatch = D3D12_DISPATCH_RAYS_DESC {
                        RayGenerationShaderRecord: self.raytracing_pipeline.stable_fill_raygen,
                        MissShaderTable: self.raytracing_pipeline.miss,
                        HitGroupTable: self.raytracing_pipeline.hit_group,
                        CallableShaderTable: D3D12_GPU_VIRTUAL_ADDRESS_RANGE_AND_STRIDE::default(),
                        Width: render_extent.width,
                        Height: render_extent.height,
                        Depth: 1,
                    };
                    command_list4.DispatchRays(&fill_dispatch);
                    self.gpu_profiler
                        .end(&self.command_list, frame_index, fill_pass);
                    self.gpu_profiler.end_event(&self.command_list);
                    submit_global_uav_barrier(&self.command_list);
                }
            }

            // Pass zero is the existing monolithic path tracer. Explicitly
            // initialize b1 even in legacy mode; relying on stale root data
            // would make hot reload and mode switches nondeterministic.
            let legacy_pass_constants = [0u32, 0u32];
            command_list4.SetComputeRoot32BitConstants(
                4,
                legacy_pass_constants.len() as u32,
                legacy_pass_constants.as_ptr().cast(),
                0,
            );
            command_list4.SetPipelineState1(&self.raytracing_pipeline.state_object);
            let stable_consumer_active = self.active_path_space.uses_stable_planes()
                && matches!(
                    self.denoiser,
                    DenoiserBackend::NrdReblur | DenoiserBackend::DlssRayReconstruction
                );
            if !stable_consumer_active {
                let dispatch = D3D12_DISPATCH_RAYS_DESC {
                    RayGenerationShaderRecord: self.raytracing_pipeline.raygen,
                    MissShaderTable: self.raytracing_pipeline.miss,
                    HitGroupTable: self.raytracing_pipeline.hit_group,
                    CallableShaderTable: D3D12_GPU_VIRTUAL_ADDRESS_RANGE_AND_STRIDE::default(),
                    Width: render_extent.width,
                    Height: render_extent.height,
                    Depth: 1,
                };
                command_list4.DispatchRays(&dispatch);
            }
            self.gpu_profiler
                .end(&self.command_list, frame_index, GpuPass::PathTrace);
            self.gpu_profiler.end_event(&self.command_list);

            let current_history = self.history_index;
            let previous_history = 1 - current_history;
            let render_groups_x = render_extent.width.div_ceil(8);
            let render_groups_y = render_extent.height.div_ceil(8);
            let output_groups_x = output_extent.width.div_ceil(8);
            let output_groups_y = output_extent.height.div_ceil(8);
            #[cfg(feature = "streamline-rr")]
            if rr_path && self.active_path_space == ActivePathSpace::Legacy {
                {
                    let rr = self
                        .active_generation
                        .rr
                        .as_mut()
                        .expect("DLSS RR generation validated above");
                    rr.primary_surface_id[current_history].collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                    rr.primary_surface_meta[current_history].collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                    rr.primary_motion.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                }
                self.submit_transition_batch(&mut command_recording_stats);

                self.gpu_profiler.begin(
                    &self.command_list,
                    frame_index,
                    GpuPass::RrPrimaryVisibility,
                );
                self.gpu_profiler
                    .begin_event(&self.command_list, GpuPass::RrPrimaryVisibility);
                let visibility_table = RR_PRIMARY_VISIBILITY_TABLE_BASE
                    + (frame_index % FRAME_COUNT * 2 + current_history)
                        * RR_PRIMARY_VISIBILITY_TABLE_STRIDE;
                let camera_words = camera_constant_words(&camera);
                self.rr_primary_visibility_pipeline.bind(
                    &self.command_list,
                    self.active_generation
                        .shader_heap
                        .gpu_handle(visibility_table),
                    &camera_words,
                );
                self.command_list
                    .Dispatch(output_groups_x, output_groups_y, 1);
                self.gpu_profiler.end(
                    &self.command_list,
                    frame_index,
                    GpuPass::RrPrimaryVisibility,
                );
                self.gpu_profiler.end_event(&self.command_list);
                let rr = self
                    .active_generation
                    .rr
                    .as_mut()
                    .expect("DLSS RR generation validated above");
                rr.primary_surface_id[current_history].collect_transition(
                    &mut self.transition_batch,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                );
                rr.primary_surface_meta[current_history].collect_transition(
                    &mut self.transition_batch,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                );
                rr.primary_motion.collect_transition(
                    &mut self.transition_batch,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                );
                self.submit_transition_batch(&mut command_recording_stats);
            }
            #[cfg(feature = "nrd")]
            let use_nrd = self.denoiser == DenoiserBackend::NrdReblur;
            #[cfg(not(feature = "nrd"))]
            let use_nrd = false;
            if use_nrd {
                #[cfg(feature = "nrd")]
                self.record_nrd_path(
                    frame_index,
                    render_groups_x,
                    render_groups_y,
                    &mut command_recording_stats,
                )?;
            } else if rr_path {
                // RR is fused: its noisy HDR and guides are consumed below.
                // Do not populate SVGF histories or NRD resources on this path.
            } else {
                self.collect_frame_input_transitions(
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                );
                self.active_generation.histories[previous_history].collect_all(
                    &mut self.transition_batch,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                );
                self.active_generation.histories[current_history].collect_all(
                    &mut self.transition_batch,
                    D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                );
                self.active_generation.rejection_mask.collect_transition(
                    &mut self.transition_batch,
                    D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                );
                self.submit_transition_batch(&mut command_recording_stats);

                self.gpu_profiler
                    .begin(&self.command_list, frame_index, GpuPass::Temporal);
                self.gpu_profiler
                    .begin_event(&self.command_list, GpuPass::Temporal);
                self.temporal_pipeline.bind(
                    &self.command_list,
                    self.active_generation
                        .shader_heap
                        .gpu_handle(TEMPORAL_TABLE_BASES[current_history]),
                    &[u32::from(self.reset_history)],
                );
                self.command_list
                    .Dispatch(render_groups_x, render_groups_y, 1);
                self.gpu_profiler
                    .end(&self.command_list, frame_index, GpuPass::Temporal);
                self.gpu_profiler.end_event(&self.command_list);
                self.active_generation.histories[current_history].collect_all(
                    &mut self.transition_batch,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                );
                self.active_generation.rejection_mask.collect_transition(
                    &mut self.transition_batch,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                );
                self.submit_transition_batch(&mut command_recording_stats);

                self.gpu_profiler
                    .begin(&self.command_list, frame_index, GpuPass::Atrous);
                self.gpu_profiler
                    .begin_event(&self.command_list, GpuPass::Atrous);
                let mut atrous_pipeline_identity: Option<*const ComputePipeline> = None;
                self.active_generation
                    .filter_diffuse_ping
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                self.active_generation
                    .filter_specular_ping
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                self.submit_transition_batch(&mut command_recording_stats);
                self.bind_atrous_arguments(
                    1,
                    self.active_generation
                        .shader_heap
                        .gpu_handle(ATROUS_HISTORY_TABLE_BASES[current_history]),
                    &[1, 0],
                    &mut atrous_pipeline_identity,
                    &mut command_recording_stats,
                );
                self.gpu_profiler
                    .begin(&self.command_list, frame_index, GpuPass::Atrous0);
                self.gpu_profiler
                    .begin_event(&self.command_list, GpuPass::Atrous0);
                self.command_list
                    .Dispatch(render_groups_x, render_groups_y, 1);
                self.gpu_profiler
                    .end(&self.command_list, frame_index, GpuPass::Atrous0);
                self.gpu_profiler.end_event(&self.command_list);
                self.active_generation
                    .filter_diffuse_ping
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                self.active_generation
                    .filter_specular_ping
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );

                self.active_generation
                    .filter_diffuse_pong
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                self.active_generation
                    .filter_specular_pong
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                self.submit_transition_batch(&mut command_recording_stats);
                self.bind_atrous_arguments(
                    2,
                    self.active_generation
                        .shader_heap
                        .gpu_handle(ATROUS_PING_TO_PONG_BASES[current_history]),
                    &[2, 1],
                    &mut atrous_pipeline_identity,
                    &mut command_recording_stats,
                );
                self.gpu_profiler
                    .begin(&self.command_list, frame_index, GpuPass::Atrous1);
                self.gpu_profiler
                    .begin_event(&self.command_list, GpuPass::Atrous1);
                self.command_list
                    .Dispatch(render_groups_x, render_groups_y, 1);
                self.gpu_profiler
                    .end(&self.command_list, frame_index, GpuPass::Atrous1);
                self.gpu_profiler.end_event(&self.command_list);
                self.active_generation
                    .filter_diffuse_pong
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                self.active_generation
                    .filter_specular_pong
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );

                self.active_generation
                    .filter_diffuse_ping
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                self.active_generation
                    .filter_specular_ping
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                self.submit_transition_batch(&mut command_recording_stats);
                self.bind_atrous_arguments(
                    4,
                    self.active_generation
                        .shader_heap
                        .gpu_handle(ATROUS_PONG_TO_PING_BASES[current_history]),
                    &[4, 2],
                    &mut atrous_pipeline_identity,
                    &mut command_recording_stats,
                );
                self.gpu_profiler
                    .begin(&self.command_list, frame_index, GpuPass::Atrous2);
                self.gpu_profiler
                    .begin_event(&self.command_list, GpuPass::Atrous2);
                self.command_list
                    .Dispatch(render_groups_x, render_groups_y, 1);
                self.gpu_profiler
                    .end(&self.command_list, frame_index, GpuPass::Atrous2);
                self.gpu_profiler.end_event(&self.command_list);
                self.active_generation
                    .filter_diffuse_ping
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                self.active_generation
                    .filter_specular_ping
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );

                self.active_generation
                    .filter_diffuse_pong
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                self.active_generation
                    .filter_specular_pong
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                self.submit_transition_batch(&mut command_recording_stats);
                self.bind_atrous_arguments(
                    8,
                    self.active_generation
                        .shader_heap
                        .gpu_handle(ATROUS_PING_TO_PONG_BASES[current_history]),
                    &[8, 3],
                    &mut atrous_pipeline_identity,
                    &mut command_recording_stats,
                );
                self.gpu_profiler
                    .begin(&self.command_list, frame_index, GpuPass::Atrous3);
                self.gpu_profiler
                    .begin_event(&self.command_list, GpuPass::Atrous3);
                self.command_list
                    .Dispatch(render_groups_x, render_groups_y, 1);
                self.gpu_profiler
                    .end(&self.command_list, frame_index, GpuPass::Atrous3);
                self.gpu_profiler.end_event(&self.command_list);
                self.active_generation
                    .filter_diffuse_pong
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                self.active_generation
                    .filter_specular_pong
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                self.submit_transition_batch(&mut command_recording_stats);
                self.gpu_profiler
                    .end(&self.command_list, frame_index, GpuPass::Atrous);
                self.gpu_profiler.end_event(&self.command_list);
            }

            #[cfg(feature = "streamline")]
            if dlss_active {
                let token = dlss_token.as_ref().ok_or_else(|| {
                    streamline_error(
                        "DLSS frame token",
                        crate::streamline::STATUS_NOT_INITIALIZED,
                    )
                })?;
                {
                    let generation = &mut self.active_generation;
                    let dlss = generation.dlss.as_mut().ok_or_else(|| {
                        streamline_error(
                            "DLSS generation resources",
                            crate::streamline::STATUS_NOT_INITIALIZED,
                        )
                    })?;
                    generation.filter_diffuse_pong.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    generation.filter_specular_pong.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    dlss.input_hdr.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                    dlss.exposure.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                    dlss.depth.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    dlss.motion.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    dlss.output_hdr.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                }
                self.submit_transition_batch(&mut command_recording_stats);

                self.gpu_profiler
                    .begin(&self.command_list, frame_index, GpuPass::DlssCompose);
                self.gpu_profiler
                    .begin_event(&self.command_list, GpuPass::DlssCompose);
                self.dlss_compose_pipeline.bind(
                    &self.command_list,
                    self.active_generation
                        .shader_heap
                        .gpu_handle(DLSS_COMPOSE_TABLE_BASES[current_history]),
                    &[u32::from(self.reset_history)],
                );
                self.command_list
                    .Dispatch(render_groups_x, render_groups_y, 1);
                self.gpu_profiler
                    .end(&self.command_list, frame_index, GpuPass::DlssCompose);
                self.gpu_profiler.end_event(&self.command_list);

                let tags = {
                    let generation = &mut self.active_generation;
                    let dlss = generation.dlss.as_mut().unwrap();
                    dlss.input_hdr.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    dlss.exposure.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    [
                        streamline_resource_tag(&dlss.input_hdr, 3, 0, render_extent),
                        streamline_resource_tag(&dlss.output_hdr, 4, 0, output_extent),
                        streamline_resource_tag(&dlss.depth, 0, 1, render_extent),
                        streamline_resource_tag(&dlss.motion, 1, 0, render_extent),
                        streamline_resource_tag(
                            &dlss.exposure,
                            13,
                            0,
                            Extent2D {
                                width: 1,
                                height: 1,
                            },
                        ),
                    ]
                };
                self.submit_transition_batch(&mut command_recording_stats);

                self.gpu_profiler
                    .begin(&self.command_list, frame_index, GpuPass::DlssEvaluate);
                self.gpu_profiler
                    .begin_event(&self.command_list, GpuPass::DlssEvaluate);
                self.streamline
                    .as_ref()
                    .ok_or_else(|| {
                        streamline_error("DLSS runtime", crate::streamline::STATUS_NOT_INITIALIZED)
                    })?
                    .set_tags_and_evaluate(
                        self.active_streamline_viewport.as_mut().ok_or_else(|| {
                            streamline_error(
                                "DLSS viewport",
                                crate::streamline::STATUS_NOT_INITIALIZED,
                            )
                        })?,
                        token,
                        &tags,
                        &self.command_list,
                    )?;
                // Streamline may bind its own descriptor heaps while recording.
                // Restore the application's heaps before the next pass records.
                self.command_list.SetDescriptorHeaps(&[
                    Some(self.active_generation.shader_heap.heap().clone()),
                    Some(self._sampler_heap.heap().clone()),
                ]);
                self.gpu_profiler
                    .end(&self.command_list, frame_index, GpuPass::DlssEvaluate);
                self.gpu_profiler.end_event(&self.command_list);
                self.active_generation
                    .dlss
                    .as_mut()
                    .expect("DLSS generation validated above")
                    .output_hdr
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                self.submit_transition_batch(&mut command_recording_stats);
            }

            #[cfg(feature = "streamline-rr")]
            if rr_active {
                let token = dlss_token.as_ref().ok_or_else(|| {
                    streamline_error(
                        "DLSS RR frame token",
                        crate::streamline::STATUS_NOT_INITIALIZED,
                    )
                })?;
                {
                    let generation = &mut self.active_generation;
                    let rr = generation.rr.as_mut().ok_or_else(|| {
                        streamline_error(
                            "DLSS RR generation resources",
                            crate::streamline::STATUS_NOT_INITIALIZED,
                        )
                    })?;
                    if self.active_path_space.uses_stable_planes() {
                        generation
                            .stable_planes
                            .as_mut()
                            .expect("stable-plane RR generation validated above")
                            .collect_all(
                                &mut self.transition_batch,
                                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                            );
                        for resource in [&mut rr.depth, &mut rr.motion, &mut rr.specular_motion] {
                            resource.collect_transition(
                                &mut self.transition_batch,
                                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                            );
                        }
                    } else {
                        generation
                            .reconstruction_normal_roughness
                            .collect_transition(
                                &mut self.transition_batch,
                                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                            );
                        generation.reconstruction_noisy_hdr.collect_transition(
                            &mut self.transition_batch,
                            D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                        );
                        generation.gbuffer_albedo.collect_transition(
                            &mut self.transition_batch,
                            D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                        );
                    }
                    rr.normal_roughness.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                    rr.input_hdr.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                    rr.primary_emissive.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                    if self.active_path_space == ActivePathSpace::Legacy {
                        rr.motion.collect_transition(
                            &mut self.transition_batch,
                            D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                        );
                    }
                    rr.emissive_history[previous_history].collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    rr.emissive_history[current_history].collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                }
                self.submit_transition_batch(&mut command_recording_stats);

                self.gpu_profiler
                    .begin(&self.command_list, frame_index, GpuPass::RrInputAdapter);
                self.gpu_profiler
                    .begin_event(&self.command_list, GpuPass::RrInputAdapter);
                if self.active_path_space.uses_stable_planes() {
                    self.gpu_profiler.begin(
                        &self.command_list,
                        frame_index,
                        GpuPass::RrStableMerge,
                    );
                    self.gpu_profiler
                        .begin_event(&self.command_list, GpuPass::RrStableMerge);
                }
                if self.active_path_space.uses_stable_planes() {
                    self.rr_stable_input_pipeline.bind(
                        &self.command_list,
                        self.active_generation
                            .shader_heap
                            .gpu_handle(RR_STABLE_INPUT_TABLE_BASE),
                        &[u32::from(self.reset_history)],
                    );
                } else {
                    self.rr_input_pipeline.bind(
                        &self.command_list,
                        self.active_generation
                            .shader_heap
                            .gpu_handle(RR_INPUT_TABLE_BASE),
                        &[u32::from(self.reset_history)],
                    );
                }
                self.command_list
                    .Dispatch(render_groups_x, render_groups_y, 1);
                // The adapter produces the low-resolution emissive layer that
                // the following output-resolution pass samples immediately.
                // An explicit UAV->SRV transition provides both ordering and
                // the correct read state without stalling unrelated outputs.
                self.active_generation
                    .rr
                    .as_mut()
                    .expect("DLSS RR generation validated above")
                    .primary_emissive
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                if self.active_path_space.uses_stable_planes() {
                    let rr = self
                        .active_generation
                        .rr
                        .as_mut()
                        .expect("stable-plane RR generation validated above");
                    for resource in [
                        &mut rr.motion,
                        &mut rr.specular_motion,
                        &mut rr.depth,
                        &mut rr.normal_roughness,
                        &mut rr.input_hdr,
                    ] {
                        resource.collect_transition(
                            &mut self.transition_batch,
                            D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                        );
                    }
                }
                self.submit_transition_batch(&mut command_recording_stats);
                self.rr_emissive_pipeline.bind(
                    &self.command_list,
                    self.active_generation
                        .shader_heap
                        .gpu_handle(RR_EMISSIVE_TABLE_BASES[current_history]),
                    &[
                        camera_jitter_px[0].to_bits(),
                        camera_jitter_px[1].to_bits(),
                        u32::from(self.reset_history),
                    ],
                );
                self.command_list
                    .Dispatch(output_groups_x, output_groups_y, 1);
                if self.active_path_space.uses_stable_planes() {
                    self.gpu_profiler
                        .end(&self.command_list, frame_index, GpuPass::RrStableMerge);
                    self.gpu_profiler.end_event(&self.command_list);
                }
                self.gpu_profiler
                    .end(&self.command_list, frame_index, GpuPass::RrInputAdapter);
                self.gpu_profiler.end_event(&self.command_list);

                let tags = {
                    let generation = &mut self.active_generation;
                    let rr = generation.rr.as_mut().unwrap();
                    rr.input_hdr.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    let (diffuse_albedo, specular_albedo) =
                        if self.active_path_space.uses_stable_planes() {
                            let stable = generation
                                .stable_planes
                                .as_ref()
                                .expect("stable-plane RR generation validated above");
                            (&stable.diffuse_albedo, &stable.specular_albedo)
                        } else {
                            (
                                &generation.reconstruction_diffuse_albedo,
                                &generation.reconstruction_specular_albedo,
                            )
                        };
                    rr.normal_roughness.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    rr.depth.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    rr.motion.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    rr.specular_motion.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    rr.output_hdr.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                    [
                        streamline_resource_tag(&rr.input_hdr, 3, 0, render_extent),
                        streamline_resource_tag(&rr.output_hdr, 4, 0, output_extent),
                        streamline_resource_tag(diffuse_albedo, 7, 0, render_extent),
                        streamline_resource_tag(specular_albedo, 8, 0, render_extent),
                        streamline_resource_tag(&rr.normal_roughness, 14, 0, render_extent),
                        streamline_resource_tag(&rr.motion, 1, 0, render_extent),
                        streamline_resource_tag(&rr.depth, 0, 1, render_extent),
                        // Explicit reflected-geometry motion is the preferred
                        // RR contract. Do not submit specular hit distance at
                        // the same time: the two guides are alternatives.
                        streamline_resource_tag(&rr.specular_motion, 10, 0, render_extent),
                    ]
                };
                self.submit_transition_batch(&mut command_recording_stats);

                self.gpu_profiler
                    .begin(&self.command_list, frame_index, GpuPass::RrEvaluate);
                self.gpu_profiler
                    .begin_event(&self.command_list, GpuPass::RrEvaluate);
                self.streamline
                    .as_ref()
                    .ok_or_else(|| {
                        streamline_error(
                            "DLSS RR runtime",
                            crate::streamline::STATUS_NOT_INITIALIZED,
                        )
                    })?
                    .set_rr_tags_and_evaluate(
                        self.active_streamline_viewport.as_mut().ok_or_else(|| {
                            streamline_error(
                                "DLSS RR viewport",
                                crate::streamline::STATUS_NOT_INITIALIZED,
                            )
                        })?,
                        token,
                        &tags,
                        &self.command_list,
                    )?;
                self.command_list.SetDescriptorHeaps(&[
                    Some(self.active_generation.shader_heap.heap().clone()),
                    Some(self._sampler_heap.heap().clone()),
                ]);
                self.gpu_profiler
                    .end(&self.command_list, frame_index, GpuPass::RrEvaluate);
                self.gpu_profiler.end_event(&self.command_list);
                self.active_generation
                    .rr
                    .as_mut()
                    .expect("DLSS RR generation validated above")
                    .output_hdr
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                self.active_generation
                    .rr
                    .as_mut()
                    .expect("DLSS RR generation validated above")
                    .emissive_history[current_history]
                    .collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                self.submit_transition_batch(&mut command_recording_stats);
            }

            #[cfg(feature = "streamline-rr")]
            if rr_active && self.active_path_space == ActivePathSpace::Legacy {
                {
                    let rr = self
                        .active_generation
                        .rr
                        .as_mut()
                        .expect("DLSS RR generation validated above");
                    rr.output_hdr.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    rr.primary_surface_id[current_history].collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    rr.primary_surface_meta[current_history].collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    rr.primary_motion.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    rr.primary_surface_id[previous_history].collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    rr.primary_surface_meta[previous_history].collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    rr.boundary_history[previous_history].collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                    rr.boundary_history[current_history].collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                    rr.boundary_mask.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                }
                self.submit_transition_batch(&mut command_recording_stats);

                self.gpu_profiler.begin(
                    &self.command_list,
                    frame_index,
                    GpuPass::RrBoundaryResolve,
                );
                self.gpu_profiler
                    .begin_event(&self.command_list, GpuPass::RrBoundaryResolve);
                self.rr_boundary_resolve_pipeline.bind(
                    &self.command_list,
                    self.active_generation
                        .shader_heap
                        .gpu_handle(RR_BOUNDARY_TABLE_BASES[current_history]),
                    &[u32::from(self.reset_history)],
                );
                self.command_list
                    .Dispatch(output_groups_x, output_groups_y, 1);
                self.gpu_profiler
                    .end(&self.command_list, frame_index, GpuPass::RrBoundaryResolve);
                self.gpu_profiler.end_event(&self.command_list);
                let rr = self
                    .active_generation
                    .rr
                    .as_mut()
                    .expect("DLSS RR generation validated above");
                rr.boundary_history[current_history].collect_transition(
                    &mut self.transition_batch,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                );
                rr.boundary_mask.collect_transition(
                    &mut self.transition_batch,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                );
                self.submit_transition_batch(&mut command_recording_stats);
            }

            self.gpu_profiler
                .begin(&self.command_list, frame_index, GpuPass::ToneMap);
            self.gpu_profiler
                .begin_event(&self.command_list, GpuPass::ToneMap);
            let display_output = &mut self.active_generation.display_output;
            #[cfg(feature = "streamline")]
            let tonemap_table = if dlss_active {
                self.active_generation
                    .shader_heap
                    .gpu_handle(DLSS_TONEMAP_TABLE_BASE)
            } else if rr_active {
                #[cfg(feature = "streamline-rr")]
                {
                    self.active_generation
                        .shader_heap
                        .gpu_handle(RR_TONEMAP_TABLE_BASES[current_history])
                }
                #[cfg(not(feature = "streamline-rr"))]
                {
                    unreachable!("RR cannot be active without streamline-rr")
                }
            } else {
                self.active_generation
                    .shader_heap
                    .gpu_handle(TONEMAP_TABLE_BASES[current_history])
            };
            #[cfg(not(feature = "streamline"))]
            let tonemap_table = self
                .active_generation
                .shader_heap
                .gpu_handle(TONEMAP_TABLE_BASES[current_history]);
            self.tonemap_pipeline.bind(
                &self.command_list,
                tonemap_table,
                &[
                    self.debug_view.hlsl_value(),
                    1.0_f32.to_bits(),
                    tonemap_input_mode(self.denoiser, dlss_active || rr_active),
                ],
            );
            self.command_list
                .Dispatch(output_groups_x, output_groups_y, 1);
            self.gpu_profiler
                .end(&self.command_list, frame_index, GpuPass::ToneMap);
            self.gpu_profiler.end_event(&self.command_list);
            display_output
                .collect_transition(&mut self.transition_batch, D3D12_RESOURCE_STATE_COPY_SOURCE);
            self.submit_transition_batch(&mut command_recording_stats);

            self.gpu_profiler
                .end(&self.command_list, frame_index, GpuPass::Total);
            self.gpu_profiler.end_event(&self.command_list);

            // Readback is recorded after Total and before the swap-chain copy. It
            // therefore cannot contaminate either the pass timings or Present.
            let pending_capture = if self.capture_due() {
                Some(self.record_capture_copy(output_extent, render_extent)?)
            } else {
                None
            };

            self.render_targets[frame_index]
                .as_mut()
                .unwrap()
                .collect_transition(&mut self.transition_batch, D3D12_RESOURCE_STATE_COPY_DEST);
            self.submit_transition_batch(&mut command_recording_stats);

            let display_output = &self.active_generation.display_output;
            let render_target = self.render_targets[frame_index].as_mut().unwrap();
            self.command_list
                .CopyResource(render_target.resource(), display_output.resource());
            render_target
                .collect_transition(&mut self.transition_batch, D3D12_RESOURCE_STATE_PRESENT);
            self.active_generation.display_output.collect_transition(
                &mut self.transition_batch,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            );
            self.submit_transition_batch(&mut command_recording_stats);
            self.gpu_profiler.resolve_frame(
                &self.command_list,
                frame_index,
                active_gpu_passes(
                    ReconstructionPath::from_backend(self.denoiser),
                    dlss_active,
                    rr_active,
                    rr_active && self.active_path_space == ActivePathSpace::Legacy,
                    self.active_path_space.uses_stable_planes(),
                ),
            );
            self.command_list.Close()?;
            self.gpu_profiler.record_command_recording(
                command_recording_started.elapsed(),
                command_recording_stats,
            );

            let command_list: ID3D12CommandList = self.command_list.cast()?;
            self.command_queue
                .ExecuteCommandLists(&[Some(command_list)]);
            #[cfg(feature = "streamline")]
            if let Some(token) = frame_token.as_ref() {
                self.submit_pcl_marker(token, PCL_RENDER_SUBMIT_END)?;
                self.submit_pcl_marker(token, PCL_PRESENT_START)?;
            }
            if let Err(error) = self.active_swap_chain().Present(1, DXGI_PRESENT(0)).ok() {
                return Err(device_removed_error(&self.device, error));
            }
            #[cfg(feature = "streamline")]
            if self.streamline_swap_chain.is_some() {
                // The upgraded swap-chain proxy invokes Streamline common's
                // presentCommon exactly once for this successful Present.
                self.reflex_present_common_count =
                    self.reflex_present_common_count.saturating_add(1);
            }
            #[cfg(feature = "streamline")]
            if let Some(token) = frame_token.as_ref() {
                self.submit_pcl_marker(token, PCL_PRESENT_END)?;
            }

            let fence_value = self.next_fence_value;
            self.next_fence_value += 1;
            self.command_queue.Signal(&self.fence, fence_value)?;
            if let Some(mut pending_capture) = pending_capture {
                pending_capture.fence_value = fence_value;
                self.capture_request = None;
                self.pending_capture = Some(pending_capture);
            }
            self.frames[frame_index].fence_value = fence_value;
            self.frames[frame_index].timing_valid = !self.reset_history;
            self.frames[frame_index].timing_generation_id = self.active_generation.id;
            self.frames[frame_index].timing_passes = active_gpu_passes(
                ReconstructionPath::from_backend(self.denoiser),
                dlss_active,
                rr_active,
                rr_active && self.active_path_space == ActivePathSpace::Legacy,
                self.active_path_space.uses_stable_planes(),
            );
            self.frames[frame_index].stable_counter_pending =
                self.active_path_space.uses_stable_planes();
            self.frames[frame_index].stable_counter_valid =
                !self.reset_history && self.active_path_space.uses_stable_planes();
            self.frames[frame_index].stable_counter_generation_id = self.active_generation.id;
            self.frames[frame_index].stable_counter_extent = render_extent;
            self.active_generation.last_used_fence = fence_value;
            if self.benchmark_measurement_active {
                self.benchmark_render_min.width =
                    self.benchmark_render_min.width.min(render_extent.width);
                self.benchmark_render_min.height =
                    self.benchmark_render_min.height.min(render_extent.height);
                self.benchmark_render_max.width =
                    self.benchmark_render_max.width.max(render_extent.width);
                self.benchmark_render_max.height =
                    self.benchmark_render_max.height.max(render_extent.height);
            }
            self._scene_geometry.commit_animation(self.animate_model);
            self.frame_number = self.frame_number.wrapping_add(1);
            self.accumulated_frames = self.accumulated_frames.saturating_add(1);
            self.previous_camera_position = self.camera_position;
            self.previous_camera_yaw = self.camera_yaw;
            self.previous_camera_pitch = self.camera_pitch;
            self.history_index = previous_history;
            self.reset_history = false;
            Ok(())
        }
    }

    fn capture_due(&self) -> bool {
        self.capture_request.as_ref().is_some_and(|request| {
            self.pending_capture.is_none()
                && self.accumulated_frames.saturating_add(1) >= request.after_spp
        })
    }

    unsafe fn record_capture_copy(
        &mut self,
        output_extent: Extent2D,
        render_extent: Extent2D,
    ) -> Result<PendingCapture> {
        let request = self.capture_request.as_ref().ok_or_else(|| {
            WindowsError::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                "capture request missing",
            )
        })?;
        let source = self.active_generation.display_output.resource();
        let description = unsafe { source.GetDesc() };
        let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
        let mut row_count = 0;
        let mut row_size = 0;
        let mut total_bytes = 0;
        unsafe {
            self.device.GetCopyableFootprints(
                &description,
                0,
                1,
                0,
                Some(&mut footprint),
                Some(&mut row_count),
                Some(&mut row_size),
                Some(&mut total_bytes),
            );
        }
        let total_bytes = usize::try_from(total_bytes).map_err(|_| {
            WindowsError::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                "capture readback size does not fit usize",
            )
        })?;
        if total_bytes == 0
            || row_count != output_extent.height
            || row_size < output_extent.width as u64 * 4
        {
            return Err(WindowsError::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                "unexpected display_output copy footprint",
            ));
        }
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_READBACK,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 0,
            VisibleNodeMask: 0,
        };
        let readback_description = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0,
            Width: total_bytes as u64,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_UNKNOWN,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: D3D12_RESOURCE_FLAG_NONE,
        };
        let mut readback: Option<ID3D12Resource> = None;
        unsafe {
            self.device.CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &readback_description,
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut readback,
            )?;
        }
        let readback = readback.ok_or_else(|| {
            WindowsError::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                "D3D12 returned no capture readback resource",
            )
        })?;
        let mut destination = D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(readback.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: footprint,
            },
        };
        let mut source_location = D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(source.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: 0,
            },
        };
        unsafe {
            self.command_list
                .CopyTextureRegion(&destination, 0, 0, 0, &source_location, None);
            ManuallyDrop::drop(&mut destination.pResource);
            ManuallyDrop::drop(&mut source_location.pResource);
        }

        #[cfg(feature = "streamline")]
        let viewport_id = self
            .active_streamline_viewport
            .as_ref()
            .map(|viewport| viewport.viewport.id);
        #[cfg(not(feature = "streamline"))]
        let viewport_id = None;
        let streamline_sdk_version = if cfg!(feature = "streamline") {
            Some("2.12.0".to_string())
        } else {
            None
        };

        Ok(PendingCapture {
            readback,
            footprint,
            total_bytes,
            width: output_extent.width,
            height: output_extent.height,
            metadata: CaptureMetadata {
                png_path: request.path.to_string_lossy().into_owned(),
                gpu_name: self.gpu_name.clone(),
                output_width: output_extent.width,
                output_height: output_extent.height,
                render_width: render_extent.width,
                render_height: render_extent.height,
                requested_scale: self.render_scale(),
                resolution_mode: self.resolution_mode_name().to_string(),
                debug_view: self.debug_view,
                actual_spp: self.accumulated_frames.saturating_add(1),
                frame_index: self.frame_number,
                generation_id: self.active_generation.id,
                atrous_mode: self.atrous_mode.as_str().to_string(),
                command_recording_mode: self.command_recording_mode.as_str().to_string(),
                acceleration_structure_mode: self
                    ._acceleration_structures
                    .stats()
                    .mode
                    .as_str()
                    .to_string(),
                requested_path_space: self.requested_path_space.as_str().to_string(),
                active_path_space: self.active_path_space.as_str().to_string(),
                path_space_consumer: self.path_space_consumer().to_string(),
                stable_plane_allocated_bytes: self
                    .active_generation
                    .stable_planes
                    .as_ref()
                    .map_or(0, |stable_planes| stable_planes.allocated_bytes),
                denoiser_backend: self.denoiser.as_str().to_string(),
                upscaler_mode: self.upscaler.as_str().to_string(),
                reflex_mode: self.reflex_mode_name().to_string(),
                streamline_sdk_version,
                viewport_id,
            },
            fence_value: 0,
        })
    }

    fn poll_pending_capture(&mut self) -> Result<()> {
        let ready = self.pending_capture.as_ref().is_some_and(|pending| unsafe {
            self.fence.GetCompletedValue() >= pending.fence_value
        });
        if !ready {
            return Ok(());
        }
        let pending = self
            .pending_capture
            .take()
            .expect("capture readiness was checked");
        let read_range = D3D12_RANGE {
            Begin: 0,
            End: pending.total_bytes,
        };
        let mut mapped = std::ptr::null_mut::<c_void>();
        unsafe {
            pending
                .readback
                .Map(0, Some(&read_range), Some(&mut mapped))?;
            let mapped = std::slice::from_raw_parts(mapped.cast::<u8>(), pending.total_bytes);
            let rgba = unpack_rgba8_rows(
                mapped,
                pending.footprint.Offset as usize,
                pending.footprint.Footprint.RowPitch as usize,
                pending.width,
                pending.height,
            );
            pending
                .readback
                .Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
            let rgba = rgba.map_err(|error| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    error.to_string(),
                )
            })?;
            let png_bytes = write_png_atomic(
                PathBuf::from(&pending.metadata.png_path).as_path(),
                &rgba,
                pending.width,
                pending.height,
            )
            .map_err(|error| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    error.to_string(),
                )
            })?;
            self.capture_result = Some(capture_json_line(&pending.metadata, png_bytes));
        }
        Ok(())
    }

    pub fn take_capture_result(&mut self) -> Option<String> {
        self.capture_result.take()
    }

    pub fn shutdown(&mut self) -> Result<()> {
        unsafe {
            self.wait_for_gpu()?;
        }
        self.poll_pending_capture()
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            self.minimized = true;
            eprintln!(
                "resize_diagnostic state=minimized output={}x{} render={}x{} generation={} history_reset=0 idle_waits=0",
                self.width,
                self.height,
                self.render_width(),
                self.render_height(),
                self.render_generation_id(),
            );
            return Ok(());
        }
        if !self.minimized && width == self.width && height == self.height {
            return Ok(());
        }

        unsafe {
            let idle_waits_before = self.gpu_idle_wait_count;
            self.wait_for_gpu()?;
            self.poll_pending_capture()?;
            self.reclaim_retired_generations();
            self.gpu_profiler.invalidate();
            self.stable_plane_counter_telemetry = Default::default();
            // 命令列表会持有上一帧 Back Buffer 的引用；重置后再释放资源，
            // 否则 ResizeBuffers 会因仍有外部引用而返回 DXGI_ERROR_INVALID_CALL。
            let frame_index = self.active_swap_chain().GetCurrentBackBufferIndex() as usize;
            self.frames[frame_index].allocator.Reset()?;
            self.command_list.Reset(
                &self.frames[frame_index].allocator,
                None::<&ID3D12PipelineState>,
            )?;
            self.command_list.Close()?;
            self.render_targets = [None, None, None];
            self.active_swap_chain().ResizeBuffers(
                FRAME_COUNT as u32,
                width,
                height,
                DXGI_FORMAT_R8G8B8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
            for frame in &mut self.frames {
                frame.fence_value = 0;
                frame.timing_valid = false;
                frame.timing_generation_id = 0;
                frame.timing_passes = [false; profiler::PASS_COUNT];
                frame.stable_counter_pending = false;
                frame.stable_counter_valid = false;
                frame.stable_counter_generation_id = 0;
                frame.stable_counter_extent = Extent2D {
                    width: 0,
                    height: 0,
                };
            }
            self.width = width;
            self.height = height;
            self.minimized = false;
            self.history_index = 0;
            self.request_history_reset();
            self.accumulated_frames = 0;
            self.previous_camera_position = self.camera_position;
            self.previous_camera_yaw = self.camera_yaw;
            self.previous_camera_pitch = self.camera_pitch;
            let output_extent = Extent2D { width, height };
            #[cfg(feature = "streamline")]
            let new_streamline_viewport = if self.upscaler.uses_streamline() {
                let runtime = self.streamline.as_ref().ok_or_else(|| {
                    streamline_error("DLSS runtime", crate::streamline::STATUS_NOT_INITIALIZED)
                })?;
                let viewport_id = self.next_streamline_viewport_id;
                let viewport = runtime.create_reconstruction_viewport(
                    self.denoiser,
                    self.upscaler,
                    output_extent,
                    viewport_id,
                )?;
                self.next_streamline_viewport_id = viewport_id.saturating_add(1);
                Some(viewport)
            } else {
                None
            };
            #[cfg(feature = "streamline")]
            let new_render_extent = new_streamline_viewport.as_ref().map_or_else(
                || render_extent(output_extent, self.requested_render_scale),
                StreamlineViewport::optimal_extent,
            );
            #[cfg(not(feature = "streamline"))]
            let new_render_extent = render_extent(output_extent, self.requested_render_scale);
            if new_render_extent != self.active_generation.render_extent {
                self.render_extent_change_count = self.render_extent_change_count.saturating_add(1);
            }
            let generation_id = self.next_generation_id;
            let new_generation = RenderResourceGeneration::new(
                &self.device,
                &self._textures,
                &self._scene_geometry,
                &self._acceleration_structures,
                RenderGenerationDesc {
                    output_extent,
                    render_extent: new_render_extent,
                    id: generation_id,
                    with_nrd: self.denoiser == DenoiserBackend::NrdReblur,
                    with_dlss_sr: !self.upscaler.is_native()
                        && self.denoiser != DenoiserBackend::DlssRayReconstruction,
                    with_dlss_rr: self.denoiser == DenoiserBackend::DlssRayReconstruction,
                    with_stable_planes: self.active_path_space.uses_stable_planes(),
                },
            )?;
            self.next_generation_id = self.next_generation_id.saturating_add(1);
            let previous = std::mem::replace(&mut self.active_generation, new_generation);
            #[cfg(feature = "streamline")]
            let previous_streamline_viewport = std::mem::replace(
                &mut self.active_streamline_viewport,
                new_streamline_viewport,
            );
            self.retire_generation(
                previous,
                #[cfg(feature = "streamline")]
                previous_streamline_viewport,
            );
            if let Some(controller) = self.dynamic_resolution.as_mut() {
                controller.reset_after_discontinuity(self.requested_render_scale);
            }
            self.render_generation_create_count =
                self.render_generation_create_count.saturating_add(1);
            self.render_generation_switch_count =
                self.render_generation_switch_count.saturating_add(1);
            self.reclaim_retired_generations();
            self.create_render_targets()?;
            eprintln!(
                "resize_diagnostic state=completed output={}x{} render={}x{} generation={} history_reset=1 idle_waits={}",
                self.width,
                self.height,
                self.render_width(),
                self.render_height(),
                self.render_generation_id(),
                self.gpu_idle_wait_count.saturating_sub(idle_waits_before),
            );
            Ok(())
        }
    }

    fn apply_dynamic_resolution_sample(
        &mut self,
        total_gpu_time_ms: f64,
        generation_matches: bool,
    ) -> Result<()> {
        let now = self.resolution_clock.elapsed();
        let output_extent = self.active_generation.output_extent;
        let active_extent = self.active_generation.render_extent;
        let decision = match self.dynamic_resolution.as_mut() {
            Some(controller) => controller.observe_sample(
                Some(total_gpu_time_ms),
                generation_matches,
                now,
                output_extent,
                active_extent,
            ),
            None => return Ok(()),
        };
        let Some(decision) = decision else {
            return Ok(());
        };

        let old_generation_id = self.active_generation.id;
        let old_extent = self.active_generation.render_extent;
        let old_retire_fence = self.active_generation.last_used_fence;
        self.set_render_scale(decision.new_scale)?;
        if self.active_generation.id == old_generation_id {
            return Ok(());
        }

        self.dynamic_resolution
            .as_mut()
            .expect("dynamic resolution controller exists for a dynamic decision")
            .commit_switch(decision, now);
        let direction = match decision.direction {
            DynamicResolutionDirection::Down => "down",
            DynamicResolutionDirection::Up => "up",
        };
        eprintln!(
            "动态分辨率切换：direction={direction} total={:.3}ms threshold={:.3}ms streak={} scale {:.3}->{:.3} extent {}x{}->{}x{} generation={} old_retire_fence={}",
            f64::from(decision.total_gpu_time_us) / 1_000.0,
            f64::from(decision.threshold_gpu_time_us) / 1_000.0,
            decision.streak,
            decision.old_scale.get(),
            decision.new_scale.get(),
            old_extent.width,
            old_extent.height,
            self.active_generation.render_extent.width,
            self.active_generation.render_extent.height,
            self.active_generation.id,
            old_retire_fence,
        );
        Ok(())
    }

    fn retire_generation(
        &mut self,
        resources: RenderResourceGeneration,
        #[cfg(feature = "streamline")] mut streamline_viewport: Option<StreamlineViewport>,
    ) {
        let retire_fence = resources.last_used_fence;
        if retire_fence == 0 {
            #[cfg(feature = "streamline")]
            if let (Some(runtime), Some(viewport)) =
                (self.streamline.as_ref(), streamline_viewport.as_mut())
            {
                unsafe { runtime.free_resources(viewport) };
            }
            drop(resources);
            return;
        }
        self.retired_generations
            .push_back(RetiredRenderResourceGeneration {
                retire_fence,
                resources,
                #[cfg(feature = "streamline")]
                streamline_viewport,
            });
        self.retired_generation_high_watermark = self
            .retired_generation_high_watermark
            .max(self.retired_generations.len() as u64);
        if self.benchmark_measurement_active {
            self.benchmark_retired_high_watermark = self
                .benchmark_retired_high_watermark
                .max(self.retired_generations.len() as u64);
        }
    }

    /// Request a fixed internal scale at a frame boundary. New resources and
    /// descriptors are fully built before the active generation is replaced;
    /// the old generation remains owned until its submission fence completes.
    pub fn set_render_scale(&mut self, requested: RenderScale) -> Result<()> {
        if self.upscaler.uses_streamline() {
            return Err(WindowsError::new(
                windows::core::HRESULT(0x80070057_u32 as i32),
                "DLSS/DLAA 内部尺寸由 Streamline optimal settings 决定，不能手动设置 render scale",
            ));
        }
        let output_extent = self.active_generation.output_extent;
        let new_render_extent = match classify_render_extent_change(
            self.minimized,
            output_extent,
            self.active_generation.render_extent,
            requested,
        ) {
            RenderExtentChange::DeferredWhileMinimized => {
                self.requested_render_scale = requested;
                return Ok(());
            }
            RenderExtentChange::QuantizedNoop => {
                self.requested_render_scale = requested;
                self.render_scale_quantized_noop_count =
                    self.render_scale_quantized_noop_count.saturating_add(1);
                return Ok(());
            }
            RenderExtentChange::Recreate(extent) => extent,
        };

        let generation_id = self.next_generation_id;
        let new_generation = RenderResourceGeneration::new(
            &self.device,
            &self._textures,
            &self._scene_geometry,
            &self._acceleration_structures,
            RenderGenerationDesc {
                output_extent,
                render_extent: new_render_extent,
                id: generation_id,
                with_nrd: self.denoiser == DenoiserBackend::NrdReblur,
                with_dlss_sr: !self.upscaler.is_native()
                    && self.denoiser != DenoiserBackend::DlssRayReconstruction,
                with_dlss_rr: self.denoiser == DenoiserBackend::DlssRayReconstruction,
                with_stable_planes: self.active_path_space.uses_stable_planes(),
            },
        )
        .map_err(|error| {
            WindowsError::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                format!(
                    "创建 render generation {generation_id}（{}x{}）失败：{error}",
                    new_render_extent.width, new_render_extent.height
                ),
            )
        })?;
        self.next_generation_id = self.next_generation_id.saturating_add(1);
        self.requested_render_scale = requested;
        self.render_generation_create_count = self.render_generation_create_count.saturating_add(1);
        self.render_generation_switch_count = self.render_generation_switch_count.saturating_add(1);
        let previous = std::mem::replace(&mut self.active_generation, new_generation);
        #[cfg(feature = "streamline")]
        let previous_streamline_viewport = self.active_streamline_viewport.take();
        self.retire_generation(
            previous,
            #[cfg(feature = "streamline")]
            previous_streamline_viewport,
        );
        self.render_extent_change_count = self.render_extent_change_count.saturating_add(1);
        self.request_history_reset();
        self.accumulated_frames = 0;
        self.previous_camera_position = self.camera_position;
        self.previous_camera_yaw = self.camera_yaw;
        self.previous_camera_pitch = self.camera_pitch;
        self.reclaim_retired_generations();
        Ok(())
    }

    pub fn cycle_render_scale(&mut self) -> Result<()> {
        if matches!(self.resolution_mode, ResolutionMode::Dynamic(_)) {
            eprintln!("render_scale_manual_change blocked=dynamic_gpu_controlled");
            return Ok(());
        }
        let profiles = [
            RenderScale::NATIVE,
            RenderScale::new(0.83).expect("固定 F2 档位必须有效"),
            RenderScale::new(0.75).expect("固定 F2 档位必须有效"),
            RenderScale::new(0.67).expect("固定 F2 档位必须有效"),
        ];
        let next = profiles
            .iter()
            .position(|profile| *profile == self.requested_render_scale)
            .map_or(RenderScale::NATIVE, |index| {
                profiles[(index + 1) % profiles.len()]
            });
        let old_scale = self.requested_render_scale;
        let old_extent = self.active_generation.render_extent;
        let old_generation = self.active_generation.id;
        self.set_render_scale(next)?;
        eprintln!(
            "render_scale_manual_change mode=fixed old_scale={:.3} new_scale={:.3} old_extent={}x{} new_extent={}x{} generation={}",
            old_scale.get(),
            self.requested_render_scale.get(),
            old_extent.width,
            old_extent.height,
            self.active_generation.render_extent.width,
            self.active_generation.render_extent.height,
            self.active_generation.id,
        );
        debug_assert!(
            self.active_generation.id != old_generation
                || old_extent == self.active_generation.render_extent
        );
        Ok(())
    }

    /// Switch the Streamline DLSS/DLAA viewport transactionally. The new
    /// viewport and render generation are completely prepared before the
    /// active pair is replaced; the previous pair is fence-retired together.
    pub fn cycle_upscaler(&mut self) -> Result<()> {
        #[cfg(not(feature = "streamline"))]
        {
            Err(WindowsError::new(
                windows::core::HRESULT(0x80070057_u32 as i32),
                "F4 切换 DLSS/DLAA 需要使用 cargo run --features streamline 构建",
            ))
        }

        #[cfg(feature = "streamline")]
        {
            let old = self.upscaler;
            let next = if self.denoiser == DenoiserBackend::DlssRayReconstruction {
                old.next_ray_reconstruction_mode().ok_or_else(|| {
                    WindowsError::new(
                        windows::core::HRESULT(0x80070057_u32 as i32),
                        format!("RR 当前处于不受支持的 upscaler 模式：{old}"),
                    )
                })?
            } else {
                old.next_mode()
            };
            let output_extent = self.active_generation.output_extent;
            let new_streamline_viewport = if next.uses_streamline() {
                let runtime = self.streamline.as_ref().ok_or_else(|| {
                    streamline_error("DLSS runtime", crate::streamline::STATUS_NOT_INITIALIZED)
                })?;
                let viewport_id = self.next_streamline_viewport_id;
                let viewport = unsafe {
                    runtime.create_reconstruction_viewport(
                        self.denoiser,
                        next,
                        output_extent,
                        viewport_id,
                    )
                }?;
                self.next_streamline_viewport_id = viewport_id.saturating_add(1);
                Some(viewport)
            } else {
                None
            };
            let new_render_extent = new_streamline_viewport.as_ref().map_or_else(
                || render_extent(output_extent, self.requested_render_scale),
                StreamlineViewport::optimal_extent,
            );
            let generation_id = self.next_generation_id;
            let new_generation = RenderResourceGeneration::new(
                &self.device,
                &self._textures,
                &self._scene_geometry,
                &self._acceleration_structures,
                RenderGenerationDesc {
                    output_extent,
                    render_extent: new_render_extent,
                    id: generation_id,
                    with_nrd: self.denoiser == DenoiserBackend::NrdReblur,
                    with_dlss_sr: next.uses_streamline()
                        && self.denoiser != DenoiserBackend::DlssRayReconstruction,
                    with_dlss_rr: self.denoiser == DenoiserBackend::DlssRayReconstruction,
                    with_stable_planes: self.active_path_space.uses_stable_planes(),
                },
            )
            .map_err(|error| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    format!("创建 upscaler generation {generation_id} 失败：{error}"),
                )
            })?;
            let previous = std::mem::replace(&mut self.active_generation, new_generation);
            let previous_streamline_viewport = std::mem::replace(
                &mut self.active_streamline_viewport,
                new_streamline_viewport,
            );
            let retire_fence = previous.last_used_fence;
            self.retire_generation(previous, previous_streamline_viewport);
            self.next_generation_id = self.next_generation_id.saturating_add(1);
            self.upscaler = next;
            self.render_generation_create_count =
                self.render_generation_create_count.saturating_add(1);
            self.render_generation_switch_count =
                self.render_generation_switch_count.saturating_add(1);
            self.history_index = 0;
            self.accumulated_frames = 0;
            self.previous_camera_position = self.camera_position;
            self.previous_camera_yaw = self.camera_yaw;
            self.previous_camera_pitch = self.camera_pitch;
            self.request_history_reset();
            self.reconstruction_frame_state = self
                .reconstruction_frame_state
                .reset_for_extent(self.render_width(), self.render_height());
            self.reclaim_retired_generations();
            let viewport_id = self
                .active_streamline_viewport
                .as_ref()
                .map_or(0, |viewport| viewport.viewport.id);
            eprintln!(
                "upscaler_switch from={} to={} generation={} viewport={} output={}x{} render={}x{} history_reset=1 idle_waits=0 retire_fence={}",
                old,
                next,
                self.active_generation.id,
                viewport_id,
                output_extent.width,
                output_extent.height,
                new_render_extent.width,
                new_render_extent.height,
                retire_fence,
            );
            Ok(())
        }
    }

    /// Switch denoisers at a frame boundary without waiting for the GPU. The
    /// new generation is fully constructed before the active generation is
    /// replaced; the old generation keeps its bridge and descriptors until
    /// its last submitted fence is complete.
    pub fn cycle_denoiser(&mut self) -> Result<()> {
        #[cfg(feature = "streamline-rr")]
        let rr_loaded = self
            .streamline
            .as_ref()
            .is_some_and(|runtime| runtime._rr_configured);
        #[cfg(not(feature = "streamline-rr"))]
        let rr_loaded = false;

        let next = self
            .denoiser
            .next_runtime_backend(cfg!(feature = "nrd"), rr_loaded)
            .ok_or_else(|| {
                WindowsError::new(
                    windows::core::HRESULT(0x80070057_u32 as i32),
                    "F3 没有可用的重建后端；请启用 nrd，或从 dlss-rr 会话进行 RR/SVGF A/B",
                )
            })?;
        let next_active_path_space = resolve_path_space(self.requested_path_space, next);
        let output_extent = self.active_generation.output_extent;

        // A Streamline viewport owns feature-specific persistent state. Build
        // a fresh viewport for the destination backend and later fence-retire
        // the old viewport together with the generation that last used it.
        #[cfg(feature = "streamline")]
        let destination_viewport_id = self.next_streamline_viewport_id;
        #[cfg(feature = "streamline")]
        let new_streamline_viewport = if self.upscaler.uses_streamline() {
            let runtime = self.streamline.as_ref().ok_or_else(|| {
                streamline_error("DLSS runtime", crate::streamline::STATUS_NOT_INITIALIZED)
            })?;
            let viewport = unsafe {
                runtime.create_reconstruction_viewport(
                    next,
                    self.upscaler,
                    output_extent,
                    destination_viewport_id,
                )
            }?;
            Some(viewport)
        } else {
            None
        };
        #[cfg(feature = "streamline")]
        let new_render_extent = new_streamline_viewport.as_ref().map_or(
            self.active_generation.render_extent,
            StreamlineViewport::optimal_extent,
        );
        #[cfg(not(feature = "streamline"))]
        let new_render_extent = self.active_generation.render_extent;

        let generation_id = self.next_generation_id;
        let new_generation = RenderResourceGeneration::new(
            &self.device,
            &self._textures,
            &self._scene_geometry,
            &self._acceleration_structures,
            RenderGenerationDesc {
                output_extent,
                render_extent: new_render_extent,
                id: generation_id,
                with_nrd: next == DenoiserBackend::NrdReblur,
                with_dlss_sr: !self.upscaler.is_native()
                    && next != DenoiserBackend::DlssRayReconstruction,
                with_dlss_rr: next == DenoiserBackend::DlssRayReconstruction,
                with_stable_planes: next_active_path_space.uses_stable_planes(),
            },
        )
        .map_err(|error| {
            eprintln!(
                "denoiser_switch blocked=create_failed from={} to={} error={error}",
                self.denoiser.as_str(),
                next.as_str()
            );
            WindowsError::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                format!("创建 denoiser generation {generation_id} 失败：{error}"),
            )
        })?;
        let old_name = self.denoiser.as_str();
        let old_active_path_space = self.active_path_space;
        let previous = std::mem::replace(&mut self.active_generation, new_generation);
        #[cfg(feature = "streamline")]
        let destination_viewport_created = new_streamline_viewport.is_some();
        #[cfg(feature = "streamline")]
        let previous_streamline_viewport = std::mem::replace(
            &mut self.active_streamline_viewport,
            new_streamline_viewport,
        );
        let retire_fence = previous.last_used_fence;
        self.retire_generation(
            previous,
            #[cfg(feature = "streamline")]
            previous_streamline_viewport,
        );
        self.next_generation_id = self.next_generation_id.saturating_add(1);
        #[cfg(feature = "streamline")]
        if destination_viewport_created {
            self.next_streamline_viewport_id = destination_viewport_id.saturating_add(1);
        }
        self.render_generation_create_count = self.render_generation_create_count.saturating_add(1);
        self.render_generation_switch_count = self.render_generation_switch_count.saturating_add(1);
        self.denoiser = next;
        self.active_path_space = next_active_path_space;
        self.denoiser_switch_count = self.denoiser_switch_count.saturating_add(1);
        self.stable_plane_counter_telemetry = Default::default();
        self.history_index = 0;
        self.accumulated_frames = 0;
        self.previous_camera_position = self.camera_position;
        self.previous_camera_yaw = self.camera_yaw;
        self.previous_camera_pitch = self.camera_pitch;
        self.request_history_reset();
        self.reconstruction_frame_state = self
            .reconstruction_frame_state
            .reset_for_extent(self.render_width(), self.render_height());
        if let Some(controller) = self.dynamic_resolution.as_mut() {
            controller.reset_after_discontinuity(self.requested_render_scale);
        }
        self.reclaim_retired_generations();
        eprintln!(
            "denoiser_switch from={old_name} to={} path_space_requested={} path_space_active={}->{} generation={} history_reset=1 idle_waits=0 retire_fence={}",
            next.as_str(),
            self.requested_path_space.as_str(),
            old_active_path_space.as_str(),
            self.active_path_space.as_str(),
            self.active_generation.id,
            retire_fence
        );
        Ok(())
    }

    fn reclaim_retired_generations(&mut self) {
        let completed = unsafe { self.fence.GetCompletedValue() };
        while self
            .retired_generations
            .front()
            .is_some_and(|generation| generation.retire_fence <= completed)
        {
            if let Some(retired) = self.retired_generations.pop_front() {
                self.render_generation_retired_count =
                    self.render_generation_retired_count.saturating_add(1);
                #[cfg(feature = "streamline")]
                {
                    let mut viewport = retired.streamline_viewport;
                    if let (Some(runtime), Some(viewport)) =
                        (self.streamline.as_ref(), viewport.as_mut())
                    {
                        unsafe { runtime.free_resources(viewport) };
                    }
                }
                drop(retired.resources);
            }
        }
    }

    pub fn gpu_time_ms(&self) -> f64 {
        self.gpu_profiler.time_ms(GpuPass::Total)
    }

    #[cfg(feature = "streamline")]
    unsafe fn begin_reflex_frame(&mut self) -> Result<Option<crate::streamline::FrameToken>> {
        let Some(streamline) = self.streamline.as_ref() else {
            return Ok(None);
        };
        let token_needed = streamline.reflex_supported()
            || streamline.pcl_supported()
            || self.upscaler.uses_streamline();
        if !token_needed {
            return Ok(None);
        }
        let token = unsafe { streamline.get_frame_token(self.frame_number)? };
        self.reflex_token_count = self.reflex_token_count.saturating_add(1);
        if streamline.reflex_supported() {
            unsafe { streamline.reflex_sleep(&token)? };
            self.reflex_sleep_count = self.reflex_sleep_count.saturating_add(1);
        }
        if streamline.pcl_supported() {
            unsafe { self.submit_pcl_marker(&token, PCL_SIMULATION_START)? };
        }
        Ok(Some(token))
    }

    #[cfg(feature = "streamline")]
    unsafe fn submit_pcl_marker(
        &mut self,
        token: &crate::streamline::FrameToken,
        marker: u32,
    ) -> Result<()> {
        let Some(streamline) = self.streamline.as_ref() else {
            return Ok(());
        };
        if !streamline.pcl_supported() {
            return Ok(());
        }
        if marker != self.reflex_expected_marker {
            self.reflex_marker_order_errors = self.reflex_marker_order_errors.saturating_add(1);
        }
        unsafe { streamline.pcl_marker(token, marker)? };
        if let Some(count) = self.reflex_marker_counts.get_mut(marker as usize) {
            *count = count.saturating_add(1);
        }
        self.reflex_expected_marker = if marker == PCL_PRESENT_END {
            PCL_SIMULATION_START
        } else {
            marker.saturating_add(1)
        };
        Ok(())
    }

    pub fn gpu_time_p95_ms(&self) -> f64 {
        self.gpu_profiler
            .statistics()
            .pass(GpuPass::Total)
            .p95_ms
            .unwrap_or(0.0)
    }

    pub fn gpu_pass_time_ms(&self, pass: GpuPass) -> f64 {
        self.gpu_profiler.time_ms(pass)
    }

    pub fn atrous_mode_name(&self) -> &'static str {
        self.atrous_mode.as_str()
    }

    pub fn denoiser_name(&self) -> &'static str {
        self.denoiser.as_str()
    }

    pub fn active_path_space_name(&self) -> &'static str {
        self.active_path_space.as_str()
    }

    pub fn upscaler_name(&self) -> &'static str {
        self.upscaler.as_str()
    }

    pub fn reflex_mode_name(&self) -> &'static str {
        #[cfg(feature = "streamline")]
        if self
            .streamline
            .as_ref()
            .is_some_and(StreamlineRuntime::reflex_supported)
        {
            return self.reflex_mode.as_str();
        }
        "unavailable"
    }

    pub fn dlss_evaluate_time_ms(&self) -> f64 {
        self.gpu_profiler.time_ms(GpuPass::DlssEvaluate)
    }

    pub fn command_recording_mode_name(&self) -> &'static str {
        self.command_recording_mode.as_str()
    }

    fn atrous_pipeline_for_step(&self, step_width: u32) -> &ComputePipeline {
        match atrous_pipeline_kind(self.atrous_mode, step_width) {
            AtrousPipelineKind::Baseline => &self.atrous_baseline_pipeline,
            AtrousPipelineKind::Shared => &self.atrous_shared_pipeline,
        }
    }

    pub fn valid_timing_sample_serial(&self) -> u64 {
        self.gpu_profiler.valid_sample_serial()
    }

    pub fn begin_benchmark_measurement(&mut self) {
        // Every submitted Frame Context still belongs to warm-up. Mark those
        // timestamp slots invalid so they cannot leak into the measurement
        // epoch when their fences complete later.
        for frame in &mut self.frames {
            frame.timing_valid = false;
            // A fence-complete warm-up counter is still numerically valid, so
            // explicitly detach every pending slice from the new measurement
            // epoch just as we do for timestamp queries.
            frame.stable_counter_valid = false;
        }
        self.stable_plane_counter_telemetry = Default::default();
        self.gpu_profiler.begin_benchmark_measurement();
        self.memory_telemetry.begin_benchmark_measurement();
        self.benchmark_history_reset_baseline = self.history_reset_count;
        self.benchmark_extent_change_baseline = self.render_extent_change_count;
        self.benchmark_generation_create_baseline = self.render_generation_create_count;
        self.benchmark_generation_switch_baseline = self.render_generation_switch_count;
        self.benchmark_generation_retired_baseline = self.render_generation_retired_count;
        self.benchmark_denoiser_switch_baseline = self.denoiser_switch_count;
        self.benchmark_retired_high_watermark = self.retired_generations.len() as u64;
        self.benchmark_gpu_idle_wait_baseline = self.gpu_idle_wait_count;
        self.benchmark_render_scale_noop_baseline = self.render_scale_quantized_noop_count;
        if let Some(controller) = self.dynamic_resolution.as_ref() {
            let state = controller.snapshot();
            self.benchmark_dynamic_valid_samples_baseline = state.valid_samples_consumed;
            self.benchmark_dynamic_stale_samples_baseline = state.stale_generation_samples_ignored;
            self.benchmark_dynamic_downscale_baseline = state.downscale_count;
            self.benchmark_dynamic_upscale_baseline = state.upscale_count;
            self.benchmark_dynamic_at_min_baseline = state.at_min_count;
            self.benchmark_dynamic_at_max_baseline = state.at_max_count;
        } else {
            self.benchmark_dynamic_valid_samples_baseline = 0;
            self.benchmark_dynamic_stale_samples_baseline = 0;
            self.benchmark_dynamic_downscale_baseline = 0;
            self.benchmark_dynamic_upscale_baseline = 0;
            self.benchmark_dynamic_at_min_baseline = 0;
            self.benchmark_dynamic_at_max_baseline = 0;
        }
        self.benchmark_render_min = self.active_generation.render_extent;
        self.benchmark_render_max = self.active_generation.render_extent;
        #[cfg(feature = "streamline")]
        {
            self.benchmark_reflex_token_baseline = self.reflex_token_count;
            self.benchmark_reflex_sleep_baseline = self.reflex_sleep_count;
            self.benchmark_reflex_marker_baseline = self.reflex_marker_counts;
            self.benchmark_reflex_present_common_baseline = self.reflex_present_common_count;
        }
        self.benchmark_measurement_active = true;
    }

    pub fn refresh_memory_telemetry(&mut self) {
        self.memory_telemetry.poll(true);
    }

    pub fn finish_memory_measurement(&mut self) {
        self.memory_telemetry.finish_benchmark_measurement();
    }

    pub fn video_memory_snapshot(&self) -> VideoMemorySnapshot {
        self.memory_telemetry.snapshot()
    }

    pub fn video_memory_title(&self) -> String {
        let memory = self.video_memory_snapshot();
        match (memory.usage_bytes, memory.budget_bytes, memory.usage_ratio) {
            (Some(usage), Some(budget), Some(ratio)) => format!(
                "{:.0}/{:.0} MiB ({:.1}%)",
                usage as f64 / (1024.0 * 1024.0),
                budget as f64 / (1024.0 * 1024.0),
                ratio * 100.0
            ),
            _ => "N/A".to_string(),
        }
    }

    pub fn output_width(&self) -> u32 {
        self.width
    }

    pub fn output_height(&self) -> u32 {
        self.height
    }

    pub fn render_width(&self) -> u32 {
        self.active_generation.render_extent.width
    }

    pub fn render_height(&self) -> u32 {
        self.active_generation.render_extent.height
    }

    pub fn render_scale(&self) -> f32 {
        self.requested_render_scale.get()
    }

    pub fn resolution_mode_name(&self) -> &'static str {
        match self.resolution_mode {
            ResolutionMode::Fixed(_) => "fixed",
            ResolutionMode::Dynamic(_) => "dynamic",
        }
    }

    pub fn resolution_title(&self) -> String {
        match self.dynamic_resolution.as_ref() {
            Some(controller) => {
                let state = controller.snapshot();
                format!(
                    "动态 {:.2} / 目标 {:.1} ms / C{} U{}",
                    state.current_scale.get(),
                    controller.config().target_gpu_time_ms(),
                    state.cooldown_valid_samples_remaining,
                    state.upscale_warmup_remaining,
                )
            }
            None => format!("固定 {:.2}", self.render_scale()),
        }
    }

    pub fn render_generation_id(&self) -> u64 {
        self.active_generation.id
    }

    pub fn retired_generation_count(&self) -> usize {
        self.retired_generations.len()
    }

    fn active_swap_chain(&self) -> &IDXGISwapChain3 {
        #[cfg(feature = "streamline")]
        if let Some(swap_chain) = self.streamline_swap_chain.as_ref() {
            return swap_chain;
        }
        &self.swap_chain
    }

    pub fn benchmark_json(&self, duration_seconds: u64, warmup_valid_frames: u32) -> String {
        #[cfg(feature = "streamline")]
        let dlss_viewport_id = self
            .active_streamline_viewport
            .as_ref()
            .map(|viewport| viewport.viewport.id);
        #[cfg(not(feature = "streamline"))]
        let dlss_viewport_id = None;
        benchmark_json_line(
            BenchmarkJsonContext {
                gpu_name: &self.gpu_name,
                width: self.width,
                height: self.height,
                render_width: self.render_width(),
                render_height: self.render_height(),
                render_scale_requested: self.render_scale(),
                resolution_mode: self.resolution_mode_name(),
                dynamic_resolution: self.dynamic_resolution_json(),
                render_generation_id: self.render_generation_id(),
                render_generation_create_count: self
                    .render_generation_create_count
                    .saturating_sub(self.benchmark_generation_create_baseline),
                render_generation_switch_count: self
                    .render_generation_switch_count
                    .saturating_sub(self.benchmark_generation_switch_baseline),
                render_generation_retired_count: self
                    .render_generation_retired_count
                    .saturating_sub(self.benchmark_generation_retired_baseline),
                retired_generation_count: self.retired_generation_count() as u64,
                retired_generation_high_watermark: self.benchmark_retired_high_watermark,
                gpu_idle_wait_count: self
                    .gpu_idle_wait_count
                    .saturating_sub(self.benchmark_gpu_idle_wait_baseline),
                render_scale_quantized_noop_count: self
                    .render_scale_quantized_noop_count
                    .saturating_sub(self.benchmark_render_scale_noop_baseline),
                render_min: self.benchmark_render_min,
                render_max: self.benchmark_render_max,
                duration_seconds,
                warmup_valid_frames,
                pix_events_available: self.gpu_profiler.pix_events_available(),
                history_reset_count: self
                    .history_reset_count
                    .saturating_sub(self.benchmark_history_reset_baseline),
                render_extent_change_count: self
                    .render_extent_change_count
                    .saturating_sub(self.benchmark_extent_change_baseline),
                atrous_mode: self.atrous_mode.as_str(),
                command_recording_mode: self.command_recording_mode.as_str(),
                requested_path_space: self.requested_path_space.as_str(),
                active_path_space: self.active_path_space.as_str(),
                path_space_consumer: self.path_space_consumer(),
                stable_plane_allocated_bytes: self
                    .active_generation
                    .stable_planes
                    .as_ref()
                    .map_or(0, |stable_planes| stable_planes.allocated_bytes),
                stable_plane_counters: &self.stable_plane_counter_telemetry,
                denoiser_backend: self.denoiser.as_str(),
                upscaler_mode: self.upscaler.as_str(),
                dlss_optimal: self.dlss_optimal_json(),
                dlss_viewport_id,
                rr_support: self.rr_support_json(),
                reflex: self.reflex_json(),
                denoiser_switch_count: self
                    .denoiser_switch_count
                    .saturating_sub(self.benchmark_denoiser_switch_baseline),
                nrd_compiled: self.denoiser.nrd_compiled(),
            },
            self.gpu_profiler.benchmark_statistics(),
            self.gpu_profiler.command_recording_statistics(),
            self.video_memory_snapshot(),
            self.memory_telemetry.measurement(),
            self._acceleration_structures.stats(),
        )
    }

    fn dynamic_resolution_json(&self) -> serde_json::Value {
        dynamic_resolution_json_value(
            self.dynamic_resolution.as_ref(),
            DynamicResolutionBenchmarkBaseline {
                valid_samples: self.benchmark_dynamic_valid_samples_baseline,
                stale_samples: self.benchmark_dynamic_stale_samples_baseline,
                downscale_count: self.benchmark_dynamic_downscale_baseline,
                upscale_count: self.benchmark_dynamic_upscale_baseline,
                at_min_count: self.benchmark_dynamic_at_min_baseline,
                at_max_count: self.benchmark_dynamic_at_max_baseline,
            },
        )
    }

    fn path_space_consumer(&self) -> &'static str {
        if self.active_path_space == ActivePathSpace::Legacy {
            "legacy"
        } else if self.denoiser == DenoiserBackend::NrdReblur {
            "nrd-stable-planes"
        } else if self.denoiser == DenoiserBackend::DlssRayReconstruction {
            "rr-stable-planes"
        } else {
            "diagnostic-only"
        }
    }

    fn dlss_optimal_json(&self) -> serde_json::Value {
        #[cfg(feature = "streamline")]
        if let Some(viewport) = self.active_streamline_viewport.as_ref() {
            #[cfg(feature = "streamline-rr")]
            if viewport.is_rr
                && let Some(optimal) = viewport.rr_optimal
            {
                return serde_json::json!({
                    "kind": "dlss_rr",
                    "optimal_render_width": optimal.optimal_render_width,
                    "optimal_render_height": optimal.optimal_render_height,
                    "render_width_min": optimal.render_width_min,
                    "render_height_min": optimal.render_height_min,
                    "render_width_max": optimal.render_width_max,
                    "render_height_max": optimal.render_height_max,
                    "optimal_sharpness": optimal.optimal_sharpness,
                });
            }
            return serde_json::json!({
                "kind": "dlss",
                "optimal_render_width": viewport.optimal.optimal_render_width,
                "optimal_render_height": viewport.optimal.optimal_render_height,
                "render_width_min": viewport.optimal.render_width_min,
                "render_height_min": viewport.optimal.render_height_min,
                "render_width_max": viewport.optimal.render_width_max,
                "render_height_max": viewport.optimal.render_height_max,
                "optimal_sharpness": viewport.optimal.optimal_sharpness,
            });
        }
        serde_json::Value::Null
    }

    #[cfg(feature = "streamline")]
    fn rr_support_json(&self) -> serde_json::Value {
        #[cfg(feature = "streamline-rr")]
        if let Some(runtime) = self.streamline.as_ref() {
            return serde_json::json!({
                "compiled": true,
                "supported": runtime._support.rr_supported != 0,
                "supported_raw": runtime._support.rr_supported,
                "result_raw": runtime._support.rr_result,
            });
        }
        serde_json::json!({
            "compiled": cfg!(feature = "streamline-rr"),
            "supported": false,
            "supported_raw": 0,
            "result_raw": crate::streamline::STATUS_UNSUPPORTED,
        })
    }

    #[cfg(not(feature = "streamline"))]
    fn rr_support_json(&self) -> serde_json::Value {
        serde_json::json!({
            "compiled": false,
            "supported": false,
            "supported_raw": 0,
            "result_raw": 5,
        })
    }

    fn reflex_json(&self) -> serde_json::Value {
        #[cfg(feature = "streamline")]
        {
            let (reflex_supported, pcl_supported) =
                self.streamline.as_ref().map_or((false, false), |runtime| {
                    (runtime.reflex_supported(), runtime.pcl_supported())
                });
            let active_mode = if reflex_supported {
                self.reflex_mode.as_str()
            } else {
                "unavailable"
            };
            serde_json::json!({
                "compiled": true,
                "sdk_version": crate::streamline::SDK_VERSION,
                "support": {
                    "reflex": reflex_supported,
                    "pcl": pcl_supported,
                },
                "requested_mode": self.reflex_mode.as_str(),
                "active_mode": active_mode,
                "token_count": self.reflex_token_count.saturating_sub(self.benchmark_reflex_token_baseline),
                "sleep_count": self.reflex_sleep_count.saturating_sub(self.benchmark_reflex_sleep_baseline),
                "marker_counts": {
                    "simulation_start": self.reflex_marker_counts[0].saturating_sub(self.benchmark_reflex_marker_baseline[0]),
                    "simulation_end": self.reflex_marker_counts[1].saturating_sub(self.benchmark_reflex_marker_baseline[1]),
                    "render_submit_start": self.reflex_marker_counts[2].saturating_sub(self.benchmark_reflex_marker_baseline[2]),
                    "render_submit_end": self.reflex_marker_counts[3].saturating_sub(self.benchmark_reflex_marker_baseline[3]),
                    "present_start": self.reflex_marker_counts[4].saturating_sub(self.benchmark_reflex_marker_baseline[4]),
                    "present_end": self.reflex_marker_counts[5].saturating_sub(self.benchmark_reflex_marker_baseline[5]),
                },
                "present_common_count": self
                    .reflex_present_common_count
                    .saturating_sub(self.benchmark_reflex_present_common_baseline),
                "order_errors": self.reflex_marker_order_errors,
                "report_available": false,
            })
        }
        #[cfg(not(feature = "streamline"))]
        {
            serde_json::json!({
                "compiled": false,
                "sdk_version": serde_json::Value::Null,
                "support": {
                    "reflex": false,
                    "pcl": false,
                },
                "requested_mode": self.reflex_mode.as_str(),
                "active_mode": "unavailable",
                "token_count": 0,
                "sleep_count": 0,
                "marker_counts": {
                    "simulation_start": 0,
                    "simulation_end": 0,
                    "render_submit_start": 0,
                    "render_submit_end": 0,
                    "present_start": 0,
                    "present_end": 0,
                },
                "present_common_count": 0,
                "order_errors": 0,
                "report_available": false,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DynamicResolutionBenchmarkBaseline {
    valid_samples: u64,
    stale_samples: u64,
    downscale_count: u64,
    upscale_count: u64,
    at_min_count: u64,
    at_max_count: u64,
}

fn dynamic_resolution_json_value(
    controller: Option<&DynamicResolutionController>,
    baseline: DynamicResolutionBenchmarkBaseline,
) -> serde_json::Value {
    let Some(controller) = controller else {
        return serde_json::Value::Null;
    };
    let config = controller.config();
    let state = controller.snapshot();
    let direction = state.last_direction.map(|direction| match direction {
        DynamicResolutionDirection::Down => "down",
        DynamicResolutionDirection::Up => "up",
    });
    let lifetime_switches = state.downscale_count.saturating_add(state.upscale_count);
    serde_json::json!({
        "config": {
            "target_gpu_ms": config.target_gpu_time_ms(),
            "high_threshold_ms": f64::from(config.high_threshold_us()) / 1_000.0,
            "low_threshold_ms": f64::from(config.low_threshold_us()) / 1_000.0,
            "min_scale": f64::from(DynamicResolutionConfig::DYNAMIC_MIN_SCALE_MILLI) / 1_000.0,
            "max_scale": f64::from(DynamicResolutionConfig::DYNAMIC_MAX_SCALE_MILLI) / 1_000.0,
            "down_streak": DynamicResolutionConfig::DOWN_STREAK,
            "up_streak": DynamicResolutionConfig::UP_STREAK,
            "down_step": f64::from(DynamicResolutionConfig::DOWN_STEP_MILLI) / 1_000.0,
            "up_step": f64::from(DynamicResolutionConfig::UP_STEP_MILLI) / 1_000.0,
            "cooldown_valid_samples": DynamicResolutionConfig::COOLDOWN_VALID_SAMPLES,
            "cooldown_seconds": DynamicResolutionConfig::COOLDOWN_TIME.as_secs_f64(),
            "upscale_warmup": DynamicResolutionConfig::UPSCALE_WARMUP,
        },
        "state": {
            "current_requested_scale": state.current_scale.get(),
            "cooldown_valid_samples_remaining": state.cooldown_valid_samples_remaining,
            "upscale_warmup_remaining": state.upscale_warmup_remaining,
            "over_budget_streak": state.over_budget_streak,
            "under_budget_streak": state.under_budget_streak,
            "last_direction": direction,
            "last_trigger_total_ms": state.last_trigger_total_us.map(|value| f64::from(value) / 1_000.0),
        },
        "measurement": {
            "valid_samples": state.valid_samples_consumed.saturating_sub(baseline.valid_samples),
            "stale_generation_samples_ignored": state.stale_generation_samples_ignored.saturating_sub(baseline.stale_samples),
            "downscale_count": state.downscale_count.saturating_sub(baseline.downscale_count),
            "upscale_count": state.upscale_count.saturating_sub(baseline.upscale_count),
            "switch_count": lifetime_switches.saturating_sub(baseline.downscale_count.saturating_add(baseline.upscale_count)),
            "at_min_count": state.at_min_count.saturating_sub(baseline.at_min_count),
            "at_max_count": state.at_max_count.saturating_sub(baseline.at_max_count),
        },
        "lifetime": {
            "valid_samples": state.valid_samples_consumed,
            "stale_generation_samples_ignored": state.stale_generation_samples_ignored,
            "downscale_count": state.downscale_count,
            "upscale_count": state.upscale_count,
            "switch_count": lifetime_switches,
            "at_min_count": state.at_min_count,
            "at_max_count": state.at_max_count,
        },
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AtrousPipelineKind {
    Baseline,
    Shared,
}

fn atrous_pipeline_kind(mode: AtrousMode, step_width: u32) -> AtrousPipelineKind {
    if mode == AtrousMode::Shared && matches!(step_width, 1 | 2) {
        AtrousPipelineKind::Shared
    } else {
        AtrousPipelineKind::Baseline
    }
}

fn should_bind_atrous_pipeline<T: Copy + PartialEq>(
    command_mode: CommandRecordingMode,
    previous: Option<T>,
    current: T,
) -> bool {
    command_mode == CommandRecordingMode::Baseline || previous != Some(current)
}

struct BenchmarkJsonContext<'a> {
    gpu_name: &'a str,
    width: u32,
    height: u32,
    render_width: u32,
    render_height: u32,
    render_scale_requested: f32,
    resolution_mode: &'a str,
    dynamic_resolution: serde_json::Value,
    render_generation_id: u64,
    render_generation_create_count: u64,
    render_generation_switch_count: u64,
    render_generation_retired_count: u64,
    retired_generation_count: u64,
    retired_generation_high_watermark: u64,
    gpu_idle_wait_count: u64,
    render_scale_quantized_noop_count: u64,
    render_min: Extent2D,
    render_max: Extent2D,
    duration_seconds: u64,
    warmup_valid_frames: u32,
    pix_events_available: bool,
    history_reset_count: u64,
    render_extent_change_count: u64,
    atrous_mode: &'a str,
    command_recording_mode: &'a str,
    requested_path_space: &'a str,
    active_path_space: &'a str,
    path_space_consumer: &'a str,
    stable_plane_allocated_bytes: u64,
    stable_plane_counters: &'a crate::path_space::StablePlaneCounterTelemetry,
    denoiser_backend: &'a str,
    upscaler_mode: &'a str,
    dlss_optimal: serde_json::Value,
    dlss_viewport_id: Option<u32>,
    rr_support: serde_json::Value,
    reflex: serde_json::Value,
    denoiser_switch_count: u64,
    nrd_compiled: bool,
}

fn benchmark_json_line(
    context: BenchmarkJsonContext<'_>,
    report: profiler::GpuTimingReport,
    command_recording: profiler::CommandRecordingStats,
    memory: VideoMemorySnapshot,
    memory_measurement: VideoMemoryMeasurement,
    acceleration_structures: &AccelerationStructureStats,
) -> String {
    let BenchmarkJsonContext {
        gpu_name,
        width,
        height,
        render_width,
        render_height,
        render_scale_requested,
        resolution_mode,
        dynamic_resolution,
        render_generation_id,
        render_generation_create_count,
        render_generation_switch_count,
        render_generation_retired_count,
        retired_generation_count,
        retired_generation_high_watermark,
        gpu_idle_wait_count,
        render_scale_quantized_noop_count,
        render_min,
        render_max,
        duration_seconds,
        warmup_valid_frames,
        pix_events_available,
        history_reset_count,
        render_extent_change_count,
        atrous_mode,
        command_recording_mode,
        requested_path_space,
        active_path_space,
        path_space_consumer,
        stable_plane_allocated_bytes,
        stable_plane_counters,
        denoiser_backend,
        upscaler_mode,
        dlss_optimal,
        dlss_viewport_id,
        rr_support,
        reflex,
        denoiser_switch_count,
        nrd_compiled,
    } = context;
    let status = match memory.status {
        VideoMemoryStatus::Available => "available",
        VideoMemoryStatus::Adapter3Unavailable => "adapter3_unavailable",
        VideoMemoryStatus::QueryFailed => "query_failed",
    };
    let blas = acceleration_structures
        .blas
        .iter()
        .map(|record| {
            serde_json::json!({
                "primitive_index": record.primitive_index,
                "original_result_bytes": record.original_result_bytes,
                "reported_compacted_bytes": record.reported_compacted_bytes,
                "original_allocation_bytes": record.original_allocation_bytes,
                "candidate_allocation_bytes": record.candidate_allocation_bytes,
                "final_result_bytes": record.final_result_bytes,
                "final_allocation_bytes": record.final_allocation_bytes,
                "decision": record.decision.as_str(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "schema_version": 2,
        "reconstruction_contract_version": RECONSTRUCTION_CONTRACT_VERSION,
        "build": {
            "git_head": env!("RAY_TRACING_BUILD_GIT_HEAD"),
            "git_tree": env!("RAY_TRACING_BUILD_GIT_TREE"),
            "git_dirty": env!("RAY_TRACING_BUILD_GIT_DIRTY") == "true",
            "features": env!("RAY_TRACING_BUILD_FEATURES")
                .split(',')
                .filter(|feature| !feature.is_empty())
                .collect::<Vec<_>>(),
        },
        "gpu_name": gpu_name,
        "output_width": width,
        "output_height": height,
        "render_width": render_width,
        "render_height": render_height,
        "render_min_width": render_min.width,
        "render_min_height": render_min.height,
        "render_max_width": render_max.width,
        "render_max_height": render_max.height,
        "resolution_mode": resolution_mode,
        "render_scale_requested": render_scale_requested,
        "render_scale_effective_x": render_width as f64 / width as f64,
        "render_scale_effective_y": render_height as f64 / height as f64,
        "render_generation_id": render_generation_id,
        "render_generation_create_count": render_generation_create_count,
        "render_generation_switch_count": render_generation_switch_count,
        "render_generation_retired_count": render_generation_retired_count,
        "retired_generation_count": retired_generation_count,
        "retired_generation_high_watermark": retired_generation_high_watermark,
        "gpu_idle_wait_count": gpu_idle_wait_count,
        "render_scale_quantized_noop_count": render_scale_quantized_noop_count,
        "dynamic_resolution": dynamic_resolution,
        "path_space": {
            "requested": requested_path_space,
            "active": active_path_space,
            "plane_count": if active_path_space == "stable-planes" { crate::path_space::STABLE_PLANE_COUNT } else { 0 },
            "consumer": path_space_consumer,
            "allocated_bytes": stable_plane_allocated_bytes,
            "counters": if active_path_space == "stable-planes" {
                stable_plane_counter_json(stable_plane_counters)
            } else {
                serde_json::Value::Null
            },
        },
        "atrous_mode": atrous_mode,
        "upscaler": {
            "mode": upscaler_mode,
            "dlss_optimal": dlss_optimal,
            "viewport_id": dlss_viewport_id,
        },
        "dlss_rr": rr_support,
        "reflex": reflex,
        "denoiser": {
            "requested": denoiser_backend,
            "active": denoiser_backend,
            "nrd_compiled": nrd_compiled,
            "nrd_version": if cfg!(feature = "nrd") { Some(NRD_VERSION) } else { None::<&str> },
            "nrd_commit": if cfg!(feature = "nrd") { Some(NRD_COMMIT_PREFIX) } else { None::<&str> },
            "history_reset_count": history_reset_count,
            "switch_count": denoiser_switch_count,
        },
        "command_recording": {
            "mode": command_recording_mode,
            "cpu_ms": {
                "p50_ms": command_recording.cpu_ms.p50_ms,
                "p95_ms": command_recording.cpu_ms.p95_ms,
                "valid_samples": command_recording.cpu_ms.valid_samples,
            },
            "tracked_transition_api_calls_mean": command_recording.tracked_transition_api_calls_mean,
            "tracked_transition_barriers_mean": command_recording.tracked_transition_barriers_mean,
            "atrous_pipeline_binds_mean": command_recording.atrous_pipeline_binds_mean,
            "atrous_argument_updates_mean": command_recording.atrous_argument_updates_mean,
        },
        "acceleration_structures": {
            "mode": acceleration_structures.mode.as_str(),
            "tlas_update_enabled": acceleration_structures.tlas_update_enabled,
            "blas_count": acceleration_structures.blas_count,
            "compacted_blas_count": acceleration_structures.compacted_blas_count,
            "invalid_compacted_size_count": acceleration_structures.invalid_compacted_size_count,
            "no_allocation_saving_count": acceleration_structures.no_allocation_saving_count,
            "disabled_blas_count": acceleration_structures.disabled_blas_count,
            "original_result_bytes": acceleration_structures.original_result_bytes,
            "final_result_bytes": acceleration_structures.final_result_bytes,
            "original_allocation_bytes": acceleration_structures.original_allocation_bytes,
            "final_allocation_bytes": acceleration_structures.final_allocation_bytes,
            "allocation_bytes_saved": acceleration_structures.allocation_bytes_saved,
            "allocation_saving_ratio": acceleration_structures.allocation_saving_ratio.filter(|value| value.is_finite()),
            "tlas_result_bytes": acceleration_structures.tlas_result_bytes,
            "tlas_allocation_bytes": acceleration_structures.tlas_allocation_bytes,
            // Compatibility alias: this now consistently means committed allocation bytes.
            "retained_update_scratch_bytes": acceleration_structures.retained_update_scratch_allocation_bytes,
            "retained_update_scratch_required_bytes": acceleration_structures.retained_update_scratch_required_bytes,
            "retained_update_scratch_allocation_bytes": acceleration_structures.retained_update_scratch_allocation_bytes,
            "blas": blas,
        },
        "benchmark_seconds": duration_seconds,
        "warmup_valid_frames": warmup_valid_frames,
        "valid_samples": report.pass(GpuPass::Total).valid_samples,
        "history_reset_count": history_reset_count,
        "render_extent_change_count": render_extent_change_count,
        "pix_events_available": pix_events_available,
        "pix_runtime_version": env!("WINPIX_RUNTIME_VERSION"),
        "passes": {
            "total": gpu_pass_json(report.pass(GpuPass::Total)),
            "acceleration_structure": gpu_pass_json(report.pass(GpuPass::AccelerationStructure)),
            "path_trace": gpu_pass_json(report.pass(GpuPass::PathTrace)),
            "temporal": gpu_pass_json(report.pass(GpuPass::Temporal)),
            "atrous": gpu_pass_json(report.pass(GpuPass::Atrous)),
            "atrous_0": gpu_pass_json(report.pass(GpuPass::Atrous0)),
            "atrous_1": gpu_pass_json(report.pass(GpuPass::Atrous1)),
            "atrous_2": gpu_pass_json(report.pass(GpuPass::Atrous2)),
            "atrous_3": gpu_pass_json(report.pass(GpuPass::Atrous3)),
            "tone_map": gpu_pass_json(report.pass(GpuPass::ToneMap)),
            "nrd_prep": gpu_pass_json(report.pass(GpuPass::NrdPrep)),
            "nrd_denoise": gpu_pass_json(report.pass(GpuPass::NrdDenoise)),
            "nrd_compose": gpu_pass_json(report.pass(GpuPass::NrdCompose)),
            "dlss_compose": gpu_pass_json(report.pass(GpuPass::DlssCompose)),
            "dlss_evaluate": gpu_pass_json(report.pass(GpuPass::DlssEvaluate)),
            "rr_input_adapter": gpu_pass_json(report.pass(GpuPass::RrInputAdapter)),
            "rr_evaluate": gpu_pass_json(report.pass(GpuPass::RrEvaluate)),
            "rr_primary_visibility": gpu_pass_json(report.pass(GpuPass::RrPrimaryVisibility)),
            "rr_boundary_resolve": gpu_pass_json(report.pass(GpuPass::RrBoundaryResolve)),
            "stable_plane": {
                "build": gpu_pass_json(report.pass(GpuPass::StablePlaneBuild)),
                "fill": [
                    gpu_pass_json(report.pass(GpuPass::StablePlaneFill0)),
                    gpu_pass_json(report.pass(GpuPass::StablePlaneFill1)),
                    gpu_pass_json(report.pass(GpuPass::StablePlaneFill2)),
                ],
            },
            "nrd_stable": {
                "prep": [
                    gpu_pass_json(report.pass(GpuPass::NrdStablePrep0)),
                    gpu_pass_json(report.pass(GpuPass::NrdStablePrep1)),
                    gpu_pass_json(report.pass(GpuPass::NrdStablePrep2)),
                ],
                "denoise": [
                    gpu_pass_json(report.pass(GpuPass::NrdStableDenoise0)),
                    gpu_pass_json(report.pass(GpuPass::NrdStableDenoise1)),
                    gpu_pass_json(report.pass(GpuPass::NrdStableDenoise2)),
                ],
                "compose": [
                    gpu_pass_json(report.pass(GpuPass::NrdStableCompose0)),
                    gpu_pass_json(report.pass(GpuPass::NrdStableCompose1)),
                    gpu_pass_json(report.pass(GpuPass::NrdStableCompose2)),
                ],
            },
            "rr_stable_merge": gpu_pass_json(report.pass(GpuPass::RrStableMerge)),
        },
        "memory": {
            "usage_bytes": memory.usage_bytes,
            "budget_bytes": memory.budget_bytes,
            "usage_ratio": memory.usage_ratio.filter(|ratio| ratio.is_finite()),
            "status": status,
            "measurement": {
                "active": memory_measurement.active,
                "start_usage_bytes": memory_measurement.start_usage_bytes,
                "end_usage_bytes": memory_measurement.end_usage_bytes,
                "peak_usage_bytes": memory_measurement.peak_usage_bytes,
                "minimum_budget_bytes": memory_measurement.minimum_budget_bytes,
                "peak_usage_ratio": memory_measurement.peak_usage_ratio.filter(|ratio| ratio.is_finite()),
                "valid_query_count": memory_measurement.valid_query_count,
                "failed_query_count": memory_measurement.failed_query_count,
                "checkpoint_count": memory_measurement.checkpoint_count,
                "checkpoints": memory_measurement.checkpoints[..memory_measurement.checkpoint_count]
                    .iter()
                    .flatten()
                    .map(|checkpoint| serde_json::json!({
                        "elapsed_seconds": checkpoint.elapsed_seconds,
                        "usage_bytes": checkpoint.usage_bytes,
                        "budget_bytes": checkpoint.budget_bytes,
                    }))
                    .collect::<Vec<_>>(),
            },
        },
    })
    .to_string()
}

fn gpu_pass_json(stats: profiler::GpuTimingStats) -> serde_json::Value {
    // An inactive pass is represented by null rather than a misleading zero
    // millisecond sample. A pass with no fence-complete samples is not proof
    // that the GPU spent zero time in it.
    if stats.valid_samples == 0 {
        return serde_json::Value::Null;
    }
    serde_json::json!({
        "p50_ms": stats.p50_ms.filter(|value| value.is_finite()),
        "p95_ms": stats.p95_ms.filter(|value| value.is_finite()),
        "valid_samples": stats.valid_samples,
    })
}

fn stable_plane_counter_json(
    telemetry: &crate::path_space::StablePlaneCounterTelemetry,
) -> serde_json::Value {
    let per_frame_means = (0..crate::path_space::STABLE_PLANE_COUNTER_COUNT)
        .map(|index| telemetry.per_frame_mean(index))
        .collect::<Vec<_>>();
    let schema = crate::path_space::STABLE_PLANE_COUNTER_NAMES.to_vec();
    let last = telemetry.last.map(|snapshot| {
        serde_json::json!({
            "generation_id": snapshot.generation_id,
            "render_width": snapshot.extent[0],
            "render_height": snapshot.extent[1],
            "values": snapshot.values,
        })
    });
    serde_json::json!({
        "schema": schema,
        "completed_frames": telemetry.completed_frames,
        "pixels_traced": telemetry.sum(0),
        "active_planes_mean": telemetry.active_planes_mean(),
        "plane_count_histogram": telemetry.plane_count_histogram(),
        "plane_overflow_pixels": telemetry.sum(6),
        "branch_queue_overflow_events": telemetry.sum(7),
        "interior_overflow_events": telemetry.sum(8),
        "false_intersection_rejections": telemetry.sum(9),
        "total_internal_reflection_events": telemetry.sum(10),
        "invalid_medium_exit_events": telemetry.sum(11),
        "per_frame_mean": per_frame_means,
        "last": last,
    })
}

impl Dx12Renderer {
    pub fn shader_status(&self) -> &str {
        &self.shader_status
    }

    pub fn raytracing_status(&self) -> &str {
        &self.raytracing_status
    }

    pub fn sample_count(&self) -> u32 {
        self.accumulated_frames
    }

    pub fn move_camera(&mut self, forward: f32, right: f32, vertical: f32) {
        let forward_axis = [self.camera_yaw.sin(), 0.0, self.camera_yaw.cos()];
        let right_axis = [forward_axis[2], 0.0, -forward_axis[0]];
        for axis in 0..3 {
            self.camera_position[axis] += forward_axis[axis] * forward + right_axis[axis] * right;
        }
        self.camera_position[1] += vertical;
        self.accumulated_frames = 0;
    }

    pub fn rotate_camera(&mut self, yaw: f32, pitch: f32) {
        self.camera_yaw += yaw;
        self.camera_pitch = (self.camera_pitch + pitch).clamp(-1.5, 1.5);
        self.accumulated_frames = 0;
    }

    pub fn cycle_debug_view(&mut self) {
        let previous = self.debug_view;
        self.debug_view = self.debug_view.next();
        if self.upscaler.uses_streamline()
            && (previous == DebugView::Final || self.debug_view == DebugView::Final)
        {
            self.request_history_reset();
            self.accumulated_frames = 0;
        }
        eprintln!(
            "debug_view_changed index={} name={}",
            self.debug_view.index(),
            self.debug_view.name()
        );
    }

    pub fn debug_view_name(&self) -> &'static str {
        self.debug_view.title()
    }

    unsafe fn create_render_targets(&mut self) -> Result<()> {
        for index in 0..FRAME_COUNT {
            let resource: ID3D12Resource =
                unsafe { self.active_swap_chain().GetBuffer(index as u32)? };
            let handle = self.rtv_heap.cpu_handle(index);
            unsafe { self.device.CreateRenderTargetView(&resource, None, handle) };
            self.render_targets[index] = Some(TrackedResource::new(
                resource,
                D3D12_RESOURCE_STATE_PRESENT,
                DXGI_FORMAT_R8G8B8A8_UNORM,
                self.width,
                self.height,
                format!("交换链缓冲 {index}"),
            ));
        }
        Ok(())
    }

    fn collect_frame_input_transitions(&mut self, state: D3D12_RESOURCE_STATES) {
        let generation = &mut self.active_generation;
        generation
            .raw_diffuse
            .collect_transition(&mut self.transition_batch, state);
        generation
            .raw_specular
            .collect_transition(&mut self.transition_batch, state);
        generation
            .gbuffer_albedo
            .collect_transition(&mut self.transition_batch, state);
        generation
            .gbuffer_normal_roughness
            .collect_transition(&mut self.transition_batch, state);
        generation
            .gbuffer_depth
            .collect_transition(&mut self.transition_batch, state);
        generation
            .gbuffer_motion
            .collect_transition(&mut self.transition_batch, state);
        #[cfg(feature = "streamline")]
        if let Some(dlss) = generation.dlss.as_mut() {
            dlss.depth
                .collect_transition(&mut self.transition_batch, state);
            dlss.motion
                .collect_transition(&mut self.transition_batch, state);
        }
        #[cfg(feature = "streamline-rr")]
        if let Some(rr) = generation.rr.as_mut() {
            rr.depth
                .collect_transition(&mut self.transition_batch, state);
            rr.motion
                .collect_transition(&mut self.transition_batch, state);
            rr.specular_motion
                .collect_transition(&mut self.transition_batch, state);
        }
        generation
            .gbuffer_id
            .collect_transition(&mut self.transition_batch, state);
        generation
            .gbuffer_world_position
            .collect_transition(&mut self.transition_batch, state);
        generation
            .gbuffer_hit_distance
            .collect_transition(&mut self.transition_batch, state);
        generation
            .reconstruction_noisy_hdr
            .collect_transition(&mut self.transition_batch, state);
        generation
            .reconstruction_diffuse_albedo
            .collect_transition(&mut self.transition_batch, state);
        generation
            .reconstruction_specular_albedo
            .collect_transition(&mut self.transition_batch, state);
        generation
            .reconstruction_normal_roughness
            .collect_transition(&mut self.transition_batch, state);
        generation
            .reconstruction_view_z
            .collect_transition(&mut self.transition_batch, state);
        generation
            .reconstruction_motion
            .collect_transition(&mut self.transition_batch, state);
        generation
            .reconstruction_diffuse_hit_distance
            .collect_transition(&mut self.transition_batch, state);
        generation
            .reconstruction_specular_hit_distance
            .collect_transition(&mut self.transition_batch, state);
        generation
            .reconstruction_primary_emissive
            .collect_transition(&mut self.transition_batch, state);
        if let Some(stable) = generation.stable_planes.as_mut() {
            stable.collect_all(&mut self.transition_batch, state);
        }
        #[cfg(feature = "nrd")]
        if let Some(nrd) = generation.nrd.as_mut() {
            for resource in [
                &mut nrd.transmission_raw_diffuse,
                &mut nrd.transmission_raw_specular,
                &mut nrd.transmission_base_color,
                &mut nrd.transmission_normal_roughness,
                &mut nrd.transmission_view_z,
                &mut nrd.transmission_motion,
                &mut nrd.transmission_diffuse_hit_distance,
                &mut nrd.transmission_specular_hit_distance,
                &mut nrd.transmission_primary_emissive,
                &mut nrd.transmission_diffuse_albedo,
                &mut nrd.transmission_view_proxy,
            ] {
                resource.collect_transition(&mut self.transition_batch, state);
            }
        }
        generation
            .nrd_validation
            .collect_transition(&mut self.transition_batch, state);
    }

    #[cfg(feature = "nrd")]
    fn record_nrd_path(
        &mut self,
        frame_index: usize,
        render_groups_x: u32,
        render_groups_y: u32,
        command_recording_stats: &mut CommandRecordingFrameStats,
    ) -> Result<()> {
        if self.active_path_space.uses_stable_planes() {
            return self.record_nrd_stable_path(
                frame_index,
                render_groups_x,
                render_groups_y,
                command_recording_stats,
            );
        }
        self.collect_frame_input_transitions(D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
        {
            let generation = &mut self.active_generation;
            let nrd = generation.nrd.as_mut().ok_or_else(|| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "NRD generation resources 未初始化",
                )
            })?;
            for layer in &mut nrd.layers[..2] {
                for resource in [
                    &mut layer.diffuse_input,
                    &mut layer.specular_input,
                    &mut layer.normal_roughness,
                    &mut layer.motion,
                    &mut layer.view_z,
                    &mut layer.diffuse_factor,
                    &mut layer.specular_factor,
                    &mut layer.diffuse_output,
                    &mut layer.specular_output,
                ] {
                    resource.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                }
            }
        }
        self.submit_transition_batch(command_recording_stats);

        self.gpu_profiler
            .begin(&self.command_list, frame_index, GpuPass::NrdPrep);
        self.gpu_profiler
            .begin_event(&self.command_list, GpuPass::NrdPrep);
        self.nrd_prep_pipeline.bind(
            &self.command_list,
            self.active_generation
                .shader_heap
                .gpu_handle(NRD_PREP_TABLE_BASE),
            &self
                .reconstruction_frame_state
                .camera_position
                .map(f32::to_bits),
        );
        unsafe {
            self.command_list
                .Dispatch(render_groups_x, render_groups_y, 1);
        }
        self.nrd_prep_pipeline.set_arguments(
            &self.command_list,
            self.active_generation
                .shader_heap
                .gpu_handle(NRD_TRANSMISSION_PREP_TABLE_BASE),
            &self
                .reconstruction_frame_state
                .camera_position
                .map(f32::to_bits),
        );
        unsafe {
            self.command_list
                .Dispatch(render_groups_x, render_groups_y, 1);
        }
        self.gpu_profiler
            .end(&self.command_list, frame_index, GpuPass::NrdPrep);
        self.gpu_profiler.end_event(&self.command_list);

        {
            let generation = &mut self.active_generation;
            let nrd = generation.nrd.as_mut().ok_or_else(|| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "NRD generation resources 未初始化",
                )
            })?;
            for layer in &mut nrd.layers[..2] {
                for resource in [
                    &mut layer.diffuse_input,
                    &mut layer.specular_input,
                    &mut layer.normal_roughness,
                    &mut layer.motion,
                    &mut layer.view_z,
                ] {
                    resource.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE
                            | D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                    );
                }
                for resource in [&mut layer.diffuse_factor, &mut layer.specular_factor] {
                    resource.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                }
            }
        }
        if self.debug_view == DebugView::NrdValidation {
            self.active_generation.nrd_validation.collect_transition(
                &mut self.transition_batch,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            );
        }
        self.submit_transition_batch(command_recording_stats);

        let frame_state = self.reconstruction_frame_state;
        let enable_validation = self.debug_view == DebugView::NrdValidation;
        let (mut primary_bridge_resources, mut transmission_bridge_resources) = {
            let generation = &self.active_generation;
            let nrd = generation.nrd.as_ref().ok_or_else(|| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "NRD generation resources 未初始化",
                )
            })?;
            let validation = if enable_validation {
                bridge_resource(&generation.nrd_validation)
            } else {
                reconstruction::NrdBridgeResource::default()
            };
            (
                bridge_resources_for_layer(&nrd.layers[0], validation),
                bridge_resources_for_layer(
                    &nrd.layers[1],
                    reconstruction::NrdBridgeResource::default(),
                ),
            )
        };
        self.gpu_profiler
            .begin(&self.command_list, frame_index, GpuPass::NrdDenoise);
        self.gpu_profiler
            .begin_event(&self.command_list, GpuPass::NrdDenoise);
        {
            let generation = &mut self.active_generation;
            let nrd = generation.nrd.as_mut().ok_or_else(|| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "NRD generation resources 未初始化",
                )
            })?;
            let primary_result = unsafe {
                nrd.layers[0].backend.denoise(
                    &frame_state,
                    &mut primary_bridge_resources,
                    &self.command_list,
                    enable_validation,
                )
            };
            if let Err(error) = primary_result {
                eprintln!(
                    "nrd_dispatch_failed layer=primary status=error frame={} error={error}",
                    frame_state.frame_index
                );
                return Err(error);
            }
            let transmission_result = unsafe {
                nrd.layers[1].backend.denoise(
                    &frame_state,
                    &mut transmission_bridge_resources,
                    &self.command_list,
                    false,
                )
            };
            if let Err(error) = transmission_result {
                eprintln!(
                    "nrd_dispatch_failed layer=transmission status=error frame={} error={error}",
                    frame_state.frame_index
                );
                return Err(error);
            }
        }
        self.gpu_profiler
            .end(&self.command_list, frame_index, GpuPass::NrdDenoise);
        self.gpu_profiler.end_event(&self.command_list);

        {
            let generation = &mut self.active_generation;
            let nrd = generation.nrd.as_mut().ok_or_else(|| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "NRD generation resources 未初始化",
                )
            })?;
            apply_bridge_resource_states(&mut nrd.layers[0], &primary_bridge_resources)?;
            apply_bridge_resource_states(&mut nrd.layers[1], &transmission_bridge_resources)?;
            if enable_validation {
                set_bridge_resource_state(
                    &mut generation.nrd_validation,
                    primary_bridge_resources.validation_output.state,
                )?;
            }
        }

        // NRD owns an internal descriptor heap while recording. Restore both
        // application heaps before any following pipeline/root binding.
        unsafe {
            self.command_list.SetDescriptorHeaps(&[
                Some(self.active_generation.shader_heap.heap().clone()),
                Some(self._sampler_heap.heap().clone()),
            ]);
        }
        {
            let generation = &mut self.active_generation;
            let nrd = generation.nrd.as_mut().ok_or_else(|| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "NRD generation resources 未初始化",
                )
            })?;
            for layer in &mut nrd.layers[..2] {
                for resource in [&mut layer.diffuse_output, &mut layer.specular_output] {
                    resource.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                }
            }
        }
        self.active_generation
            .filter_diffuse_pong
            .collect_transition(
                &mut self.transition_batch,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            );
        self.active_generation
            .filter_specular_pong
            .collect_transition(
                &mut self.transition_batch,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            );
        if enable_validation {
            self.active_generation.nrd_validation.collect_transition(
                &mut self.transition_batch,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );
        }
        self.submit_transition_batch(command_recording_stats);

        self.gpu_profiler
            .begin(&self.command_list, frame_index, GpuPass::NrdCompose);
        self.gpu_profiler
            .begin_event(&self.command_list, GpuPass::NrdCompose);
        self.nrd_compose_pipeline.bind(
            &self.command_list,
            self.active_generation
                .shader_heap
                .gpu_handle(NRD_COMPOSE_TABLE_BASE),
            &[0],
        );
        unsafe {
            self.command_list
                .Dispatch(render_groups_x, render_groups_y, 1);
        }
        self.gpu_profiler
            .end(&self.command_list, frame_index, GpuPass::NrdCompose);
        self.gpu_profiler.end_event(&self.command_list);
        self.active_generation
            .filter_diffuse_pong
            .collect_transition(
                &mut self.transition_batch,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );
        self.active_generation
            .filter_specular_pong
            .collect_transition(
                &mut self.transition_batch,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );
        self.submit_transition_batch(command_recording_stats);
        Ok(())
    }

    #[cfg(feature = "nrd")]
    fn record_nrd_stable_path(
        &mut self,
        frame_index: usize,
        render_groups_x: u32,
        render_groups_y: u32,
        command_recording_stats: &mut CommandRecordingFrameStats,
    ) -> Result<()> {
        self.collect_frame_input_transitions(D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
        {
            let generation = &mut self.active_generation;
            let nrd = generation.nrd.as_mut().ok_or_else(|| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "stable-plane NRD generation resources 未初始化",
                )
            })?;
            if nrd.layers.len() != crate::path_space::STABLE_PLANE_COUNT {
                return Err(WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "stable-plane NRD history 数量与 plane contract 不一致",
                ));
            }
            for layer in &mut nrd.layers {
                for resource in [
                    &mut layer.diffuse_input,
                    &mut layer.specular_input,
                    &mut layer.normal_roughness,
                    &mut layer.motion,
                    &mut layer.view_z,
                    &mut layer.diffuse_factor,
                    &mut layer.specular_factor,
                    &mut layer.diffuse_output,
                    &mut layer.specular_output,
                ] {
                    resource.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    );
                }
            }
        }
        self.submit_transition_batch(command_recording_stats);

        self.gpu_profiler
            .begin(&self.command_list, frame_index, GpuPass::NrdPrep);
        self.gpu_profiler
            .begin_event(&self.command_list, GpuPass::NrdPrep);
        for plane_index in 0..crate::path_space::STABLE_PLANE_COUNT {
            let prep_pass = match plane_index {
                0 => GpuPass::NrdStablePrep0,
                1 => GpuPass::NrdStablePrep1,
                _ => GpuPass::NrdStablePrep2,
            };
            self.gpu_profiler
                .begin(&self.command_list, frame_index, prep_pass);
            self.gpu_profiler.begin_event(&self.command_list, prep_pass);
            self.nrd_stable_prep_pipeline.bind(
                &self.command_list,
                self.active_generation
                    .shader_heap
                    .gpu_handle(NRD_STABLE_PREP_TABLE_BASES[plane_index]),
                &[plane_index as u32],
            );
            unsafe {
                self.command_list
                    .Dispatch(render_groups_x, render_groups_y, 1);
            }
            self.gpu_profiler
                .end(&self.command_list, frame_index, prep_pass);
            self.gpu_profiler.end_event(&self.command_list);
        }
        self.gpu_profiler
            .end(&self.command_list, frame_index, GpuPass::NrdPrep);
        self.gpu_profiler.end_event(&self.command_list);

        {
            let generation = &mut self.active_generation;
            let nrd = generation.nrd.as_mut().ok_or_else(|| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "stable-plane NRD generation resources 未初始化",
                )
            })?;
            for layer in &mut nrd.layers {
                for resource in [
                    &mut layer.diffuse_input,
                    &mut layer.specular_input,
                    &mut layer.normal_roughness,
                    &mut layer.motion,
                    &mut layer.view_z,
                ] {
                    resource.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE
                            | D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                    );
                }
                for resource in [&mut layer.diffuse_factor, &mut layer.specular_factor] {
                    resource.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                }
            }
        }
        let enable_validation = self.debug_view == DebugView::NrdValidation;
        if enable_validation {
            self.active_generation.nrd_validation.collect_transition(
                &mut self.transition_batch,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            );
        }
        self.submit_transition_batch(command_recording_stats);

        let frame_state = self.reconstruction_frame_state;
        let mut bridge_layers = {
            let generation = &self.active_generation;
            let nrd = generation.nrd.as_ref().ok_or_else(|| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "stable-plane NRD generation resources 未初始化",
                )
            })?;
            nrd.layers
                .iter()
                .enumerate()
                .map(|(plane_index, layer)| {
                    let validation = if enable_validation && plane_index == 0 {
                        bridge_resource(&generation.nrd_validation)
                    } else {
                        reconstruction::NrdBridgeResource::default()
                    };
                    bridge_resources_for_layer(layer, validation)
                })
                .collect::<Vec<_>>()
        };

        self.gpu_profiler
            .begin(&self.command_list, frame_index, GpuPass::NrdDenoise);
        self.gpu_profiler
            .begin_event(&self.command_list, GpuPass::NrdDenoise);
        {
            let generation = &mut self.active_generation;
            let nrd = generation.nrd.as_mut().ok_or_else(|| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "stable-plane NRD generation resources 未初始化",
                )
            })?;
            for (plane_index, (layer, resources)) in nrd
                .layers
                .iter_mut()
                .zip(bridge_layers.iter_mut())
                .enumerate()
            {
                let denoise_pass = match plane_index {
                    0 => GpuPass::NrdStableDenoise0,
                    1 => GpuPass::NrdStableDenoise1,
                    _ => GpuPass::NrdStableDenoise2,
                };
                self.gpu_profiler
                    .begin(&self.command_list, frame_index, denoise_pass);
                self.gpu_profiler
                    .begin_event(&self.command_list, denoise_pass);
                let result = unsafe {
                    layer.backend.denoise(
                        &frame_state,
                        resources,
                        &self.command_list,
                        enable_validation && plane_index == 0,
                    )
                };
                self.gpu_profiler
                    .end(&self.command_list, frame_index, denoise_pass);
                self.gpu_profiler.end_event(&self.command_list);
                if let Err(error) = result {
                    eprintln!(
                        "nrd_dispatch_failed layer=stable-plane-{plane_index} status=error frame={} error={error}",
                        frame_state.frame_index
                    );
                    return Err(error);
                }
            }
        }
        self.gpu_profiler
            .end(&self.command_list, frame_index, GpuPass::NrdDenoise);
        self.gpu_profiler.end_event(&self.command_list);

        {
            let generation = &mut self.active_generation;
            let nrd = generation.nrd.as_mut().ok_or_else(|| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "stable-plane NRD generation resources 未初始化",
                )
            })?;
            for (layer, resources) in nrd.layers.iter_mut().zip(&bridge_layers) {
                apply_bridge_resource_states(layer, resources)?;
            }
            if enable_validation {
                set_bridge_resource_state(
                    &mut generation.nrd_validation,
                    bridge_layers[0].validation_output.state,
                )?;
            }
        }

        // Every NRD Integration instance temporarily owns the descriptor heap.
        // Restore the application heaps once after all independent histories.
        unsafe {
            self.command_list.SetDescriptorHeaps(&[
                Some(self.active_generation.shader_heap.heap().clone()),
                Some(self._sampler_heap.heap().clone()),
            ]);
        }
        {
            let generation = &mut self.active_generation;
            let nrd = generation.nrd.as_mut().ok_or_else(|| {
                WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "stable-plane NRD generation resources 未初始化",
                )
            })?;
            for layer in &mut nrd.layers {
                for resource in [&mut layer.diffuse_output, &mut layer.specular_output] {
                    resource.collect_transition(
                        &mut self.transition_batch,
                        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                    );
                }
            }
        }
        self.active_generation
            .filter_diffuse_pong
            .collect_transition(
                &mut self.transition_batch,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            );
        self.active_generation
            .filter_specular_pong
            .collect_transition(
                &mut self.transition_batch,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            );
        if enable_validation {
            self.active_generation.nrd_validation.collect_transition(
                &mut self.transition_batch,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );
        }
        self.submit_transition_batch(command_recording_stats);

        self.gpu_profiler
            .begin(&self.command_list, frame_index, GpuPass::NrdCompose);
        self.gpu_profiler
            .begin_event(&self.command_list, GpuPass::NrdCompose);
        for plane_index in (0..crate::path_space::STABLE_PLANE_COUNT).rev() {
            let compose_pass = match plane_index {
                0 => GpuPass::NrdStableCompose0,
                1 => GpuPass::NrdStableCompose1,
                _ => GpuPass::NrdStableCompose2,
            };
            self.gpu_profiler
                .begin(&self.command_list, frame_index, compose_pass);
            self.gpu_profiler
                .begin_event(&self.command_list, compose_pass);
            self.nrd_stable_compose_pipeline.bind(
                &self.command_list,
                self.active_generation
                    .shader_heap
                    .gpu_handle(NRD_STABLE_COMPOSE_TABLE_BASES[plane_index]),
                &[
                    plane_index as u32,
                    u32::from(plane_index + 1 == crate::path_space::STABLE_PLANE_COUNT),
                ],
            );
            unsafe {
                self.command_list
                    .Dispatch(render_groups_x, render_groups_y, 1);
            }
            self.gpu_profiler
                .end(&self.command_list, frame_index, compose_pass);
            self.gpu_profiler.end_event(&self.command_list);
            submit_global_uav_barrier(&self.command_list);
        }
        self.gpu_profiler
            .end(&self.command_list, frame_index, GpuPass::NrdCompose);
        self.gpu_profiler.end_event(&self.command_list);
        self.active_generation
            .filter_diffuse_pong
            .collect_transition(
                &mut self.transition_batch,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );
        self.active_generation
            .filter_specular_pong
            .collect_transition(
                &mut self.transition_batch,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );
        self.submit_transition_batch(command_recording_stats);
        Ok(())
    }

    fn submit_transition_batch(&mut self, stats: &mut CommandRecordingFrameStats) {
        let mode = match self.command_recording_mode {
            CommandRecordingMode::Baseline => BarrierSubmissionMode::Immediate,
            CommandRecordingMode::Optimized => BarrierSubmissionMode::Batched,
        };
        let submitted = self.transition_batch.submit(&self.command_list, mode);
        stats.add_transition_submission(submitted.api_calls, submitted.transitions);
    }

    fn bind_atrous_arguments(
        &self,
        step_width: u32,
        descriptor_table: D3D12_GPU_DESCRIPTOR_HANDLE,
        constants: &[u32],
        previous_pipeline: &mut Option<*const ComputePipeline>,
        stats: &mut CommandRecordingFrameStats,
    ) {
        let pipeline = self.atrous_pipeline_for_step(step_width);
        let identity = pipeline as *const ComputePipeline;
        if should_bind_atrous_pipeline(self.command_recording_mode, *previous_pipeline, identity) {
            pipeline.bind_pipeline(&self.command_list);
            *previous_pipeline = Some(identity);
            stats.atrous_pipeline_binds = stats.atrous_pipeline_binds.saturating_add(1);
        }
        pipeline.set_arguments(&self.command_list, descriptor_table, constants);
        stats.atrous_argument_updates = stats.atrous_argument_updates.saturating_add(1);
    }

    fn poll_shader_reload(&mut self) {
        let Some(result) = self.shader_reloader.poll() else {
            return;
        };
        match result {
            Ok(shaders) => match self.rebuild_shader_pipelines(shaders) {
                Ok(()) => {
                    self.shader_status = "热重载成功".to_string();
                    eprintln!("Shader 热重载成功");
                }
                Err(error) => {
                    self.shader_status = format!("热重载管线失败：{error}");
                    eprintln!("{}", self.shader_status);
                }
            },
            Err(error) => {
                self.shader_status = format!("热重载编译失败：{error}");
                eprintln!("{}", self.shader_status);
            }
        }
    }

    fn rebuild_shader_pipelines(&mut self, shaders: ReloadedShaders) -> Result<()> {
        unsafe { self.wait_for_gpu()? };
        self.gpu_profiler.invalidate();
        self.stable_plane_counter_telemetry = Default::default();
        for frame in &mut self.frames {
            frame.timing_valid = false;
            frame.timing_generation_id = 0;
            frame.timing_passes = [false; profiler::PASS_COUNT];
            frame.stable_counter_pending = false;
            frame.stable_counter_valid = false;
            frame.stable_counter_generation_id = 0;
            frame.stable_counter_extent = Extent2D {
                width: 0,
                height: 0,
            };
        }
        let raytracing = RaytracingPipeline::new(&self.device, &shaders.raytracing)?;
        let temporal = ComputePipeline::new(
            &self.device,
            &shaders.temporal,
            18,
            10,
            1,
            "阶段 6 时域重投影",
        )?;
        let atrous_baseline = ComputePipeline::new(
            &self.device,
            &shaders.atrous,
            8,
            2,
            2,
            "阶段 8 À-Trous Baseline",
        )?;
        let atrous_shared = ComputePipeline::new(
            &self.device,
            &shaders.atrous_shared,
            8,
            2,
            2,
            "阶段 8 À-Trous Shared 1/2",
        )?;
        let tonemap = ComputePipeline::new(
            &self.device,
            &shaders.tonemap,
            14,
            1,
            3,
            "Tone Map 与调试视图",
        )?;
        let stable_plane_build = ComputePipeline::new_with_root_srv(
            &self.device,
            &shaders.stable_plane_build,
            4,
            6,
            16,
            4,
            "阶段 11 RTXPT-style BuildStablePlanes",
        )?;
        #[cfg(feature = "streamline")]
        let dlss_compose = ComputePipeline::new(
            &self.device,
            &shaders.dlss_compose,
            2,
            2,
            1,
            "阶段 10 DLSS HDR 合成",
        )?;
        #[cfg(feature = "streamline-rr")]
        let rr_input = ComputePipeline::new(
            &self.device,
            &shaders.rr_input,
            3,
            3,
            1,
            "阶段 11 DLSS RR 输入分层与 guide 适配",
        )?;
        #[cfg(feature = "streamline-rr")]
        let rr_stable_input = ComputePipeline::new(
            &self.device,
            &shaders.rr_stable_input,
            7,
            6,
            1,
            "阶段 11 stable-plane DLSS RR 单次合并输入",
        )?;
        #[cfg(feature = "streamline-rr")]
        let rr_emissive = ComputePipeline::new(
            &self.device,
            &shaders.rr_emissive,
            3,
            1,
            3,
            "阶段 11 DLSS RR primary emissive 稳定层",
        )?;
        #[cfg(feature = "streamline-rr")]
        let rr_primary_visibility = ComputePipeline::new(
            &self.device,
            &shaders.rr_primary_visibility,
            5,
            3,
            16,
            "阶段 11 RR stable physical and virtual visibility",
        )?;
        #[cfg(feature = "streamline-rr")]
        let rr_boundary_resolve = ComputePipeline::new(
            &self.device,
            &shaders.rr_boundary_resolve,
            7,
            2,
            1,
            "阶段 11 RR opaque boundary resolve",
        )?;
        #[cfg(feature = "nrd")]
        let nrd_prep = ComputePipeline::new(
            &self.device,
            &shaders.nrd_prep,
            11,
            7,
            3,
            "阶段 9 NRD REBLUR 输入准备",
        )?;
        #[cfg(feature = "nrd")]
        let nrd_compose = ComputePipeline::new(
            &self.device,
            &shaders.nrd_compose,
            11,
            2,
            1,
            "阶段 9 NRD REBLUR 输出合成",
        )?;
        #[cfg(feature = "nrd")]
        let nrd_stable_prep = ComputePipeline::new(
            &self.device,
            &shaders.nrd_stable_prep,
            4,
            7,
            1,
            "阶段 11 stable-plane NRD 输入准备",
        )?;
        #[cfg(feature = "nrd")]
        let nrd_stable_compose = ComputePipeline::new(
            &self.device,
            &shaders.nrd_stable_compose,
            6,
            2,
            2,
            "阶段 11 stable-plane NRD 反向合成",
        )?;
        self.raytracing_pipeline = raytracing;
        self.temporal_pipeline = temporal;
        self.atrous_baseline_pipeline = atrous_baseline;
        self.atrous_shared_pipeline = atrous_shared;
        self.tonemap_pipeline = tonemap;
        self.stable_plane_build_pipeline = stable_plane_build;
        #[cfg(feature = "streamline")]
        {
            self.dlss_compose_pipeline = dlss_compose;
        }
        #[cfg(feature = "streamline-rr")]
        {
            self.rr_input_pipeline = rr_input;
            self.rr_stable_input_pipeline = rr_stable_input;
            self.rr_emissive_pipeline = rr_emissive;
            self.rr_primary_visibility_pipeline = rr_primary_visibility;
            self.rr_boundary_resolve_pipeline = rr_boundary_resolve;
        }
        #[cfg(feature = "nrd")]
        {
            self.nrd_prep_pipeline = nrd_prep;
            self.nrd_compose_pipeline = nrd_compose;
            self.nrd_stable_prep_pipeline = nrd_stable_prep;
            self.nrd_stable_compose_pipeline = nrd_stable_compose;
        }
        self.request_history_reset();
        self.accumulated_frames = 0;
        if let Some(controller) = self.dynamic_resolution.as_mut() {
            controller.reset_after_discontinuity(self.requested_render_scale);
        }
        Ok(())
    }

    fn request_history_reset(&mut self) {
        if !self.reset_history {
            self.history_reset_count = self.history_reset_count.saturating_add(1);
        }
        self.stable_plane_counter_telemetry = Default::default();
        for frame in &mut self.frames {
            // History resets do not necessarily create a render generation.
            // Mark same-generation pending slices stale before they complete.
            frame.stable_counter_valid = false;
        }
        self.reset_history = true;
    }

    unsafe fn wait_for_frame(&self, frame_index: usize) -> Result<()> {
        let fence_value = self.frames[frame_index].fence_value;
        if fence_value != 0 && unsafe { self.fence.GetCompletedValue() } < fence_value {
            unsafe {
                self.fence
                    .SetEventOnCompletion(fence_value, self.fence_event)?;
                WaitForSingleObject(self.fence_event, INFINITE);
            }
        }
        Ok(())
    }

    unsafe fn wait_for_gpu(&mut self) -> Result<()> {
        self.gpu_idle_wait_count = self.gpu_idle_wait_count.saturating_add(1);
        let fence_value = self.next_fence_value;
        self.next_fence_value += 1;
        unsafe {
            self.command_queue.Signal(&self.fence, fence_value)?;
            self.fence
                .SetEventOnCompletion(fence_value, self.fence_event)?;
            WaitForSingleObject(self.fence_event, INFINITE);
        }
        Ok(())
    }
}

fn create_readback_buffer(device: &ID3D12Device, byte_size: u64) -> Result<ID3D12Resource> {
    let heap_properties = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_READBACK,
        ..Default::default()
    };
    let description = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: byte_size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        ..Default::default()
    };
    let mut resource = None;
    unsafe {
        device.CreateCommittedResource(
            &heap_properties,
            D3D12_HEAP_FLAG_NONE,
            &description,
            D3D12_RESOURCE_STATE_COPY_DEST,
            None,
            &mut resource,
        )?;
    }
    resource.ok_or_else(|| {
        WindowsError::new(
            windows::core::HRESULT(0x80004005_u32 as i32),
            "创建 stable-plane counter readback 失败：D3D12 未返回资源",
        )
    })
}

unsafe fn create_structured_srv(
    device: &ID3D12Device,
    heap: &DescriptorHeap,
    index: usize,
    resource: &ID3D12Resource,
    element_count: u32,
    stride: u32,
) {
    let description = D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_UNKNOWN,
        ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Buffer: D3D12_BUFFER_SRV {
                FirstElement: 0,
                NumElements: element_count,
                StructureByteStride: stride,
                Flags: D3D12_BUFFER_SRV_FLAG_NONE,
            },
        },
    };
    unsafe {
        device.CreateShaderResourceView(resource, Some(&description), heap.cpu_handle(index))
    };
}

unsafe fn create_texture_srv(
    device: &ID3D12Device,
    heap: &DescriptorHeap,
    index: usize,
    resource: &TrackedResource,
) {
    unsafe {
        device.CreateShaderResourceView(resource.resource(), None, heap.cpu_handle(index));
    }
}

#[cfg(feature = "streamline")]
unsafe fn create_null_texture_srv(
    device: &ID3D12Device,
    heap: &DescriptorHeap,
    index: usize,
    format: DXGI_FORMAT,
) {
    let description = D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D12_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
                PlaneSlice: 0,
                ResourceMinLODClamp: 0.0,
            },
        },
    };
    unsafe {
        // A typed null SRV is the neutral descriptor for an unused texture:
        // any accidental read returns zero instead of duplicating HDR energy.
        device.CreateShaderResourceView(None, Some(&description), heap.cpu_handle(index));
    }
}

unsafe fn create_texture_uav(
    device: &ID3D12Device,
    heap: &DescriptorHeap,
    index: usize,
    resource: &TrackedResource,
) {
    unsafe {
        device.CreateUnorderedAccessView(resource.resource(), None, None, heap.cpu_handle(index));
    }
}

unsafe fn create_structured_uav(
    device: &ID3D12Device,
    heap: &DescriptorHeap,
    index: usize,
    resource: &TrackedResource,
    element_count: u32,
    stride: u32,
) {
    let description = D3D12_UNORDERED_ACCESS_VIEW_DESC {
        Format: DXGI_FORMAT_UNKNOWN,
        ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
        Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
            Buffer: D3D12_BUFFER_UAV {
                FirstElement: 0,
                NumElements: element_count,
                StructureByteStride: stride,
                CounterOffsetInBytes: 0,
                Flags: D3D12_BUFFER_UAV_FLAG_NONE,
            },
        },
    };
    unsafe {
        device.CreateUnorderedAccessView(
            resource.resource(),
            None,
            Some(&description),
            heap.cpu_handle(index),
        );
    }
}

unsafe fn create_null_structured_uav(
    device: &ID3D12Device,
    heap: &DescriptorHeap,
    index: usize,
    stride: u32,
) {
    let description = D3D12_UNORDERED_ACCESS_VIEW_DESC {
        Format: DXGI_FORMAT_UNKNOWN,
        ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
        Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
            Buffer: D3D12_BUFFER_UAV {
                FirstElement: 0,
                NumElements: 1,
                StructureByteStride: stride,
                CounterOffsetInBytes: 0,
                Flags: D3D12_BUFFER_UAV_FLAG_NONE,
            },
        },
    };
    unsafe {
        device.CreateUnorderedAccessView(None, None, Some(&description), heap.cpu_handle(index));
    }
}

unsafe fn create_null_texture_array_uav(
    device: &ID3D12Device,
    heap: &DescriptorHeap,
    index: usize,
    format: DXGI_FORMAT,
) {
    let description = D3D12_UNORDERED_ACCESS_VIEW_DESC {
        Format: format,
        ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2DARRAY,
        Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
            Texture2DArray: D3D12_TEX2D_ARRAY_UAV {
                MipSlice: 0,
                FirstArraySlice: 0,
                ArraySize: crate::path_space::STABLE_PLANE_COUNT as u32,
                PlaneSlice: 0,
            },
        },
    };
    unsafe {
        device.CreateUnorderedAccessView(None, None, Some(&description), heap.cpu_handle(index));
    }
}

unsafe fn populate_texture_table(
    device: &ID3D12Device,
    heap: &DescriptorHeap,
    base: usize,
    srvs: &[&TrackedResource],
    uavs: &[&TrackedResource],
) {
    debug_assert!(base + srvs.len() + uavs.len() <= SHADER_DESCRIPTOR_COUNT);
    for (offset, resource) in srvs.iter().enumerate() {
        unsafe { create_texture_srv(device, heap, base + offset, resource) };
    }
    for (offset, resource) in uavs.iter().enumerate() {
        unsafe { create_texture_uav(device, heap, base + srvs.len() + offset, resource) };
    }
}

fn require_raytracing_tier_1_1(device: &ID3D12Device) -> Result<String> {
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS5::default();
    unsafe {
        device.CheckFeatureSupport(
            D3D12_FEATURE_D3D12_OPTIONS5,
            (&mut options as *mut D3D12_FEATURE_DATA_D3D12_OPTIONS5).cast(),
            std::mem::size_of::<D3D12_FEATURE_DATA_D3D12_OPTIONS5>() as u32,
        )?;
    }
    if options.RaytracingTier.0 < D3D12_RAYTRACING_TIER_1_1.0 {
        return Err(WindowsError::new(
            windows::core::HRESULT(0x80004005_u32 as i32),
            format!(
                "需要 DXR Tier 1.1，当前设备为 Tier {}",
                options.RaytracingTier.0 as f32 / 10.0
            ),
        ));
    }
    Ok(format!(
        "DXR Tier {}",
        options.RaytracingTier.0 as f32 / 10.0
    ))
}

impl Drop for Dx12Renderer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.wait_for_gpu();
            #[cfg(feature = "streamline")]
            self.reclaim_retired_generations();
            #[cfg(feature = "streamline")]
            if let (Some(runtime), Some(viewport)) = (
                self.streamline.as_ref(),
                self.active_streamline_viewport.as_mut(),
            ) {
                runtime.free_resources(viewport);
            }
            #[cfg(feature = "streamline")]
            if let Some(streamline) = self.streamline.take() {
                // Streamline requires slShutdown before destroying DXGI/D3D12
                // components. Keep the upgraded proxy swap chain alive until
                // shutdown has finished, then release its owned reference.
                streamline.shutdown_after_gpu();
            }
            #[cfg(feature = "streamline")]
            drop(self.streamline_swap_chain.take());
            if cfg!(debug_assertions) {
                report_debug_messages(&self.device);
            }
            let _ = CloseHandle(self.fence_event);
        }
    }
}

unsafe fn create_hardware_device(
    factory: &IDXGIFactory6,
) -> Result<(ID3D12Device, IDXGIAdapter1, String)> {
    let mut adapter_index = 0;
    loop {
        let adapter = match unsafe {
            factory.EnumAdapterByGpuPreference::<IDXGIAdapter1>(
                adapter_index,
                DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
            )
        } {
            Ok(adapter) => adapter,
            Err(_) => {
                return Err(WindowsError::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "未找到同时支持 D3D12 Feature Level 12_0 和 DXR Tier 1.1 的硬件适配器",
                ));
            }
        };
        adapter_index += 1;
        let description = unsafe { adapter.GetDesc1()? };
        if description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }

        let mut device = None;
        if unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device) }.is_ok() {
            let Some(device) = device else {
                continue;
            };
            if require_raytracing_tier_1_1(&device).is_err() {
                continue;
            }
            let name_end = description
                .Description
                .iter()
                .position(|character| *character == 0)
                .unwrap_or(description.Description.len());
            let gpu_name = String::from_utf16_lossy(&description.Description[..name_end]);
            eprintln!("DX12 适配器：{}", gpu_name);
            return Ok((device, adapter, gpu_name));
        }
    }
}

fn window_hwnd(window: &Window) -> Result<HWND> {
    let handle = window.window_handle().map_err(|error| {
        windows::core::Error::new(
            windows::core::HRESULT(0x80004005_u32 as i32),
            error.to_string(),
        )
    })?;
    match handle.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(HWND(handle.hwnd.get() as *mut c_void)),
        _ => Err(windows::core::Error::new(
            windows::core::HRESULT(0x80004005_u32 as i32),
            "winit 未提供 Win32 HWND",
        )),
    }
}

fn dx_error(stage: &str, error: WindowsError) -> WindowsError {
    WindowsError::new(error.code(), format!("{stage}：{error}"))
}

fn device_removed_error(device: &ID3D12Device, present_error: WindowsError) -> WindowsError {
    let reason = unsafe { device.GetDeviceRemovedReason() }
        .err()
        .map(|error| error.to_string())
        .unwrap_or_else(|| "未报告额外原因".to_string());
    let dred_summary = device
        .cast::<ID3D12DeviceRemovedExtendedData1>()
        .ok()
        .and_then(|dred| unsafe {
            let breadcrumbs = dred.GetAutoBreadcrumbsOutput1().ok()?;
            let page_fault = dred.GetPageFaultAllocationOutput1().ok()?;
            Some(format!(
                "DRED breadcrumbs={}，page-fault VA=0x{:016X}",
                !breadcrumbs.pHeadAutoBreadcrumbNode.is_null(),
                page_fault.PageFaultVA
            ))
        })
        .unwrap_or_else(|| "DRED 输出不可用".to_string());
    WindowsError::new(
        present_error.code(),
        format!("Present 失败：{present_error}；设备原因：{reason}；{dred_summary}"),
    )
}

unsafe fn enable_debug_interfaces() {
    if !cfg!(debug_assertions) {
        return;
    }

    let mut debug: Option<ID3D12Debug1> = None;
    if unsafe { D3D12GetDebugInterface(&mut debug) }.is_ok()
        && let Some(debug) = debug
    {
        unsafe {
            debug.EnableDebugLayer();
            debug.SetEnableGPUBasedValidation(true);
        }
    }

    let mut dred: Option<ID3D12DeviceRemovedExtendedDataSettings1> = None;
    if unsafe { D3D12GetDebugInterface(&mut dred) }.is_ok()
        && let Some(dred) = dred
    {
        unsafe {
            dred.SetAutoBreadcrumbsEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
            dred.SetPageFaultEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
            dred.SetBreadcrumbContextEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
        }
    }
}

unsafe fn configure_info_queue(device: &ID3D12Device) {
    if !cfg!(debug_assertions) {
        return;
    }
    if let Ok(info_queue) = device.cast::<ID3D12InfoQueue>() {
        // Keep validation messages in the queue for the shutdown report. A
        // debugger break would terminate a standalone smoke-test process
        // before the message can be observed.
        unsafe { info_queue.ClearStoredMessages() };
    }
}

unsafe fn report_debug_messages(device: &ID3D12Device) {
    let Ok(info_queue) = device.cast::<ID3D12InfoQueue>() else {
        return;
    };
    let message_count = unsafe { info_queue.GetNumStoredMessagesAllowedByRetrievalFilter() };
    if message_count == 0 {
        eprintln!("D3D12 Debug InfoQueue：0 条消息");
        return;
    }
    let mut corruption_count = 0u64;
    let mut error_count = 0u64;
    let mut warning_count = 0u64;
    let mut info_count = 0u64;
    let mut message_only_count = 0u64;
    let mut unreadable_count = 0u64;
    for index in 0..message_count {
        let mut byte_length = 0usize;
        if unsafe { info_queue.GetMessage(index, None, &mut byte_length) }.is_err()
            || byte_length == 0
        {
            unreadable_count = unreadable_count.saturating_add(1);
            continue;
        }
        let mut storage = vec![0_u8; byte_length];
        let message = storage.as_mut_ptr().cast::<D3D12_MESSAGE>();
        if unsafe { info_queue.GetMessage(index, Some(message), &mut byte_length) }.is_err() {
            unreadable_count = unreadable_count.saturating_add(1);
            continue;
        }
        let severity = unsafe { (*message).Severity };
        if severity == D3D12_MESSAGE_SEVERITY_CORRUPTION {
            corruption_count = corruption_count.saturating_add(1);
        } else if severity == D3D12_MESSAGE_SEVERITY_ERROR {
            error_count = error_count.saturating_add(1);
        } else if severity == D3D12_MESSAGE_SEVERITY_WARNING {
            warning_count = warning_count.saturating_add(1);
        } else if severity == D3D12_MESSAGE_SEVERITY_INFO {
            info_count = info_count.saturating_add(1);
        } else {
            message_only_count = message_only_count.saturating_add(1);
        }
        if severity != D3D12_MESSAGE_SEVERITY_CORRUPTION
            && severity != D3D12_MESSAGE_SEVERITY_ERROR
            && severity != D3D12_MESSAGE_SEVERITY_WARNING
        {
            continue;
        }
        let description = if unsafe { (*message).pDescription.is_null() } {
            "<无描述>".to_string()
        } else {
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    (*message).pDescription,
                    (*message).DescriptionByteLength,
                )
            };
            String::from_utf8_lossy(bytes).into_owned()
        };
        eprintln!(
            "D3D12 Debug [{}] severity={} id={}: {}",
            index,
            debug_severity_label(severity),
            unsafe { (*message).ID.0 },
            description.trim_end_matches('\0')
        );
    }
    eprintln!(
        "D3D12 Debug InfoQueue：总计 {}，CORRUPTION {}，ERROR {}，WARNING {}，INFO {}，MESSAGE {}，读取失败 {}",
        message_count,
        corruption_count,
        error_count,
        warning_count,
        info_count,
        message_only_count,
        unreadable_count,
    );
}

fn debug_severity_label(severity: D3D12_MESSAGE_SEVERITY) -> &'static str {
    if severity == D3D12_MESSAGE_SEVERITY_CORRUPTION {
        "CORRUPTION"
    } else if severity == D3D12_MESSAGE_SEVERITY_ERROR {
        "ERROR"
    } else if severity == D3D12_MESSAGE_SEVERITY_WARNING {
        "WARNING"
    } else if severity == D3D12_MESSAGE_SEVERITY_INFO {
        "INFO"
    } else {
        "MESSAGE"
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn camera_constants_match_the_sixteen_dword_root_constant_contract() {
        assert_eq!(size_of::<CameraConstants>(), 16 * size_of::<u32>());
    }

    #[test]
    fn explicit_stable_svgf_reports_its_diagnostic_gpu_passes() {
        let active = active_gpu_passes(ReconstructionPath::Svgf, false, false, false, true);
        for pass in [
            GpuPass::StablePlaneBuild,
            GpuPass::StablePlaneFill0,
            GpuPass::StablePlaneFill1,
            GpuPass::StablePlaneFill2,
        ] {
            assert!(active[pass as usize]);
        }
        assert!(!active[GpuPass::RrStableMerge as usize]);
        assert!(!active[GpuPass::NrdStablePrep0 as usize]);
    }

    #[test]
    fn denoiser_switch_commits_path_space_as_one_generation_transaction() {
        let source = include_str!("d3d12.rs");
        let start = source.find("pub fn cycle_denoiser").unwrap();
        let end = source[start..]
            .find("fn reclaim_retired_generations")
            .map(|offset| start + offset)
            .unwrap();
        let body = &source[start..end];

        assert_eq!(body.matches("RenderResourceGeneration::new(").count(), 1);
        assert_eq!(body.matches("self.request_history_reset();").count(), 1);
        assert!(body.contains(
            "with_stable_planes: next_active_path_space.uses_stable_planes()"
        ));

        let generation_ready = body.find("let old_name = self.denoiser.as_str();").unwrap();
        let denoiser_commit = body.find("self.denoiser = next;").unwrap();
        let path_space_commit = body
            .find("self.active_path_space = next_active_path_space;")
            .unwrap();
        let history_reset = body.find("self.request_history_reset();").unwrap();
        assert!(generation_ready < denoiser_commit);
        assert!(denoiser_commit < path_space_commit);
        assert!(path_space_commit < history_reset);
    }

    #[test]
    fn material_srv_stride_tracks_the_shared_gpu_abi() {
        let resources = include_str!("d3d12/render_resources.rs");
        assert!(resources.contains("size_of::<GpuMaterial>() as u32"));
        assert!(!resources.contains("scene_geometry.material_count(),\n                64,"));
    }

    #[cfg(feature = "streamline-rr")]
    #[test]
    fn rr_and_compatible_dlss_options_share_one_viewport_contract() {
        let output_extent = Extent2D {
            width: 1920,
            height: 1080,
        };
        let dlss = StreamlineRuntime::dlss_options(
            crate::upscaler::UpscalerMode::DlssBalanced,
            output_extent,
        )
        .unwrap();
        let rr = StreamlineRuntime::rr_options(
            crate::upscaler::UpscalerMode::DlssBalanced,
            output_extent,
        )
        .unwrap();

        assert_eq!(dlss.mode, rr.mode);
        assert_eq!(dlss.output_width, rr.output_width);
        assert_eq!(dlss.output_height, rr.output_height);
        assert_eq!(dlss.pre_exposure, rr.pre_exposure);
        assert_eq!(dlss.exposure_scale, rr.exposure_scale);
        assert_eq!(dlss.color_buffers_hdr, rr.color_buffers_hdr);
        assert_eq!(dlss.alpha_upscaling_enabled, rr.alpha_upscaling_enabled);
        assert_eq!(rr.quality_preset, DLSS_RR_RENDER_PRESET);
        assert_eq!(rr.balanced_preset, DLSS_RR_RENDER_PRESET);
        assert_eq!(rr.performance_preset, DLSS_RR_RENDER_PRESET);
    }

    #[cfg(feature = "streamline-rr")]
    #[test]
    fn rr_input_adapter_bounds_dispatch_and_emits_finite_guides() {
        fn decode(encoded: [f32; 4]) -> ([f32; 3], f32) {
            let mut normal = [
                encoded[0] * 2.0 - 1.0,
                encoded[1] * 2.0 - 1.0,
                encoded[2] * 2.0 - 1.0,
            ];
            let length_squared = normal.iter().map(|value| value * value).sum::<f32>();
            let present = encoded[..3].iter().any(|value| *value != 0.0);
            if present
                && encoded[..3].iter().all(|value| value.is_finite())
                && length_squared.is_finite()
                && length_squared >= 1.0e-10
            {
                let inverse_length = length_squared.sqrt().recip();
                normal.iter_mut().for_each(|value| *value *= inverse_length);
            } else {
                normal = [0.0, 0.0, 1.0];
            }
            let roughness = if encoded[3].is_finite() {
                encoded[3].clamp(0.0, 1.0)
            } else {
                1.0
            };
            (normal, roughness)
        }

        let shader = include_str!("../../shaders/stage11_rr_input.hlsl");
        assert!(shader.contains("ReconstructionNormalRoughness.GetDimensions"));
        assert!(shader.contains("dispatchId.x >= sourceWidth || dispatchId.y >= sourceHeight"));
        assert!(shader.contains("abs(primaryKind - 3.0) < 0.25"));
        assert!(shader.contains("max(noisyHdr - primaryEmissive, 0.0)"));
        assert!(shader.contains("RrPrimaryEmissive[dispatchId.xy]"));

        let (normal, roughness) = decode([0.5, 0.5, 1.0, 0.25]);
        assert_eq!(normal, [0.0, 0.0, 1.0]);
        assert_eq!(roughness, 0.25);
        let length = normal.iter().map(|value| value * value).sum::<f32>().sqrt();
        assert!((length - 1.0).abs() <= f32::EPSILON);

        let (cleared_normal, cleared_roughness) = decode([0.0; 4]);
        assert_eq!(cleared_normal, [0.0, 0.0, 1.0]);
        assert_eq!(cleared_roughness, 0.0);
        let (invalid_normal, invalid_roughness) = decode([f32::NAN; 4]);
        assert_eq!(invalid_normal, [0.0, 0.0, 1.0]);
        assert_eq!(invalid_roughness, 1.0);
        assert!(invalid_normal.iter().all(|value| value.is_finite()));
        assert!(invalid_roughness.is_finite());
    }

    #[cfg(feature = "streamline-rr")]
    #[test]
    fn rr_primary_emissive_resolve_has_stable_lattice_and_bounded_history() {
        let shader = include_str!("../../shaders/stage11_rr_emissive.hlsl");
        assert!(shader.contains("- 0.5 - CameraJitterPx"));
        assert!(shader.contains("previousPosition = float2(dispatchId.xy) + outputMotion"));
        assert!(shader.contains("ResetHistory == 0u && previousInBounds"));
        assert!(shader.contains("if (!stationaryProjection)"));
        assert!(shader.contains("acceptedPrevious = clamp("));
        assert!(shader.contains("MaxHistorySamples = 64.0"));
        assert!(shader.contains("motionMagnitude > 2.0"));
        assert!(shader.contains("stationaryProjection && historyCount >= MaxHistorySamples"));
        assert!(shader.contains("converged ? 1.0"));
    }

    #[cfg(feature = "streamline-rr")]
    #[test]
    fn rr_primary_visibility_contract_is_output_centered_and_jitter_free() {
        let shader = include_str!("../../shaders/stage11_rr_primary_visibility.hlsl");
        assert!(shader.contains("Stage11PrimaryRayDirection"));
        assert!(shader.contains("instanceData.stableSurfaceId"));
        assert!(shader.contains("PrimaryMotion[pixel] = ResetHistory != 0u"));
        assert!(shader.contains("query.CommittedWorldToObject3x4()"));
        assert!(shader.contains("StructuredBuffer<Material> Materials : register(t4)"));
        assert!(shader.contains("bool pureLegacyMirror"));
        assert!(shader.contains("reflectedQuery.TraceRayInline"));
        assert!(shader.contains("VIRTUAL_SURFACE_BIT"));
        assert!(shader.contains("ReflectPointAcrossPlane"));
        assert!(shader.contains("virtualPreviousUv - virtualCurrentUv"));
        assert_eq!(shader.matches("JitterPadding").count(), 1);
        assert_eq!(shader.matches("FrameIndex").count(), 1);
        assert!(!shader.contains("CameraJitterPx"));
        assert!(!shader.contains("SampleOwenSobol"));

        let shared_camera = include_str!("../../shaders/stage11_camera.hlsli");
        assert!(shared_camera.contains("float2(pixel) + 0.5"));
        assert_eq!(
            shared_camera.matches("STAGE11_CAMERA_FOCAL_LENGTH").count(),
            4
        );
    }

    #[cfg(feature = "streamline-rr")]
    #[test]
    fn rr_boundary_contract_filters_taps_before_interpolation() {
        #[derive(Clone, Copy)]
        struct Tap {
            id: u32,
            normal_dot: f32,
            depth_delta: f32,
            weight: f32,
            history: f32,
        }

        fn valid_tap(tap: Tap) -> bool {
            tap.id == 7
                && tap.normal_dot >= 0.95
                && tap.depth_delta <= 0.01
                && tap.weight.is_finite()
                && tap.history.is_finite()
                && tap.history > 0.0
        }

        let taps = [
            Tap {
                id: 7,
                normal_dot: 1.0,
                depth_delta: 0.0,
                weight: 0.25,
                history: 4.0,
            },
            Tap {
                id: 8,
                normal_dot: 1.0,
                depth_delta: 0.0,
                weight: 0.25,
                history: 32.0,
            },
            Tap {
                id: 7,
                normal_dot: 0.94,
                depth_delta: 0.0,
                weight: 0.25,
                history: 32.0,
            },
            Tap {
                id: 7,
                normal_dot: 1.0,
                depth_delta: 0.02,
                weight: 0.25,
                history: 32.0,
            },
        ];
        let accepted_weight: f32 = taps
            .into_iter()
            .filter(|tap| valid_tap(*tap))
            .map(|tap| tap.weight)
            .sum();
        let accepted_count: f32 = taps
            .into_iter()
            .filter(|tap| valid_tap(*tap))
            .map(|tap| tap.weight * tap.history)
            .sum::<f32>()
            / accepted_weight;
        assert_eq!(accepted_weight, 0.25);
        assert_eq!(accepted_count, 4.0);

        let current = [0.25_f32, 0.5, 0.75];
        let non_boundary = current;
        assert_eq!(non_boundary, current);

        let boundary = include_str!("../../shaders/stage11_rr_boundary_resolve.hlsl");
        assert!(boundary.contains("PreviousSurfaceId.Load"));
        assert!(boundary.contains("weightSum < MIN_HISTORY_WEIGHT"));
        assert!(boundary.contains("YCOCG_CLAMP_RELATIVE_EXPANSION"));
        assert!(boundary.contains("SHADING_REJECTION_RELATIVE"));
        assert!(boundary.contains("CurrentBoundaryHistory[pixel] = float4(currentColor, count)"));
        assert!(boundary.contains("previousPosition = float2(pixel) + motion"));
        assert!(boundary.contains("if (!boundary && !virtualSurface)"));
        assert!(boundary.contains("BOUNDARY_MASK_VIRTUAL_SURFACE"));
        assert!(boundary.contains("WriteCurrent(pixel, currentColor, 0u, 1.0)"));
    }

    #[test]
    fn tonemap_distinguishes_split_signals_from_composed_dlss_hdr() {
        assert_eq!(tonemap_input_mode(DenoiserBackend::Svgf, false), 0);
        assert_eq!(tonemap_input_mode(DenoiserBackend::NrdReblur, false), 1);
        assert_eq!(tonemap_input_mode(DenoiserBackend::Svgf, true), 2);
        assert_eq!(tonemap_input_mode(DenoiserBackend::NrdReblur, true), 2);
        assert_eq!(
            tonemap_input_mode(DenoiserBackend::DlssRayReconstruction, true),
            TONEMAP_INPUT_RR_HDR
        );

        let shader = include_str!("../../shaders/stage6_tonemap.hlsl");
        assert!(shader.contains("if (InputMode >= 2u)"));
        assert!(shader.contains("if (InputMode == 3u)"));
        assert!(shader.contains("composed += stableEmissive"));

        let renderer = include_str!("d3d12.rs");
        assert!(renderer.contains("streamline_resource_tag(&rr.input_hdr, 3"));

        let resources = include_str!("d3d12/render_resources.rs");
        assert!(resources.contains("DLSS_TONEMAP_TABLE_BASE + 1"));
        assert!(resources.contains("DXGI_FORMAT_R16G16B16A16_FLOAT"));
        assert!(!resources.contains("&dlss.output_hdr,\n                    &dlss.output_hdr,"));
    }

    #[test]
    fn tonemap_debug_views_sample_each_resource_at_its_own_extent() {
        let shader = include_str!("../../shaders/stage6_tonemap.hlsl");
        assert!(shader.contains("LoadForOutput(Texture2D<float4>"));
        assert!(shader.contains("LoadForOutput(Texture2D<float2>"));
        assert!(shader.contains("LoadForOutput(Texture2D<float>"));
        for resource in ["RejectionMask", "HistoryLength", "Id"] {
            assert!(
                shader.contains(&format!("{resource}.GetDimensions(sourceSize.x")),
                "{resource} debug view still inherits another texture's extent"
            );
        }
        assert!(shader.contains("LoadForOutput(NormalRoughness, pixel, size)"));
        assert!(shader.contains("LoadForOutput(Depth, pixel, size)"));
        assert!(shader.contains("LoadForOutput(Motion, pixel, size)"));
    }

    #[test]
    fn dlss_guide_mode_preserves_rr_path_semantics() {
        assert_eq!(dlss_guide_mode(false, false), DLSS_GUIDE_MODE_DISABLED);
        assert_eq!(dlss_guide_mode(true, false), DLSS_GUIDE_MODE_SR);
        assert_eq!(dlss_guide_mode(false, true), DLSS_GUIDE_MODE_RR);
    }

    #[test]
    fn svgf_does_not_clip_stable_sparse_history_to_one_frame() {
        let shader = include_str!("../../shaders/stage6_temporal.hlsl");
        assert!(shader.contains("bool movingHistory = motionMagnitude > 0.01"));
        assert!(shader.contains("if (movingHistory)"));
        assert!(shader.contains("previousDiffuse = clamp(previousDiffuse"));
        assert!(shader.contains("creates a systematic dark bias"));
    }

    #[test]
    fn descriptor_tables_do_not_overlap_and_fit_the_heap() {
        let mut ranges = vec![(
            DXR_TABLE_BASE,
            texture::DXR_UAV_BASE + DXR_UAV_REGISTER_COUNT,
        )];
        for base in TEMPORAL_TABLE_BASES {
            ranges.push((base, base + 28));
        }
        for bases in [
            ATROUS_HISTORY_TABLE_BASES,
            ATROUS_PING_TO_PONG_BASES,
            ATROUS_PONG_TO_PING_BASES,
        ] {
            for base in bases {
                ranges.push((base, base + 10));
            }
        }
        for base in TONEMAP_TABLE_BASES {
            ranges.push((base, base + 15));
        }
        ranges.push((
            STABLE_BUILD_TABLE_BASE,
            STABLE_BUILD_TABLE_BASE + STABLE_BUILD_DESCRIPTOR_COUNT,
        ));
        #[cfg(feature = "nrd")]
        {
            ranges.push((NRD_PREP_TABLE_BASE, NRD_PREP_TABLE_BASE + 18));
            ranges.push((
                NRD_TRANSMISSION_PREP_TABLE_BASE,
                NRD_TRANSMISSION_PREP_TABLE_BASE + 18,
            ));
            ranges.push((NRD_COMPOSE_TABLE_BASE, NRD_COMPOSE_TABLE_BASE + 13));
            for base in NRD_STABLE_PREP_TABLE_BASES {
                ranges.push((base, base + 11));
            }
            for base in NRD_STABLE_COMPOSE_TABLE_BASES {
                ranges.push((base, base + 8));
            }
        }
        #[cfg(feature = "streamline")]
        {
            for base in DLSS_COMPOSE_TABLE_BASES {
                ranges.push((base, base + 4));
            }
            ranges.push((DLSS_TONEMAP_TABLE_BASE, DLSS_TONEMAP_TABLE_BASE + 15));
        }
        #[cfg(feature = "streamline-rr")]
        {
            ranges.push((RR_INPUT_TABLE_BASE, RR_INPUT_TABLE_BASE + 6));
            ranges.push((RR_STABLE_INPUT_TABLE_BASE, RR_STABLE_INPUT_TABLE_BASE + 13));
            for base in RR_EMISSIVE_TABLE_BASES {
                ranges.push((base, base + 4));
            }
            for base in RR_TONEMAP_TABLE_BASES {
                ranges.push((base, base + 15));
            }
            for frame_index in 0..FRAME_COUNT {
                for history_index in 0..2 {
                    let base = RR_PRIMARY_VISIBILITY_TABLE_BASE
                        + (frame_index * 2 + history_index) * RR_PRIMARY_VISIBILITY_TABLE_STRIDE;
                    ranges.push((base, base + RR_PRIMARY_VISIBILITY_TABLE_STRIDE));
                }
            }
            for base in RR_BOUNDARY_TABLE_BASES {
                ranges.push((base, base + 9));
            }
        }
        ranges.sort_unstable();

        for pair in ranges.windows(2) {
            assert!(pair[0].1 <= pair[1].0, "descriptor tables overlap");
        }
        assert!(ranges.last().unwrap().1 <= SHADER_DESCRIPTOR_COUNT);
    }

    #[test]
    fn stage3_reconstruction_uav_registers_match_the_descriptor_contract() {
        let shader = include_str!("../../shaders/stage3_triangle.hlsl");
        for (declaration, register) in [
            (
                "RWTexture2D<float> ReconstructionDiffuseHitDistance",
                RECONSTRUCTION_DIFFUSE_HIT_DISTANCE_UAV_REGISTER,
            ),
            (
                "RWTexture2D<float> ReconstructionSpecularHitDistance",
                RECONSTRUCTION_SPECULAR_HIT_DISTANCE_UAV_REGISTER,
            ),
            (
                "RWTexture2D<float4> ReconstructionPrimaryEmissive",
                RECONSTRUCTION_PRIMARY_EMISSIVE_UAV_REGISTER,
            ),
            ("RWTexture2D<float> DlssDepth", DLSS_DEPTH_UAV_REGISTER),
            ("RWTexture2D<float2> DlssMotion", DLSS_MOTION_UAV_REGISTER),
            (
                "RWTexture2D<float2> DlssSpecularMotion",
                DLSS_SPECULAR_MOTION_UAV_REGISTER,
            ),
        ] {
            assert!(
                shader.contains(&format!("{declaration} : register(u{register});")),
                "stage3 UAV declaration differs from the Rust descriptor contract: {declaration}"
            );
        }
        assert_eq!(
            shader.matches("if (DlssGuideMode != 0u)").count(),
            3,
            "DLSS guide clear and both first-hit paths must remain uniformly guarded"
        );
        for (offset, declaration) in [
            "RWTexture2D<float4> TransmissionRawDiffuse",
            "RWTexture2D<float4> TransmissionRawSpecular",
            "RWTexture2D<float4> TransmissionBaseColor",
            "RWTexture2D<float4> TransmissionNormalRoughness",
            "RWTexture2D<float> TransmissionViewZ",
            "RWTexture2D<float4> TransmissionMotion",
            "RWTexture2D<float> TransmissionDiffuseHitDistance",
            "RWTexture2D<float> TransmissionSpecularHitDistance",
            "RWTexture2D<float4> TransmissionPrimaryEmissive",
            "RWTexture2D<float4> TransmissionDiffuseAlbedo",
            "RWTexture2D<float4> TransmissionViewProxy",
        ]
        .into_iter()
        .enumerate()
        {
            let register = TRANSMISSION_RAW_DIFFUSE_UAV_REGISTER + offset;
            assert!(
                shader.contains(&format!("{declaration} : register(u{register});")),
                "transmission-layer UAV differs from the descriptor contract: {declaration}"
            );
        }
        assert!(shader.contains("uint NrdEnabled;"));
        assert!(shader.contains("SampleOwenSobol2D"));
        assert!(shader.contains("SampleGgxVndfDirection"));
        assert!(shader.contains("SampleStratifiedLobe"));
        assert!(shader.contains("payload.depth > 0u && payload.firstKind != 0u ? 4u : 1u"));
        assert_eq!(shader.matches("6u + payload.depth * 8u").count(), 2);
        assert!(!shader.contains("SampleGgxDirection"));
    }

    #[test]
    fn streamline_primary_rays_use_only_the_submitted_global_jitter() {
        let shader = include_str!("../../shaders/stage3_triangle.hlsl");

        assert!(shader.contains("float2 PrimaryRaySampleOffset(uint2 pixel)"));
        assert!(shader.contains("return DlssGuideMode != 0u"));
        assert!(shader.contains("? float2(0.5, 0.5)"));
        assert!(shader.contains(": SampleOwenSobol2D(pixel, 0u);"));
        assert!(shader.contains("primarySampleOffset + CameraJitterPx"));
        assert!(!shader.contains("float2 jitter = SampleOwenSobol2D(pixel, 0u);"));
    }

    #[test]
    fn nrd_glass_layers_are_denoised_before_fresnel_composition() {
        let shader = include_str!("../../shaders/stage3_triangle.hlsl");
        assert!(shader.contains("payload.psrActive == 3u"));
        assert!(
            shader.contains("refractionChild.psrThroughput = baseColor.xyz * refractionWeight")
        );
        assert!(shader.contains("payload.radiance - layeredTransmission"));
        assert!(shader.contains("TransmissionBaseColor[pixel] = float4(0, 0, 0, -1)"));

        let compose = include_str!("../../shaders/stage9_nrd_compose.hlsl");
        assert!(compose.contains("TransmissionDiffuseRadianceHitDistance"));
        assert!(compose.contains("TransmissionSpecularRadianceHitDistance"));
        assert!(compose.contains("TransmissionBaseColor.Load(pixel).w >= -0.5"));
        assert!(compose.contains("restoredTransmissionDiffuse"));
        assert!(compose.contains("restoredTransmissionSpecular"));
    }

    #[test]
    fn rr_glass_uses_deterministic_virtual_transmission_contract() {
        let shader = include_str!("../../shaders/stage3_triangle.hlsl");
        assert!(
            shader.contains(
                "bool layeredTransmissionEnabled = NrdEnabled != 0u || DlssGuideMode == 2u"
            )
        );
        assert!(shader.contains("bool deterministicGlass = layeredTransmissionEnabled"));
        assert!(shader.contains("ReconstructionNoisyHdr[glassPixel] = float4(payload.radiance"));
        assert!(shader.contains("if (NrdEnabled != 0u && isTransmissionSurface)"));

        let visibility = include_str!("../../shaders/stage11_rr_primary_visibility.hlsl");
        assert!(visibility.contains("bool TraceStaticGlassVirtualSurface"));
        assert!(visibility.contains("RayQuery<RAY_FLAG_FORCE_OPAQUE> exitQuery"));
        assert!(visibility.contains("MakeTransmissionVirtualSurface"));
        assert!(visibility.contains("legacyGlass && staticPrimarySurface && staticCamera"));
        assert!(visibility.contains("VIRTUAL_SURFACE_BIT | instanceData.stableSurfaceId"));
        assert!(visibility.contains("PrimaryMotion[pixel] = 0.0"));
    }

    #[test]
    fn nrd_lobes_and_rr_motion_use_separate_reconstruction_contracts() {
        let bridge = include_str!("../../native/nrd_bridge/src/nrd_bridge.cpp");
        assert!(bridge.contains(
            "hitDistanceReconstructionMode = nrd::HitDistanceReconstructionMode::AREA_3X3"
        ));
        let shader = include_str!("../../shaders/stage3_triangle.hlsl");
        assert!(shader.contains("minimumProbability = useNrdProbabilisticLobe ? 0.25 : 0.05"));
        assert!(shader.contains("bool useNrdProbabilisticLobe = NrdEnabled != 0u"));
        assert!(shader.contains("(payload.depth == 0u || isPsrSurface)"));
        assert!(shader.contains("WritePsrSurfaceGuides"));
        assert!(shader.contains("child.psrActive == 2u"));
        assert!(shader.contains("psrMirrorIsStatic"));
        assert!(shader.contains("if (any(diffuseBounceWeight > 0.0))"));
        assert!(shader.contains("if (any(specularBounceWeight > 0.0))"));
        assert!(shader.contains("&& DlssGuideMode != 2u"));
        assert!(shader.contains("RrSpecularGuide TraceRrSpecularGuide"));
        assert!(shader.contains("RayQuery<RAY_FLAG_CULL_BACK_FACING_TRIANGLES"));
        assert!(shader.contains("if (DlssGuideMode == 2u && (kind == 0u || kind == 1u))"));
        assert!(shader.contains("CommittedTriangleBarycentrics"));
        assert!(shader.contains("DlssSpecularMotion[DispatchRaysIndex().xy]"));

        let renderer = include_str!("d3d12.rs");
        assert!(renderer.contains("streamline_resource_tag(&rr.specular_motion, 10"));
        assert!(!renderer.contains(
            "streamline_resource_tag(\n                            &generation.reconstruction_specular_hit_distance"
        ));
        let resources = include_str!("d3d12/render_resources.rs");
        assert!(resources.contains("&rr.specular_motion"));
        assert!(resources.contains("reconstruction_specular_hit_distance"));

        let prep = include_str!("../../shaders/stage9_nrd_prep.hlsl");
        assert!(prep.contains("float materialId = clamp(round(baseColorKind.w), 0.0, 3.0)"));
        assert!(prep.contains("roughness,\n        materialId)"));
        assert!(bridge.contains("reblur_settings.minMaterialForDiffuse = 1.0f"));
        assert!(bridge.contains("reblur_settings.minMaterialForSpecular = 2.0f"));
    }

    #[test]
    fn benchmark_json_is_single_line_and_has_stable_pass_and_memory_fields() {
        let report = profiler::GpuTimingReport {
            passes: [profiler::GpuTimingStats {
                p50_ms: Some(1.25),
                p95_ms: Some(2.5),
                valid_samples: 240,
            }; profiler::PASS_COUNT],
        };
        let memory = VideoMemorySnapshot {
            usage_bytes: Some(512),
            budget_bytes: Some(1_024),
            usage_ratio: Some(0.5),
            status: VideoMemoryStatus::Available,
        };
        let json = benchmark_json_line(
            BenchmarkJsonContext {
                gpu_name: "RTX 4060 \"Laptop\"",
                width: 1280,
                height: 720,
                render_width: 1280,
                render_height: 720,
                render_scale_requested: 1.0,
                resolution_mode: "fixed",
                dynamic_resolution: serde_json::Value::Null,
                render_generation_id: 1,
                render_generation_create_count: 0,
                render_generation_switch_count: 0,
                render_generation_retired_count: 0,
                retired_generation_count: 0,
                retired_generation_high_watermark: 2,
                gpu_idle_wait_count: 0,
                render_scale_quantized_noop_count: 0,
                render_min: Extent2D {
                    width: 1280,
                    height: 720,
                },
                render_max: Extent2D {
                    width: 1280,
                    height: 720,
                },
                duration_seconds: 30,
                warmup_valid_frames: 120,
                pix_events_available: true,
                history_reset_count: 2,
                render_extent_change_count: 1,
                atrous_mode: "shared",
                command_recording_mode: "optimized",
                requested_path_space: "auto",
                active_path_space: "stable-planes",
                path_space_consumer: "nrd-stable-planes",
                stable_plane_allocated_bytes: 228_556_800,
                stable_plane_counters: &crate::path_space::StablePlaneCounterTelemetry::default(),
                denoiser_backend: "svgf",
                upscaler_mode: "native",
                dlss_optimal: serde_json::Value::Null,
                dlss_viewport_id: None,
                rr_support: serde_json::json!({
                    "compiled": false,
                    "supported": false,
                    "supported_raw": 0,
                    "result_raw": 5,
                }),
                reflex: serde_json::Value::Null,
                denoiser_switch_count: 0,
                nrd_compiled: false,
            },
            report,
            profiler::CommandRecordingStats {
                cpu_ms: profiler::GpuTimingStats {
                    p50_ms: Some(0.42),
                    p95_ms: Some(0.61),
                    valid_samples: 240,
                },
                tracked_transition_api_calls_mean: Some(11.0),
                tracked_transition_barriers_mean: Some(58.0),
                atrous_pipeline_binds_mean: Some(1.0),
                atrous_argument_updates_mean: Some(4.0),
            },
            memory,
            VideoMemoryMeasurement::default(),
            &AccelerationStructureStats::from_records(
                crate::realtime::AccelerationStructureMode::Baseline,
                true,
                Vec::new(),
                256,
                256,
                512,
                65_536,
            ),
        );
        assert!(!json.contains(['\r', '\n']));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["reconstruction_contract_version"], 1);
        assert_eq!(value["gpu_name"], "RTX 4060 \"Laptop\"");
        assert_eq!(value["resolution_mode"], "fixed");
        assert_eq!(value["path_space"]["plane_count"], 3);
        assert_eq!(value["path_space"]["requested"], "auto");
        assert_eq!(value["path_space"]["active"], "stable-planes");
        assert_eq!(value["path_space"]["consumer"], "nrd-stable-planes");
        assert!(value["dynamic_resolution"].is_null());
        assert_eq!(value["render_scale_requested"], 1.0);
        assert_eq!(value["render_generation_id"], 1);
        assert_eq!(value["render_generation_create_count"], 0);
        assert_eq!(value["retired_generation_count"], 0);
        assert_eq!(value["retired_generation_high_watermark"], 2);
        assert_eq!(value["atrous_mode"], "shared");
        assert_eq!(value["denoiser"]["requested"], "svgf");
        assert_eq!(value["denoiser"]["active"], "svgf");
        assert_eq!(value["denoiser"]["switch_count"], 0);
        assert_eq!(value["denoiser"]["nrd_compiled"], false);
        assert_eq!(value["command_recording"]["mode"], "optimized");
        assert_eq!(value["command_recording"]["cpu_ms"]["p95_ms"], 0.61);
        assert_eq!(
            value["command_recording"]["tracked_transition_api_calls_mean"],
            11.0
        );
        assert_eq!(
            value["command_recording"]["atrous_pipeline_binds_mean"],
            1.0
        );
        assert_eq!(value["render_min_width"], 1280);
        assert_eq!(value["render_max_height"], 720);
        assert_eq!(value["history_reset_count"], 2);
        assert_eq!(value["render_extent_change_count"], 1);
        assert_eq!(value["pix_events_available"], true);
        assert_eq!(value["passes"]["atrous_0"]["p50_ms"], 1.25);
        assert_eq!(value["passes"]["tone_map"]["valid_samples"], 240);
        assert_eq!(
            value["passes"]["rr_primary_visibility"]["valid_samples"],
            240
        );
        assert_eq!(value["passes"]["rr_boundary_resolve"]["p95_ms"], 2.5);
        assert_eq!(value["memory"]["usage_bytes"], 512);
        assert_eq!(value["memory"]["usage_ratio"], 0.5);
        assert_eq!(value["acceleration_structures"]["mode"], "baseline");
        assert_eq!(value["acceleration_structures"]["blas_count"], 0);
        assert!(value["acceleration_structures"]["allocation_saving_ratio"].is_null());
        assert_eq!(
            value["acceleration_structures"]["retained_update_scratch_required_bytes"],
            512
        );
        assert_eq!(
            value["acceleration_structures"]["retained_update_scratch_allocation_bytes"],
            65_536
        );
        assert_eq!(
            value["acceleration_structures"]["retained_update_scratch_bytes"],
            65_536
        );
    }

    #[cfg(feature = "streamline-rr")]
    #[test]
    fn rr_boundary_cpu_reference_enforces_reset_motion_and_history_limits() {
        fn history_limit(motion_pixels: f32) -> Option<f32> {
            if !motion_pixels.is_finite() || motion_pixels > 2.0 {
                return None;
            }
            Some(if motion_pixels <= 0.01 {
                128.0
            } else if motion_pixels > 0.5 {
                4.0
            } else {
                32.0
            })
        }

        assert_eq!(history_limit(0.0), Some(128.0));
        assert_eq!(history_limit(0.01), Some(128.0));
        assert_eq!(history_limit(0.0101), Some(32.0));
        assert_eq!(history_limit(0.5), Some(32.0));
        assert_eq!(history_limit(0.5001), Some(4.0));
        assert_eq!(history_limit(2.0), Some(4.0));
        assert_eq!(history_limit(2.0001), None);
        assert_eq!(history_limit(f32::NAN), None);
        assert_eq!(history_limit(f32::INFINITY), None);

        let shader = include_str!("../../shaders/stage11_rr_boundary_resolve.hlsl");
        for required in [
            "ResetHistory != 0u",
            "!all(isfinite(motion))",
            "samplePosition.x < 0",
            "previousId != currentId",
            "dot(currentNormal, previousNormal) < NORMAL_DOT_THRESHOLD",
            "previousMeta.z - currentMeta.w",
            "BOUNDARY_RADIUS",
            "IsVirtualSurface(currentId)",
            "BOUNDARY_MASK_VIRTUAL_SURFACE",
            "STATIC_HISTORY_COUNT",
            "SLOW_MOVING_HISTORY_COUNT",
            "MOTION_REJECT_PIXELS",
        ] {
            assert!(
                shader.contains(required),
                "missing boundary guard: {required}"
            );
        }
    }

    #[test]
    fn dynamic_resolution_json_preserves_state_and_measurement_baselines() {
        let config = DynamicResolutionConfig::default();
        let output_extent = Extent2D {
            width: 1920,
            height: 1080,
        };
        let mut controller = DynamicResolutionController::new(config);
        let mut decision = None;
        for sample in 0..DynamicResolutionConfig::DOWN_STREAK {
            decision = controller.observe_sample(
                Some(16.0),
                true,
                Duration::from_millis(u64::from(sample)),
                output_extent,
                output_extent,
            );
        }
        controller.commit_switch(
            decision.expect("eighth high sample must downscale"),
            Duration::ZERO,
        );
        let active_extent = render_extent(output_extent, controller.current_scale());
        let _ = controller.observe_sample(
            Some(16.0),
            false,
            Duration::from_millis(10),
            output_extent,
            active_extent,
        );
        for sample in 0..3 {
            let _ = controller.observe_sample(
                Some(16.0),
                true,
                Duration::from_millis(20 + sample),
                output_extent,
                active_extent,
            );
        }

        let value = dynamic_resolution_json_value(
            Some(&controller),
            DynamicResolutionBenchmarkBaseline {
                valid_samples: 2,
                ..Default::default()
            },
        );
        assert_eq!(value["config"]["target_gpu_ms"], 14.5);
        assert!(
            (value["state"]["current_requested_scale"]
                .as_f64()
                .expect("scale must be numeric")
                - 0.95)
                .abs()
                < 1.0e-6
        );
        assert_eq!(value["state"]["last_direction"], "down");
        assert_eq!(value["state"]["last_trigger_total_ms"], 16.0);
        assert_eq!(value["state"]["cooldown_valid_samples_remaining"], 57);
        assert_eq!(value["measurement"]["valid_samples"], 9);
        assert_eq!(value["measurement"]["stale_generation_samples_ignored"], 1);
        assert_eq!(value["measurement"]["downscale_count"], 1);
        assert_eq!(value["measurement"]["switch_count"], 1);
        assert_eq!(value["lifetime"]["valid_samples"], 11);
        assert_eq!(value["lifetime"]["stale_generation_samples_ignored"], 1);
        assert_eq!(value["lifetime"]["switch_count"], 1);
        assert!(dynamic_resolution_json_value(None, Default::default()).is_null());
    }

    #[test]
    fn shared_atrous_is_limited_to_the_small_step_iterations() {
        assert_eq!(
            atrous_pipeline_kind(AtrousMode::Baseline, 1),
            AtrousPipelineKind::Baseline
        );
        assert_eq!(
            atrous_pipeline_kind(AtrousMode::Shared, 1),
            AtrousPipelineKind::Shared
        );
        assert_eq!(
            atrous_pipeline_kind(AtrousMode::Shared, 2),
            AtrousPipelineKind::Shared
        );
        assert_eq!(
            atrous_pipeline_kind(AtrousMode::Shared, 4),
            AtrousPipelineKind::Baseline
        );
        assert_eq!(
            atrous_pipeline_kind(AtrousMode::Shared, 8),
            AtrousPipelineKind::Baseline
        );
        assert_eq!(
            atrous_pipeline_kind(AtrousMode::Shared, 0),
            AtrousPipelineKind::Baseline
        );
    }

    #[test]
    fn atrous_binding_runs_match_command_and_shader_modes() {
        fn counts(command_mode: CommandRecordingMode, atrous_mode: AtrousMode) -> (usize, usize) {
            let mut previous = None;
            let mut pipeline_binds = 0;
            let mut argument_updates = 0;
            for step_width in [1, 2, 4, 8] {
                let current = atrous_pipeline_kind(atrous_mode, step_width);
                if should_bind_atrous_pipeline(command_mode, previous, current) {
                    pipeline_binds += 1;
                }
                previous = Some(current);
                argument_updates += 1;
            }
            (pipeline_binds, argument_updates)
        }

        assert_eq!(
            counts(CommandRecordingMode::Baseline, AtrousMode::Baseline),
            (4, 4)
        );
        assert_eq!(
            counts(CommandRecordingMode::Optimized, AtrousMode::Baseline),
            (1, 4)
        );
        assert_eq!(
            counts(CommandRecordingMode::Optimized, AtrousMode::Shared),
            (2, 4)
        );
    }
}
