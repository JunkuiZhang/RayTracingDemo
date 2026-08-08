use std::{ffi::c_void, mem::size_of, time::Instant};

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

use crate::{
    realtime::RealtimeConfig,
    scene::{SceneAsset, gltf_loader},
};

use self::{
    descriptor::DescriptorHeap,
    pipeline::ComputePipeline,
    profiler::{GpuPass, GpuProfiler},
    resource::TrackedResource,
    shader::{ReloadedShaders, ShaderReloader},
    texture::{DXR_UAV_BASE, TextureSet},
};
use raytracing::{AccelerationStructures, RaytracingPipeline, SceneGeometry};

mod descriptor;
mod pipeline;
pub(crate) mod profiler;
mod raytracing;
mod resource;
mod shader;
mod texture;

const FRAME_COUNT: usize = 3;
const SHADER_DESCRIPTOR_COUNT: usize = 320;
const STAGE3_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage3_triangle.dxil"));
const TEMPORAL_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage6_temporal.dxil"));
const ATROUS_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage6_atrous.dxil"));
const TONEMAP_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage6_tonemap.dxil"));

const DXR_TABLE_BASE: usize = 0;
const TEMPORAL_TABLE_BASES: [usize; 2] = [144, 176];
const ATROUS_HISTORY_TABLE_BASES: [usize; 2] = [208, 220];
const ATROUS_PING_TO_PONG_BASES: [usize; 2] = [232, 244];
const ATROUS_PONG_TO_PING_BASES: [usize; 2] = [256, 268];
const TONEMAP_TABLE_BASES: [usize; 2] = [280, 296];

struct FrameContext {
    allocator: ID3D12CommandAllocator,
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
    previous_padding: [f32; 2],
    reset_history: u32,
}

struct DenoiseHistory {
    diffuse: TrackedResource,
    specular: TrackedResource,
    moments: TrackedResource,
    normal_roughness: TrackedResource,
    depth: TrackedResource,
    length: TrackedResource,
    id: TrackedResource,
    world_position: TrackedResource,
    hit_distance: TrackedResource,
}

impl DenoiseHistory {
    fn transition_all(
        &mut self,
        command_list: &ID3D12GraphicsCommandList,
        state: D3D12_RESOURCE_STATES,
    ) {
        self.diffuse.transition(command_list, state);
        self.specular.transition(command_list, state);
        self.moments.transition(command_list, state);
        self.normal_roughness.transition(command_list, state);
        self.depth.transition(command_list, state);
        self.length.transition(command_list, state);
        self.id.transition(command_list, state);
        self.world_position.transition(command_list, state);
        self.hit_distance.transition(command_list, state);
    }
}

/// 阶段 1 的最小 DX12 后端：三缓冲交换链、清屏和逐帧 Fence。
pub struct Dx12Renderer {
    device: ID3D12Device,
    command_queue: ID3D12CommandQueue,
    swap_chain: IDXGISwapChain3,
    rtv_heap: DescriptorHeap,
    shader_heap: DescriptorHeap,
    render_targets: [Option<TrackedResource>; FRAME_COUNT],
    display_output: Option<TrackedResource>,
    raw_diffuse: Option<TrackedResource>,
    raw_specular: Option<TrackedResource>,
    gbuffer_albedo: Option<TrackedResource>,
    gbuffer_normal_roughness: Option<TrackedResource>,
    gbuffer_depth: Option<TrackedResource>,
    gbuffer_motion: Option<TrackedResource>,
    gbuffer_id: Option<TrackedResource>,
    gbuffer_world_position: Option<TrackedResource>,
    gbuffer_hit_distance: Option<TrackedResource>,
    histories: [Option<DenoiseHistory>; 2],
    filter_diffuse_ping: Option<TrackedResource>,
    filter_diffuse_pong: Option<TrackedResource>,
    filter_specular_ping: Option<TrackedResource>,
    filter_specular_pong: Option<TrackedResource>,
    rejection_mask: Option<TrackedResource>,
    gpu_profiler: GpuProfiler,
    shader_status: String,
    raytracing_status: String,
    _textures: TextureSet,
    _scene_geometry: SceneGeometry,
    _acceleration_structures: AccelerationStructures,
    raytracing_pipeline: RaytracingPipeline,
    temporal_pipeline: ComputePipeline,
    atrous_pipeline: ComputePipeline,
    tonemap_pipeline: ComputePipeline,
    shader_reloader: ShaderReloader,
    frames: Vec<FrameContext>,
    command_list: ID3D12GraphicsCommandList,
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
    debug_view: u32,
    camera_position: [f32; 3],
    camera_yaw: f32,
    camera_pitch: f32,
    previous_camera_position: [f32; 3],
    previous_camera_yaw: f32,
    previous_camera_pitch: f32,
    animate_model: bool,
    animation_start: Instant,
}

impl Dx12Renderer {
    pub fn new(window: &Window, width: u32, height: u32, config: &RealtimeConfig) -> Result<Self> {
        unsafe {
            enable_debug_interfaces();

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
            let device =
                create_hardware_device(&factory).map_err(|error| dx_error("创建设备", error))?;
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
            factory
                .MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER)
                .map_err(|error| dx_error("设置窗口关联", error))?;

            let rtv_heap =
                DescriptorHeap::new(&device, D3D12_DESCRIPTOR_HEAP_TYPE_RTV, FRAME_COUNT, false)
                    .map_err(|error| dx_error("创建 RTV 描述符堆", error))?;
            let shader_heap = DescriptorHeap::new(
                &device,
                D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                SHADER_DESCRIPTOR_COUNT,
                true,
            )
            .map_err(|error| dx_error("创建 Shader 描述符堆", error))?;
            let gpu_profiler = GpuProfiler::new(&device, &command_queue, FRAME_COUNT)
                .map_err(|error| dx_error("创建 GPU 计时器", error))?;
            let raytracing_status = require_raytracing_tier_1_1(&device)?;
            let mut frames = Vec::with_capacity(FRAME_COUNT);
            for _ in 0..FRAME_COUNT {
                frames.push(FrameContext {
                    allocator: device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?,
                    fence_value: 0,
                });
            }
            let command_list: ID3D12GraphicsCommandList = device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                &frames[0].allocator,
                None::<&ID3D12PipelineState>,
            )?;
            let mut scene = SceneAsset::cornell_box();
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
            let mut texture_set =
                TextureSet::new(&device, &command_list, &scene.images, &scene.materials)
                    .map_err(|error| dx_error("创建 glTF 纹理资源", error))?;
            let mut scene_geometry =
                SceneGeometry::new(&device, &command_list, &scene, &texture_set)
                    .map_err(|error| dx_error("创建场景网格", error))?;
            let mut acceleration_structures =
                AccelerationStructures::build(&device, &command_list, &scene_geometry)
                    .map_err(|error| dx_error("构建 DXR 加速结构", error))?;
            let tlas_view = D3D12_SHADER_RESOURCE_VIEW_DESC {
                Format: DXGI_FORMAT_UNKNOWN,
                ViewDimension: D3D12_SRV_DIMENSION_RAYTRACING_ACCELERATION_STRUCTURE,
                Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                    RaytracingAccelerationStructure: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_SRV {
                        Location: acceleration_structures.tlas.GetGPUVirtualAddress(),
                    },
                },
            };
            device.CreateShaderResourceView(None, Some(&tlas_view), shader_heap.cpu_handle(0));
            create_structured_srv(
                &device,
                &shader_heap,
                1,
                scene_geometry.vertex_buffer(),
                scene_geometry.vertex_count(),
                48,
            );
            create_structured_srv(
                &device,
                &shader_heap,
                2,
                scene_geometry.index_buffer(),
                scene_geometry.index_count(),
                4,
            );
            create_structured_srv(
                &device,
                &shader_heap,
                3,
                scene_geometry.material_buffer(),
                scene_geometry.material_count(),
                64,
            );
            texture_set.write_srvs(&device, &shader_heap);
            let raytracing_pipeline = RaytracingPipeline::new(&device, STAGE3_SHADER)
                .map_err(|error| dx_error("创建 DXR State Object", error))?;
            let temporal_pipeline =
                ComputePipeline::new(&device, TEMPORAL_SHADER, 18, 10, 1, "阶段 6 时域重投影")
                    .map_err(|error| dx_error("创建时域重投影管线", error))?;
            let atrous_pipeline =
                ComputePipeline::new(&device, ATROUS_SHADER, 8, 2, 2, "阶段 6 À-Trous")
                    .map_err(|error| dx_error("创建 À-Trous 管线", error))?;
            let tonemap_pipeline =
                ComputePipeline::new(&device, TONEMAP_SHADER, 13, 1, 2, "Tone Map 与调试视图")
                    .map_err(|error| dx_error("创建 Tone Map 管线", error))?;
            command_list.Close()?;

            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let fence_event = CreateEventW(None, false, false, None)?;
            let initialization_list: ID3D12CommandList = command_list.cast()?;
            command_queue.ExecuteCommandLists(&[Some(initialization_list)]);
            command_queue.Signal(&fence, 1)?;
            fence.SetEventOnCompletion(1, fence_event)?;
            WaitForSingleObject(fence_event, INFINITE);
            scene_geometry.release_uploads();
            texture_set.release_uploads();
            acceleration_structures.release_build_resources();
            let mut renderer = Self {
                device,
                command_queue,
                swap_chain,
                rtv_heap,
                shader_heap,
                render_targets: [None, None, None],
                display_output: None,
                raw_diffuse: None,
                raw_specular: None,
                gbuffer_albedo: None,
                gbuffer_normal_roughness: None,
                gbuffer_depth: None,
                gbuffer_motion: None,
                gbuffer_id: None,
                gbuffer_world_position: None,
                gbuffer_hit_distance: None,
                histories: [None, None],
                filter_diffuse_ping: None,
                filter_diffuse_pong: None,
                filter_specular_ping: None,
                filter_specular_pong: None,
                rejection_mask: None,
                gpu_profiler,
                shader_status: format!(
                    "DXR/Temporal/À-Trous（{} KiB）",
                    (STAGE3_SHADER.len()
                        + TEMPORAL_SHADER.len()
                        + ATROUS_SHADER.len()
                        + TONEMAP_SHADER.len())
                        / 1024
                ),
                raytracing_status,
                _textures: texture_set,
                _scene_geometry: scene_geometry,
                _acceleration_structures: acceleration_structures,
                raytracing_pipeline,
                temporal_pipeline,
                atrous_pipeline,
                tonemap_pipeline,
                shader_reloader: ShaderReloader::new(),
                frames,
                command_list,
                fence,
                next_fence_value: 2,
                fence_event,
                width,
                height,
                minimized: false,
                frame_number: 0,
                accumulated_frames: 0,
                history_index: 0,
                reset_history: true,
                debug_view: 0,
                camera_position: [0.0, 0.0, -2.666_666_7],
                camera_yaw: 0.0,
                camera_pitch: 0.0,
                previous_camera_position: [0.0, 0.0, -2.666_666_7],
                previous_camera_yaw: 0.0,
                previous_camera_pitch: 0.0,
                animate_model: config.animate_model,
                animation_start: Instant::now(),
            };
            renderer
                .create_render_targets()
                .map_err(|error| dx_error("创建交换链渲染目标", error))?;
            renderer
                .create_frame_resources()
                .map_err(|error| dx_error("创建帧资源", error))?;
            renderer
                .create_denoise_resources()
                .map_err(|error| dx_error("创建时空降噪资源", error))?;
            renderer.create_shader_views();
            Ok(renderer)
        }
    }

    pub fn render(&mut self) -> Result<()> {
        self.poll_shader_reload();
        if self.minimized || self.width == 0 || self.height == 0 {
            return Ok(());
        }

        unsafe {
            let frame_index = self.swap_chain.GetCurrentBackBufferIndex() as usize;
            self.wait_for_frame(frame_index)?;
            self.gpu_profiler.collect(frame_index)?;
            let frame = &self.frames[frame_index];
            frame.allocator.Reset()?;
            self.command_list
                .Reset(&frame.allocator, None::<&ID3D12PipelineState>)?;

            self.gpu_profiler.begin(
                &self.command_list,
                frame_index,
                GpuPass::AccelerationStructure,
            );
            self._scene_geometry
                .prepare_animation(self.animation_start.elapsed(), self.animate_model);
            self._acceleration_structures.update(
                &self.command_list,
                frame_index,
                &self._scene_geometry,
            )?;
            self.gpu_profiler.end(
                &self.command_list,
                frame_index,
                GpuPass::AccelerationStructure,
            );

            self.command_list
                .SetDescriptorHeaps(&[Some(self.shader_heap.heap().clone())]);
            let command_list4: ID3D12GraphicsCommandList4 = self.command_list.cast()?;
            self.gpu_profiler
                .begin(&self.command_list, frame_index, GpuPass::Total);
            self.gpu_profiler
                .begin(&self.command_list, frame_index, GpuPass::PathTrace);
            self.transition_frame_inputs(D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            command_list4.SetComputeRootSignature(&self.raytracing_pipeline.root_signature);
            command_list4
                .SetComputeRootDescriptorTable(0, self.shader_heap.gpu_handle(DXR_TABLE_BASE));
            command_list4.SetComputeRootShaderResourceView(
                2,
                self._acceleration_structures
                    .instance_gpu_address(frame_index),
            );
            let camera = CameraConstants {
                frame_index: self.frame_number,
                position: self.camera_position,
                yaw: self.camera_yaw,
                pitch: self.camera_pitch,
                padding: [0.0; 2],
                previous_position: self.previous_camera_position,
                previous_yaw: self.previous_camera_yaw,
                previous_pitch: self.previous_camera_pitch,
                previous_padding: [0.0; 2],
                reset_history: u32::from(self.reset_history),
            };
            debug_assert_eq!(size_of::<CameraConstants>(), 16 * size_of::<u32>());
            command_list4.SetComputeRoot32BitConstants(
                1,
                16,
                (&camera as *const CameraConstants).cast(),
                0,
            );
            command_list4.SetPipelineState1(&self.raytracing_pipeline.state_object);
            let dispatch = D3D12_DISPATCH_RAYS_DESC {
                RayGenerationShaderRecord: self.raytracing_pipeline.raygen,
                MissShaderTable: self.raytracing_pipeline.miss,
                HitGroupTable: self.raytracing_pipeline.hit_group,
                CallableShaderTable: D3D12_GPU_VIRTUAL_ADDRESS_RANGE_AND_STRIDE::default(),
                Width: self.width,
                Height: self.height,
                Depth: 1,
            };
            command_list4.DispatchRays(&dispatch);
            self.gpu_profiler
                .end(&self.command_list, frame_index, GpuPass::PathTrace);

            self.transition_frame_inputs(D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
            let current_history = self.history_index;
            let previous_history = 1 - current_history;
            self.histories[previous_history]
                .as_mut()
                .unwrap()
                .transition_all(
                    &self.command_list,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                );
            self.histories[current_history]
                .as_mut()
                .unwrap()
                .transition_all(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.rejection_mask
                .as_mut()
                .unwrap()
                .transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);

            let groups_x = self.width.div_ceil(8);
            let groups_y = self.height.div_ceil(8);
            self.gpu_profiler
                .begin(&self.command_list, frame_index, GpuPass::Temporal);
            self.temporal_pipeline.bind(
                &self.command_list,
                self.shader_heap
                    .gpu_handle(TEMPORAL_TABLE_BASES[current_history]),
                &[u32::from(self.reset_history)],
            );
            self.command_list.Dispatch(groups_x, groups_y, 1);
            self.gpu_profiler
                .end(&self.command_list, frame_index, GpuPass::Temporal);
            self.histories[current_history]
                .as_mut()
                .unwrap()
                .transition_all(
                    &self.command_list,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                );
            self.rejection_mask.as_mut().unwrap().transition(
                &self.command_list,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );

            self.gpu_profiler
                .begin(&self.command_list, frame_index, GpuPass::Atrous);
            self.filter_diffuse_ping
                .as_mut()
                .unwrap()
                .transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.filter_specular_ping
                .as_mut()
                .unwrap()
                .transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.atrous_pipeline.bind(
                &self.command_list,
                self.shader_heap
                    .gpu_handle(ATROUS_HISTORY_TABLE_BASES[current_history]),
                &[1, 0],
            );
            self.command_list.Dispatch(groups_x, groups_y, 1);
            self.filter_diffuse_ping.as_mut().unwrap().transition(
                &self.command_list,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );
            self.filter_specular_ping.as_mut().unwrap().transition(
                &self.command_list,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );

            self.filter_diffuse_pong
                .as_mut()
                .unwrap()
                .transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.filter_specular_pong
                .as_mut()
                .unwrap()
                .transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.atrous_pipeline.bind(
                &self.command_list,
                self.shader_heap
                    .gpu_handle(ATROUS_PING_TO_PONG_BASES[current_history]),
                &[2, 1],
            );
            self.command_list.Dispatch(groups_x, groups_y, 1);
            self.filter_diffuse_pong.as_mut().unwrap().transition(
                &self.command_list,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );
            self.filter_specular_pong.as_mut().unwrap().transition(
                &self.command_list,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );

            self.filter_diffuse_ping
                .as_mut()
                .unwrap()
                .transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.filter_specular_ping
                .as_mut()
                .unwrap()
                .transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.atrous_pipeline.bind(
                &self.command_list,
                self.shader_heap
                    .gpu_handle(ATROUS_PONG_TO_PING_BASES[current_history]),
                &[4, 2],
            );
            self.command_list.Dispatch(groups_x, groups_y, 1);
            self.filter_diffuse_ping.as_mut().unwrap().transition(
                &self.command_list,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );
            self.filter_specular_ping.as_mut().unwrap().transition(
                &self.command_list,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );

            self.filter_diffuse_pong
                .as_mut()
                .unwrap()
                .transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.filter_specular_pong
                .as_mut()
                .unwrap()
                .transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.atrous_pipeline.bind(
                &self.command_list,
                self.shader_heap
                    .gpu_handle(ATROUS_PING_TO_PONG_BASES[current_history]),
                &[8, 3],
            );
            self.command_list.Dispatch(groups_x, groups_y, 1);
            self.filter_diffuse_pong.as_mut().unwrap().transition(
                &self.command_list,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );
            self.filter_specular_pong.as_mut().unwrap().transition(
                &self.command_list,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            );
            self.gpu_profiler
                .end(&self.command_list, frame_index, GpuPass::Atrous);

            self.gpu_profiler
                .begin(&self.command_list, frame_index, GpuPass::ToneMap);
            let display_output = self.display_output.as_mut().unwrap();
            display_output.transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.tonemap_pipeline.bind(
                &self.command_list,
                self.shader_heap
                    .gpu_handle(TONEMAP_TABLE_BASES[current_history]),
                &[self.debug_view, 1.0_f32.to_bits()],
            );
            self.command_list.Dispatch(groups_x, groups_y, 1);
            self.gpu_profiler
                .end(&self.command_list, frame_index, GpuPass::ToneMap);
            display_output.transition(&self.command_list, D3D12_RESOURCE_STATE_COPY_SOURCE);

            let render_target = self.render_targets[frame_index].as_mut().unwrap();
            render_target.transition(&self.command_list, D3D12_RESOURCE_STATE_COPY_DEST);
            self.command_list
                .CopyResource(render_target.resource(), display_output.resource());
            render_target.transition(&self.command_list, D3D12_RESOURCE_STATE_PRESENT);
            display_output.transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.gpu_profiler
                .end(&self.command_list, frame_index, GpuPass::Total);
            self.gpu_profiler
                .resolve_frame(&self.command_list, frame_index);
            self.command_list.Close()?;

            let command_list: ID3D12CommandList = self.command_list.cast()?;
            self.command_queue
                .ExecuteCommandLists(&[Some(command_list)]);
            if let Err(error) = self.swap_chain.Present(1, DXGI_PRESENT(0)).ok() {
                return Err(device_removed_error(&self.device, error));
            }

            let fence_value = self.next_fence_value;
            self.next_fence_value += 1;
            self.command_queue.Signal(&self.fence, fence_value)?;
            self.frames[frame_index].fence_value = fence_value;
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

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            self.minimized = true;
            return Ok(());
        }
        if !self.minimized && width == self.width && height == self.height {
            return Ok(());
        }

        unsafe {
            self.wait_for_gpu()?;
            // 命令列表会持有上一帧 Back Buffer 的引用；重置后再释放资源，
            // 否则 ResizeBuffers 会因仍有外部引用而返回 DXGI_ERROR_INVALID_CALL。
            let frame_index = self.swap_chain.GetCurrentBackBufferIndex() as usize;
            self.frames[frame_index].allocator.Reset()?;
            self.command_list.Reset(
                &self.frames[frame_index].allocator,
                None::<&ID3D12PipelineState>,
            )?;
            self.command_list.Close()?;
            self.display_output = None;
            self.raw_diffuse = None;
            self.raw_specular = None;
            self.gbuffer_albedo = None;
            self.gbuffer_normal_roughness = None;
            self.gbuffer_depth = None;
            self.gbuffer_motion = None;
            self.gbuffer_id = None;
            self.gbuffer_world_position = None;
            self.gbuffer_hit_distance = None;
            self.histories = [None, None];
            self.filter_diffuse_ping = None;
            self.filter_diffuse_pong = None;
            self.filter_specular_ping = None;
            self.filter_specular_pong = None;
            self.rejection_mask = None;
            self.render_targets = [None, None, None];
            self.swap_chain.ResizeBuffers(
                FRAME_COUNT as u32,
                width,
                height,
                DXGI_FORMAT_R8G8B8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
            for frame in &mut self.frames {
                frame.fence_value = 0;
            }
            self.width = width;
            self.height = height;
            self.minimized = false;
            self.history_index = 0;
            self.reset_history = true;
            self.accumulated_frames = 0;
            self.previous_camera_position = self.camera_position;
            self.previous_camera_yaw = self.camera_yaw;
            self.previous_camera_pitch = self.camera_pitch;
            self.create_render_targets()?;
            self.create_frame_resources()?;
            self.create_denoise_resources()?;
            self.create_shader_views();
            Ok(())
        }
    }

    pub fn gpu_time_ms(&self) -> f64 {
        self.gpu_profiler.time_ms(GpuPass::Total)
    }

    pub fn gpu_pass_time_ms(&self, pass: GpuPass) -> f64 {
        self.gpu_profiler.time_ms(pass)
    }

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
        self.debug_view = (self.debug_view + 1) % 11;
    }

    pub fn debug_view_name(&self) -> &'static str {
        match self.debug_view {
            0 => "最终",
            1 => "原始 1 SPP",
            2 => "反照率",
            3 => "法线/粗糙度",
            4 => "深度",
            5 => "运动矢量",
            6 => "方差",
            7 => "历史拒绝",
            8 => "历史长度",
            9 => "物体/材质 ID",
            _ => "镜面命中距离",
        }
    }

    unsafe fn create_render_targets(&mut self) -> Result<()> {
        for index in 0..FRAME_COUNT {
            let resource: ID3D12Resource = unsafe { self.swap_chain.GetBuffer(index as u32)? };
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

    fn create_uav_texture(
        &self,
        format: DXGI_FORMAT,
        name: impl Into<String>,
    ) -> Result<TrackedResource> {
        TrackedResource::create_texture_2d(
            &self.device,
            self.width,
            self.height,
            format,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            name,
        )
    }

    fn create_frame_resources(&mut self) -> Result<()> {
        self.display_output =
            Some(self.create_uav_texture(DXGI_FORMAT_R8G8B8A8_UNORM, "Tone Map 显示输出")?);
        self.raw_diffuse =
            Some(self.create_uav_texture(DXGI_FORMAT_R16G16B16A16_FLOAT, "原始漫反射辐射亮度")?);
        self.raw_specular =
            Some(self.create_uav_texture(DXGI_FORMAT_R16G16B16A16_FLOAT, "原始镜面辐射亮度")?);
        self.gbuffer_albedo = Some(
            self.create_uav_texture(DXGI_FORMAT_R16G16B16A16_FLOAT, "第一交点反照率和材质类别")?,
        );
        self.gbuffer_normal_roughness = Some(
            self.create_uav_texture(DXGI_FORMAT_R16G16B16A16_FLOAT, "第一交点世界法线和粗糙度")?,
        );
        self.gbuffer_depth =
            Some(self.create_uav_texture(DXGI_FORMAT_R32_FLOAT, "第一交点线性距离")?);
        self.gbuffer_motion =
            Some(self.create_uav_texture(DXGI_FORMAT_R16G16_FLOAT, "当前到上一帧像素运动矢量")?);
        self.gbuffer_id =
            Some(self.create_uav_texture(DXGI_FORMAT_R32_UINT, "第一交点实例和材质 ID")?);
        self.gbuffer_world_position =
            Some(self.create_uav_texture(DXGI_FORMAT_R32G32B32A32_FLOAT, "第一交点世界位置")?);
        self.gbuffer_hit_distance =
            Some(self.create_uav_texture(DXGI_FORMAT_R32_FLOAT, "镜面反射命中距离")?);
        Ok(())
    }

    fn create_history(&self, index: usize) -> Result<DenoiseHistory> {
        let label = |value: &str| format!("历史 {index} {value}");
        Ok(DenoiseHistory {
            diffuse: self
                .create_uav_texture(DXGI_FORMAT_R16G16B16A16_FLOAT, label("解调漫反射"))?,
            specular: self.create_uav_texture(DXGI_FORMAT_R16G16B16A16_FLOAT, label("镜面信号"))?,
            moments: self.create_uav_texture(
                DXGI_FORMAT_R16G16B16A16_FLOAT,
                label("漫反射和镜面一二阶矩"),
            )?,
            normal_roughness: self
                .create_uav_texture(DXGI_FORMAT_R16G16B16A16_FLOAT, label("法线和粗糙度"))?,
            depth: self.create_uav_texture(DXGI_FORMAT_R32_FLOAT, label("线性深度"))?,
            length: self
                .create_uav_texture(DXGI_FORMAT_R32G32_UINT, label("漫反射和镜面历史长度"))?,
            id: self.create_uav_texture(DXGI_FORMAT_R32_UINT, label("实例和材质 ID"))?,
            world_position: self
                .create_uav_texture(DXGI_FORMAT_R32G32B32A32_FLOAT, label("世界位置"))?,
            hit_distance: self.create_uav_texture(DXGI_FORMAT_R32_FLOAT, label("镜面命中距离"))?,
        })
    }

    fn create_denoise_resources(&mut self) -> Result<()> {
        self.histories = [Some(self.create_history(0)?), Some(self.create_history(1)?)];
        self.filter_diffuse_ping =
            Some(self.create_uav_texture(DXGI_FORMAT_R16G16B16A16_FLOAT, "À-Trous 漫反射 Ping")?);
        self.filter_diffuse_pong =
            Some(self.create_uav_texture(DXGI_FORMAT_R16G16B16A16_FLOAT, "À-Trous 漫反射 Pong")?);
        self.filter_specular_ping =
            Some(self.create_uav_texture(DXGI_FORMAT_R16G16B16A16_FLOAT, "À-Trous 镜面 Ping")?);
        self.filter_specular_pong =
            Some(self.create_uav_texture(DXGI_FORMAT_R16G16B16A16_FLOAT, "À-Trous 镜面 Pong")?);
        self.rejection_mask =
            Some(self.create_uav_texture(DXGI_FORMAT_R32_UINT, "时域历史拒绝原因")?);
        Ok(())
    }

    unsafe fn create_shader_views(&self) {
        unsafe {
            self._textures.write_srvs(&self.device, &self.shader_heap);
        }
        let raw_diffuse = self.raw_diffuse.as_ref().unwrap();
        let raw_specular = self.raw_specular.as_ref().unwrap();
        let albedo = self.gbuffer_albedo.as_ref().unwrap();
        let normal = self.gbuffer_normal_roughness.as_ref().unwrap();
        let depth = self.gbuffer_depth.as_ref().unwrap();
        let motion = self.gbuffer_motion.as_ref().unwrap();
        let id = self.gbuffer_id.as_ref().unwrap();
        let world_position = self.gbuffer_world_position.as_ref().unwrap();
        let hit_distance = self.gbuffer_hit_distance.as_ref().unwrap();
        let rejection = self.rejection_mask.as_ref().unwrap();
        let display_output = self.display_output.as_ref().unwrap();
        let diffuse_ping = self.filter_diffuse_ping.as_ref().unwrap();
        let diffuse_pong = self.filter_diffuse_pong.as_ref().unwrap();
        let specular_ping = self.filter_specular_ping.as_ref().unwrap();
        let specular_pong = self.filter_specular_pong.as_ref().unwrap();

        let dxr_uavs = [
            raw_diffuse,
            raw_specular,
            albedo,
            normal,
            depth,
            motion,
            id,
            world_position,
            hit_distance,
        ];
        for (offset, resource) in dxr_uavs.into_iter().enumerate() {
            unsafe {
                create_texture_uav(
                    &self.device,
                    &self.shader_heap,
                    DXR_UAV_BASE + offset,
                    resource,
                )
            };
        }

        for current_index in 0..2 {
            let previous_index = 1 - current_index;
            let current = self.histories[current_index].as_ref().unwrap();
            let previous = self.histories[previous_index].as_ref().unwrap();
            let temporal_srvs = [
                raw_diffuse,
                raw_specular,
                albedo,
                normal,
                depth,
                motion,
                id,
                world_position,
                hit_distance,
                &previous.diffuse,
                &previous.specular,
                &previous.moments,
                &previous.normal_roughness,
                &previous.depth,
                &previous.length,
                &previous.id,
                &previous.world_position,
                &previous.hit_distance,
            ];
            let temporal_uavs = [
                &current.diffuse,
                &current.specular,
                &current.moments,
                &current.normal_roughness,
                &current.depth,
                &current.length,
                &current.id,
                &current.world_position,
                &current.hit_distance,
                rejection,
            ];
            unsafe {
                populate_texture_table(
                    &self.device,
                    &self.shader_heap,
                    TEMPORAL_TABLE_BASES[current_index],
                    &temporal_srvs,
                    &temporal_uavs,
                )
            };

            let atrous_auxiliary = [
                normal,
                depth,
                &current.moments,
                id,
                &current.length,
                hit_distance,
            ];
            let write_atrous_table =
                |base: usize,
                 source_diffuse: &TrackedResource,
                 source_specular: &TrackedResource,
                 destination_diffuse: &TrackedResource,
                 destination_specular: &TrackedResource| {
                    let srvs = [
                        source_diffuse,
                        source_specular,
                        atrous_auxiliary[0],
                        atrous_auxiliary[1],
                        atrous_auxiliary[2],
                        atrous_auxiliary[3],
                        atrous_auxiliary[4],
                        atrous_auxiliary[5],
                    ];
                    let uavs = [destination_diffuse, destination_specular];
                    unsafe {
                        populate_texture_table(&self.device, &self.shader_heap, base, &srvs, &uavs)
                    };
                };
            write_atrous_table(
                ATROUS_HISTORY_TABLE_BASES[current_index],
                &current.diffuse,
                &current.specular,
                diffuse_ping,
                specular_ping,
            );
            write_atrous_table(
                ATROUS_PING_TO_PONG_BASES[current_index],
                diffuse_ping,
                specular_ping,
                diffuse_pong,
                specular_pong,
            );
            write_atrous_table(
                ATROUS_PONG_TO_PING_BASES[current_index],
                diffuse_pong,
                specular_pong,
                diffuse_ping,
                specular_ping,
            );

            let tonemap_srvs = [
                diffuse_pong,
                specular_pong,
                raw_diffuse,
                raw_specular,
                albedo,
                normal,
                depth,
                motion,
                &current.moments,
                rejection,
                &current.length,
                id,
                hit_distance,
            ];
            unsafe {
                populate_texture_table(
                    &self.device,
                    &self.shader_heap,
                    TONEMAP_TABLE_BASES[current_index],
                    &tonemap_srvs,
                    &[display_output],
                )
            };
        }
    }

    fn transition_frame_inputs(&mut self, state: D3D12_RESOURCE_STATES) {
        self.raw_diffuse
            .as_mut()
            .unwrap()
            .transition(&self.command_list, state);
        self.raw_specular
            .as_mut()
            .unwrap()
            .transition(&self.command_list, state);
        self.gbuffer_albedo
            .as_mut()
            .unwrap()
            .transition(&self.command_list, state);
        self.gbuffer_normal_roughness
            .as_mut()
            .unwrap()
            .transition(&self.command_list, state);
        self.gbuffer_depth
            .as_mut()
            .unwrap()
            .transition(&self.command_list, state);
        self.gbuffer_motion
            .as_mut()
            .unwrap()
            .transition(&self.command_list, state);
        self.gbuffer_id
            .as_mut()
            .unwrap()
            .transition(&self.command_list, state);
        self.gbuffer_world_position
            .as_mut()
            .unwrap()
            .transition(&self.command_list, state);
        self.gbuffer_hit_distance
            .as_mut()
            .unwrap()
            .transition(&self.command_list, state);
    }

    fn poll_shader_reload(&mut self) {
        let Some(result) = self.shader_reloader.poll() else {
            return;
        };
        match result {
            Ok(shaders) => match self.rebuild_shader_pipelines(shaders) {
                Ok(()) => self.shader_status = "热重载成功".to_string(),
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
        let raytracing = RaytracingPipeline::new(&self.device, &shaders.raytracing)?;
        let temporal = ComputePipeline::new(
            &self.device,
            &shaders.temporal,
            18,
            10,
            1,
            "阶段 6 时域重投影",
        )?;
        let atrous =
            ComputePipeline::new(&self.device, &shaders.atrous, 8, 2, 2, "阶段 6 À-Trous")?;
        let tonemap = ComputePipeline::new(
            &self.device,
            &shaders.tonemap,
            13,
            1,
            2,
            "Tone Map 与调试视图",
        )?;
        self.raytracing_pipeline = raytracing;
        self.temporal_pipeline = temporal;
        self.atrous_pipeline = atrous;
        self.tonemap_pipeline = tonemap;
        self.reset_history = true;
        self.accumulated_frames = 0;
        Ok(())
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
            let _ = CloseHandle(self.fence_event);
        }
    }
}

unsafe fn create_hardware_device(factory: &IDXGIFactory6) -> Result<ID3D12Device> {
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
            println!(
                "DX12 适配器：{}",
                String::from_utf16_lossy(&description.Description[..name_end])
            );
            return Ok(device);
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
        unsafe {
            let _ = info_queue.SetBreakOnSeverity(D3D12_MESSAGE_SEVERITY_CORRUPTION, true);
            let _ = info_queue.SetBreakOnSeverity(D3D12_MESSAGE_SEVERITY_ERROR, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_constants_match_the_sixteen_dword_root_constant_contract() {
        assert_eq!(size_of::<CameraConstants>(), 16 * size_of::<u32>());
    }

    #[test]
    fn descriptor_tables_do_not_overlap_and_fit_the_heap() {
        let mut ranges = vec![(DXR_TABLE_BASE, DXR_UAV_BASE + 9)];
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
            ranges.push((base, base + 14));
        }
        ranges.sort_unstable();

        for pair in ranges.windows(2) {
            assert!(pair[0].1 <= pair[1].0, "descriptor tables overlap");
        }
        assert!(ranges.last().unwrap().1 <= SHADER_DESCRIPTOR_COUNT);
    }
}
