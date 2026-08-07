use std::ffi::c_void;

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

use self::{descriptor::DescriptorHeap, profiler::GpuProfiler, resource::TrackedResource};
use raytracing::{AccelerationStructures, RaytracingPipeline, TriangleGeometry};

mod descriptor;
mod profiler;
mod raytracing;
mod resource;

const FRAME_COUNT: usize = 3;
const STAGE3_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage3_triangle.dxil"));

struct FrameContext {
    allocator: ID3D12CommandAllocator,
    fence_value: u64,
}

/// 阶段 1 的最小 DX12 后端：三缓冲交换链、清屏和逐帧 Fence。
pub struct Dx12Renderer {
    device: ID3D12Device,
    command_queue: ID3D12CommandQueue,
    swap_chain: IDXGISwapChain3,
    rtv_heap: DescriptorHeap,
    shader_heap: DescriptorHeap,
    render_targets: [Option<TrackedResource>; FRAME_COUNT],
    compute_output: Option<TrackedResource>,
    gpu_profiler: GpuProfiler,
    shader_status: String,
    raytracing_status: String,
    _triangle_geometry: TriangleGeometry,
    _acceleration_structures: AccelerationStructures,
    raytracing_pipeline: RaytracingPipeline,
    frames: Vec<FrameContext>,
    command_list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    next_fence_value: u64,
    fence_event: HANDLE,
    width: u32,
    height: u32,
    minimized: bool,
}

impl Dx12Renderer {
    pub fn new(window: &Window, width: u32, height: u32) -> Result<Self> {
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
            let shader_heap =
                DescriptorHeap::new(&device, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, 2, true)
                    .map_err(|error| dx_error("创建 Shader 描述符堆", error))?;
            let gpu_profiler = GpuProfiler::new(&device, &command_queue, FRAME_COUNT)
                .map_err(|error| dx_error("创建 GPU 计时器", error))?;
            let raytracing_status = raytracing_status(&device);
            let triangle_geometry = TriangleGeometry::new(&device)
                .map_err(|error| dx_error("创建 DXR 三角形几何", error))?;

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
            let acceleration_structures =
                AccelerationStructures::build(&device, &command_list, &triangle_geometry)
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
            let raytracing_pipeline = RaytracingPipeline::new(&device, STAGE3_SHADER)
                .map_err(|error| dx_error("创建 DXR State Object", error))?;
            command_list.Close()?;

            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let fence_event = CreateEventW(None, false, false, None)?;
            let initialization_list: ID3D12CommandList = command_list.cast()?;
            command_queue.ExecuteCommandLists(&[Some(initialization_list)]);
            command_queue.Signal(&fence, 1)?;
            fence.SetEventOnCompletion(1, fence_event)?;
            WaitForSingleObject(fence_event, INFINITE);
            let mut renderer = Self {
                device,
                command_queue,
                swap_chain,
                rtv_heap,
                shader_heap,
                render_targets: [None, None, None],
                compute_output: None,
                gpu_profiler,
                shader_status: format!("内嵌 DXR（{} 字节）", STAGE3_SHADER.len()),
                raytracing_status,
                _triangle_geometry: triangle_geometry,
                _acceleration_structures: acceleration_structures,
                raytracing_pipeline,
                frames,
                command_list,
                fence,
                next_fence_value: 2,
                fence_event,
                width,
                height,
                minimized: false,
            };
            renderer
                .create_render_targets()
                .map_err(|error| dx_error("创建交换链渲染目标", error))?;
            renderer
                .create_compute_output()
                .map_err(|error| dx_error("创建 Compute 输出", error))?;
            Ok(renderer)
        }
    }

    pub fn render(&mut self) -> Result<()> {
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

            self.command_list
                .SetDescriptorHeaps(&[Some(self.shader_heap.heap().clone())]);
            let command_list4: ID3D12GraphicsCommandList4 = self.command_list.cast()?;
            command_list4.SetComputeRootSignature(&self.raytracing_pipeline.root_signature);
            command_list4.SetComputeRootDescriptorTable(0, self.shader_heap.gpu_handle(0));
            command_list4.SetPipelineState1(&self.raytracing_pipeline.state_object);
            self.gpu_profiler.begin(&self.command_list, frame_index);
            let compute_output = self.compute_output.as_mut().unwrap();
            compute_output.transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
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
            compute_output.transition(&self.command_list, D3D12_RESOURCE_STATE_COPY_SOURCE);

            let render_target = self.render_targets[frame_index].as_mut().unwrap();
            render_target.transition(&self.command_list, D3D12_RESOURCE_STATE_COPY_DEST);
            self.command_list
                .CopyResource(render_target.resource(), compute_output.resource());
            render_target.transition(&self.command_list, D3D12_RESOURCE_STATE_PRESENT);
            compute_output.transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.gpu_profiler.end(&self.command_list, frame_index);
            self.command_list.Close()?;

            let command_list: ID3D12CommandList = self.command_list.cast()?;
            self.command_queue
                .ExecuteCommandLists(&[Some(command_list)]);
            self.swap_chain.Present(1, DXGI_PRESENT(0)).ok()?;

            let fence_value = self.next_fence_value;
            self.next_fence_value += 1;
            self.command_queue.Signal(&self.fence, fence_value)?;
            self.frames[frame_index].fence_value = fence_value;
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
            self.compute_output = None;
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
            self.create_render_targets()?;
            self.create_compute_output()?;
            Ok(())
        }
    }

    pub fn gpu_time_ms(&self) -> f64 {
        self.gpu_profiler.last_time_ms()
    }

    pub fn shader_status(&self) -> &str {
        &self.shader_status
    }

    pub fn raytracing_status(&self) -> &str {
        &self.raytracing_status
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

    unsafe fn create_compute_output(&mut self) -> Result<()> {
        let output = TrackedResource::create_texture_2d(
            &self.device,
            self.width,
            self.height,
            DXGI_FORMAT_R8G8B8A8_UNORM,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            "阶段 2 Compute 输出",
        )?;
        unsafe {
            self.device.CreateUnorderedAccessView(
                output.resource(),
                None,
                None,
                self.shader_heap.cpu_handle(1),
            );
        }
        self.compute_output = Some(output);
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

fn raytracing_status(device: &ID3D12Device) -> String {
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS5::default();
    let result = unsafe {
        device.CheckFeatureSupport(
            D3D12_FEATURE_D3D12_OPTIONS5,
            (&mut options as *mut D3D12_FEATURE_DATA_D3D12_OPTIONS5).cast(),
            std::mem::size_of::<D3D12_FEATURE_DATA_D3D12_OPTIONS5>() as u32,
        )
    };
    if result.is_err() || options.RaytracingTier == D3D12_RAYTRACING_TIER_NOT_SUPPORTED {
        "DXR 不可用".to_string()
    } else {
        format!("DXR Tier {}", options.RaytracingTier.0 as f32 / 10.0)
    }
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
        let adapter = unsafe {
            factory.EnumAdapterByGpuPreference::<IDXGIAdapter1>(
                adapter_index,
                DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
            )
        }?;
        adapter_index += 1;
        let description = unsafe { adapter.GetDesc1()? };
        if description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }

        let mut device = None;
        if unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device) }.is_ok() {
            let name_end = description
                .Description
                .iter()
                .position(|character| *character == 0)
                .unwrap_or(description.Description.len());
            println!(
                "DX12 适配器：{}",
                String::from_utf16_lossy(&description.Description[..name_end])
            );
            return Ok(device.unwrap());
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

unsafe fn enable_debug_interfaces() {
    if !cfg!(debug_assertions) {
        return;
    }

    let mut debug: Option<ID3D12Debug1> = None;
    if unsafe { D3D12GetDebugInterface(&mut debug) }.is_ok() {
        if let Some(debug) = debug {
            unsafe {
                debug.EnableDebugLayer();
                debug.SetEnableGPUBasedValidation(true);
            }
        }
    }

    let mut dred: Option<ID3D12DeviceRemovedExtendedDataSettings1> = None;
    if unsafe { D3D12GetDebugInterface(&mut dred) }.is_ok() {
        if let Some(dred) = dred {
            unsafe {
                dred.SetAutoBreadcrumbsEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
                dred.SetPageFaultEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
                dred.SetBreadcrumbContextEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
            }
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
