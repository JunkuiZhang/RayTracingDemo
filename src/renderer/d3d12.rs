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
    as_policy::AccelerationStructureStats,
    realtime::{AtrousMode, CommandRecordingMode, RealtimeConfig},
    resolution::Extent2D,
    scene::{MAX_SCENE_SAMPLERS, SceneAsset, gltf_loader},
};

use self::{
    descriptor::DescriptorHeap,
    memory::{VideoMemorySnapshot, VideoMemoryStatus, VideoMemoryTelemetry},
    pipeline::ComputePipeline,
    profiler::{CommandRecordingFrameStats, GpuPass, GpuProfiler},
    render_resources::RenderResourceGeneration,
    resource::{BarrierSubmissionMode, TrackedResource, TransitionBatch},
    shader::{ReloadedShaders, ShaderReloader},
    texture::{DXR_UAV_BASE, TextureSet},
};
use raytracing::{AccelerationStructures, RaytracingPipeline, SceneGeometry};

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
const SHADER_DESCRIPTOR_COUNT: usize = 320;
const STAGE3_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage3_triangle.dxil"));
const TEMPORAL_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage6_temporal.dxil"));
const ATROUS_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage6_atrous.dxil"));
const ATROUS_SHARED_SHADER: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/stage8_atrous_shared.dxil"));
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
    timing_valid: bool,
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

/// 阶段 1 的最小 DX12 后端：三缓冲交换链、清屏和逐帧 Fence。
pub struct Dx12Renderer {
    device: ID3D12Device,
    _adapter: IDXGIAdapter1,
    gpu_name: String,
    command_queue: ID3D12CommandQueue,
    swap_chain: IDXGISwapChain3,
    rtv_heap: DescriptorHeap,
    render_targets: [Option<TrackedResource>; FRAME_COUNT],
    active_generation: RenderResourceGeneration,
    gpu_profiler: GpuProfiler,
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
    debug_view: u32,
    camera_position: [f32; 3],
    camera_yaw: f32,
    camera_pitch: f32,
    previous_camera_position: [f32; 3],
    previous_camera_yaw: f32,
    previous_camera_pitch: f32,
    animate_model: bool,
    atrous_mode: AtrousMode,
    command_recording_mode: CommandRecordingMode,
    animation_start: Instant,
    history_reset_count: u64,
    render_extent_change_count: u64,
    benchmark_history_reset_baseline: u64,
    benchmark_extent_change_baseline: u64,
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
            let (device, adapter, gpu_name) =
                create_hardware_device(&factory).map_err(|error| dx_error("创建设备", error))?;
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
            let raytracing_status = require_raytracing_tier_1_1(&device)?;
            let mut frames = Vec::with_capacity(FRAME_COUNT);
            for _ in 0..FRAME_COUNT {
                frames.push(FrameContext {
                    allocator: device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?,
                    fence_value: 0,
                    timing_valid: false,
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
            let active_generation = RenderResourceGeneration::new(
                &device,
                &texture_set,
                &scene_geometry,
                &acceleration_structures,
                output_extent,
                output_extent,
                1,
            )
            .map_err(|error| dx_error("创建初始渲染资源代际", error))?;
            let mut renderer = Self {
                device,
                _adapter: adapter,
                gpu_name,
                command_queue,
                swap_chain,
                rtv_heap,
                render_targets: [None, None, None],
                active_generation,
                gpu_profiler,
                memory_telemetry,
                shader_status: format!(
                    "DXR/Temporal/À-Trous（{} KiB）",
                    (STAGE3_SHADER.len()
                        + TEMPORAL_SHADER.len()
                        + ATROUS_SHADER.len()
                        + ATROUS_SHARED_SHADER.len()
                        + TONEMAP_SHADER.len())
                        / 1024
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
                debug_view: 0,
                camera_position: [0.0, 0.0, -2.666_666_7],
                camera_yaw: 0.0,
                camera_pitch: 0.0,
                previous_camera_position: [0.0, 0.0, -2.666_666_7],
                previous_camera_yaw: 0.0,
                previous_camera_pitch: 0.0,
                animate_model: config.animate_model,
                atrous_mode: config.atrous_mode,
                command_recording_mode: config.command_recording_mode,
                animation_start: Instant::now(),
                history_reset_count: 1,
                render_extent_change_count: 0,
                benchmark_history_reset_baseline: 1,
                benchmark_extent_change_baseline: 0,
            };
            renderer
                .create_render_targets()
                .map_err(|error| dx_error("创建交换链渲染目标", error))?;
            Ok(renderer)
        }
    }

    pub fn render(&mut self) -> Result<()> {
        self.poll_shader_reload();
        if self.minimized || self.width == 0 || self.height == 0 {
            return Ok(());
        }
        self.memory_telemetry.poll(false);

        unsafe {
            let frame_index = self.swap_chain.GetCurrentBackBufferIndex() as usize;
            let previous_fence_value = self.frames[frame_index].fence_value;
            let previous_timing_valid = self.frames[frame_index].timing_valid;
            self.wait_for_frame(frame_index)?;
            let fence_completed =
                previous_fence_value != 0 && self.fence.GetCompletedValue() >= previous_fence_value;
            self.gpu_profiler
                .collect(frame_index, fence_completed, previous_timing_valid)?;
            let command_recording_started = Instant::now();
            let mut command_recording_stats = CommandRecordingFrameStats::default();
            let frame = &self.frames[frame_index];
            frame.allocator.Reset()?;
            self.command_list
                .Reset(&frame.allocator, None::<&ID3D12PipelineState>)?;

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
            let acceleration_dirty = self
                ._scene_geometry
                .prepare_animation(self.animation_start.elapsed(), self.animate_model);
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
            self.gpu_profiler.end_event(&self.command_list);

            self.collect_frame_input_transitions(D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
            let current_history = self.history_index;
            let previous_history = 1 - current_history;
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

            let groups_x = self.width.div_ceil(8);
            let groups_y = self.height.div_ceil(8);
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
            self.command_list.Dispatch(groups_x, groups_y, 1);
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
            self.command_list.Dispatch(groups_x, groups_y, 1);
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
            self.command_list.Dispatch(groups_x, groups_y, 1);
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
            self.command_list.Dispatch(groups_x, groups_y, 1);
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
            self.command_list.Dispatch(groups_x, groups_y, 1);
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

            self.gpu_profiler
                .begin(&self.command_list, frame_index, GpuPass::ToneMap);
            self.gpu_profiler
                .begin_event(&self.command_list, GpuPass::ToneMap);
            let display_output = &mut self.active_generation.display_output;
            self.tonemap_pipeline.bind(
                &self.command_list,
                self.active_generation
                    .shader_heap
                    .gpu_handle(TONEMAP_TABLE_BASES[current_history]),
                &[self.debug_view, 1.0_f32.to_bits()],
            );
            self.command_list.Dispatch(groups_x, groups_y, 1);
            self.gpu_profiler
                .end(&self.command_list, frame_index, GpuPass::ToneMap);
            self.gpu_profiler.end_event(&self.command_list);
            display_output
                .collect_transition(&mut self.transition_batch, D3D12_RESOURCE_STATE_COPY_SOURCE);
            self.submit_transition_batch(&mut command_recording_stats);

            self.gpu_profiler
                .end(&self.command_list, frame_index, GpuPass::Total);
            self.gpu_profiler.end_event(&self.command_list);

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
            self.gpu_profiler
                .resolve_frame(&self.command_list, frame_index);
            self.command_list.Close()?;
            self.gpu_profiler.record_command_recording(
                command_recording_started.elapsed(),
                command_recording_stats,
            );

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
            self.frames[frame_index].timing_valid = !self.reset_history;
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
            self.gpu_profiler.invalidate();
            // 命令列表会持有上一帧 Back Buffer 的引用；重置后再释放资源，
            // 否则 ResizeBuffers 会因仍有外部引用而返回 DXGI_ERROR_INVALID_CALL。
            let frame_index = self.swap_chain.GetCurrentBackBufferIndex() as usize;
            self.frames[frame_index].allocator.Reset()?;
            self.command_list.Reset(
                &self.frames[frame_index].allocator,
                None::<&ID3D12PipelineState>,
            )?;
            self.command_list.Close()?;
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
                frame.timing_valid = false;
            }
            self.width = width;
            self.height = height;
            self.render_extent_change_count = self.render_extent_change_count.saturating_add(1);
            self.minimized = false;
            self.history_index = 0;
            self.request_history_reset();
            self.accumulated_frames = 0;
            self.previous_camera_position = self.camera_position;
            self.previous_camera_yaw = self.camera_yaw;
            self.previous_camera_pitch = self.camera_pitch;
            let output_extent = Extent2D { width, height };
            self.active_generation = RenderResourceGeneration::new(
                &self.device,
                &self._textures,
                &self._scene_geometry,
                &self._acceleration_structures,
                output_extent,
                output_extent,
                self.active_generation.id.saturating_add(1),
            )?;
            self.create_render_targets()?;
            Ok(())
        }
    }

    pub fn gpu_time_ms(&self) -> f64 {
        self.gpu_profiler.time_ms(GpuPass::Total)
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
        }
        self.gpu_profiler.begin_benchmark_measurement();
        self.benchmark_history_reset_baseline = self.history_reset_count;
        self.benchmark_extent_change_baseline = self.render_extent_change_count;
    }

    pub fn refresh_memory_telemetry(&mut self) {
        self.memory_telemetry.poll(true);
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

    pub fn benchmark_json(&self, duration_seconds: u64, warmup_valid_frames: u32) -> String {
        benchmark_json_line(
            BenchmarkJsonContext {
                gpu_name: &self.gpu_name,
                width: self.width,
                height: self.height,
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
            },
            self.gpu_profiler.benchmark_statistics(),
            self.gpu_profiler.command_recording_statistics(),
            self.video_memory_snapshot(),
            self._acceleration_structures.stats(),
        )
    }
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
    duration_seconds: u64,
    warmup_valid_frames: u32,
    pix_events_available: bool,
    history_reset_count: u64,
    render_extent_change_count: u64,
    atrous_mode: &'a str,
    command_recording_mode: &'a str,
}

fn benchmark_json_line(
    context: BenchmarkJsonContext<'_>,
    report: profiler::GpuTimingReport,
    command_recording: profiler::CommandRecordingStats,
    memory: VideoMemorySnapshot,
    acceleration_structures: &AccelerationStructureStats,
) -> String {
    let BenchmarkJsonContext {
        gpu_name,
        width,
        height,
        duration_seconds,
        warmup_valid_frames,
        pix_events_available,
        history_reset_count,
        render_extent_change_count,
        atrous_mode,
        command_recording_mode,
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
        "schema_version": 1,
        "gpu_name": gpu_name,
        "output_width": width,
        "output_height": height,
        "render_width": width,
        "render_height": height,
        "render_min_width": width,
        "render_min_height": height,
        "render_max_width": width,
        "render_max_height": height,
        "resolution_mode": "fixed",
        "atrous_mode": atrous_mode,
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
        },
        "memory": {
            "usage_bytes": memory.usage_bytes,
            "budget_bytes": memory.budget_bytes,
            "usage_ratio": memory.usage_ratio.filter(|ratio| ratio.is_finite()),
            "status": status,
        },
    })
    .to_string()
}

fn gpu_pass_json(stats: profiler::GpuTimingStats) -> serde_json::Value {
    serde_json::json!({
        "p50_ms": stats.p50_ms.filter(|value| value.is_finite()),
        "p95_ms": stats.p95_ms.filter(|value| value.is_finite()),
        "valid_samples": stats.valid_samples,
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

    #[cfg(any())]
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

    #[cfg(any())]
    fn create_frame_resources(&mut self) -> Result<()> {
        self.display_output =
            Some(self.create_uav_texture(DXGI_FORMAT_R8G8B8A8_UNORM, "Tone Map 显示输出")?);
        self.raw_diffuse =
            Some(self.create_uav_texture(DXGI_FORMAT_R16G16B16A16_FLOAT, "原始漫反射辐射亮度")?);
        self.raw_specular = Some(
            self.create_uav_texture(DXGI_FORMAT_R16G16B16A16_FLOAT, "原始未调制镜面和自发光信号")?,
        );
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

    #[cfg(any())]
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

    #[cfg(any())]
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

    #[cfg(any())]
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
        generation
            .gbuffer_id
            .collect_transition(&mut self.transition_batch, state);
        generation
            .gbuffer_world_position
            .collect_transition(&mut self.transition_batch, state);
        generation
            .gbuffer_hit_distance
            .collect_transition(&mut self.transition_batch, state);
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
        for frame in &mut self.frames {
            frame.timing_valid = false;
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
            13,
            1,
            2,
            "Tone Map 与调试视图",
        )?;
        self.raytracing_pipeline = raytracing;
        self.temporal_pipeline = temporal;
        self.atrous_baseline_pipeline = atrous_baseline;
        self.atrous_shared_pipeline = atrous_shared;
        self.tonemap_pipeline = tonemap;
        self.request_history_reset();
        self.accumulated_frames = 0;
        Ok(())
    }

    fn request_history_reset(&mut self) {
        if !self.reset_history {
            self.history_reset_count = self.history_reset_count.saturating_add(1);
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
                duration_seconds: 30,
                warmup_valid_frames: 120,
                pix_events_available: true,
                history_reset_count: 2,
                render_extent_change_count: 1,
                atrous_mode: "shared",
                command_recording_mode: "optimized",
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
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["gpu_name"], "RTX 4060 \"Laptop\"");
        assert_eq!(value["resolution_mode"], "fixed");
        assert_eq!(value["atrous_mode"], "shared");
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
