use std::{ffi::c_void, time::Instant};

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

use self::{
    descriptor::DescriptorHeap, pipeline::ComputePipeline, profiler::GpuProfiler,
    resource::TrackedResource, shader::ShaderReloader, upload::UploadRing,
};

mod descriptor;
mod pipeline;
mod profiler;
mod resource;
mod shader;
mod upload;

const FRAME_COUNT: usize = 3;
const STAGE2_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stage2_gradient.dxil"));

#[repr(C)]
#[derive(Clone, Copy)]
struct FrameConstants {
    elapsed_seconds: f32,
    output_width: u32,
    output_height: u32,
    frame_index: u32,
}

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
    compute_pipeline: ComputePipeline,
    gpu_profiler: GpuProfiler,
    shader_reloader: ShaderReloader,
    shader_status: String,
    upload_ring: UploadRing,
    frames: Vec<FrameContext>,
    command_list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    next_fence_value: u64,
    fence_event: HANDLE,
    width: u32,
    height: u32,
    minimized: bool,
    start_time: Instant,
    frame_number: u32,
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
                DescriptorHeap::new(&device, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, 1, true)
                    .map_err(|error| dx_error("创建 Shader 描述符堆", error))?;
            let compute_pipeline = ComputePipeline::new(&device, STAGE2_SHADER)
                .map_err(|error| dx_error("创建 Compute Pipeline", error))?;
            let gpu_profiler = GpuProfiler::new(&device, &command_queue, FRAME_COUNT)
                .map_err(|error| dx_error("创建 GPU 计时器", error))?;
            let upload_ring = UploadRing::new(&device, FRAME_COUNT)
                .map_err(|error| dx_error("创建上传环形缓冲", error))?;

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
            command_list.Close()?;

            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let fence_event = CreateEventW(None, false, false, None)?;
            let mut renderer = Self {
                device,
                command_queue,
                swap_chain,
                rtv_heap,
                shader_heap,
                render_targets: [None, None, None],
                compute_output: None,
                compute_pipeline,
                gpu_profiler,
                shader_reloader: ShaderReloader::new(),
                shader_status: "内嵌 DXIL".to_string(),
                upload_ring,
                frames,
                command_list,
                fence,
                next_fence_value: 1,
                fence_event,
                width,
                height,
                minimized: false,
                start_time: Instant::now(),
                frame_number: 0,
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
            self.reload_shader_if_changed()?;
            let frame_index = self.swap_chain.GetCurrentBackBufferIndex() as usize;
            self.wait_for_frame(frame_index)?;
            self.gpu_profiler.collect(frame_index)?;
            let frame = &self.frames[frame_index];
            frame.allocator.Reset()?;
            self.command_list
                .Reset(&frame.allocator, None::<&ID3D12PipelineState>)?;

            let constants = FrameConstants {
                elapsed_seconds: self.start_time.elapsed().as_secs_f32(),
                output_width: self.width,
                output_height: self.height,
                frame_index: self.frame_number,
            };
            let constant_buffer = self.upload_ring.write(frame_index, &constants);
            self.command_list
                .SetDescriptorHeaps(&[Some(self.shader_heap.heap().clone())]);
            self.compute_pipeline.bind(
                &self.command_list,
                constant_buffer,
                self.shader_heap.gpu_handle(0),
            );
            self.gpu_profiler.begin(&self.command_list, frame_index);
            let compute_output = self.compute_output.as_mut().unwrap();
            compute_output.transition(&self.command_list, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            self.command_list
                .Dispatch(self.width.div_ceil(8), self.height.div_ceil(8), 1);
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
            self.frame_number = self.frame_number.wrapping_add(1);
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

    unsafe fn reload_shader_if_changed(&mut self) -> Result<()> {
        let Some(result) = self.shader_reloader.poll() else {
            return Ok(());
        };
        match result {
            Ok(shader) => {
                unsafe { self.wait_for_gpu()? };
                match ComputePipeline::new(&self.device, &shader) {
                    Ok(pipeline) => {
                        self.compute_pipeline = pipeline;
                        self.shader_status = "已热重载".to_string();
                        println!("Shader 热重载成功");
                    }
                    Err(error) => {
                        self.shader_status = "Pipeline 创建失败".to_string();
                        eprintln!("Shader Pipeline 创建失败：{error}");
                    }
                }
            }
            Err(error) => {
                self.shader_status = "编译失败，保留旧版本".to_string();
                eprintln!("{error}");
            }
        }
        Ok(())
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
                self.shader_heap.cpu_handle(0),
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
