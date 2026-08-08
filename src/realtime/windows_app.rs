use std::{
    error::Error,
    io,
    time::{Duration, Instant},
};

use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

use crate::{
    realtime::RealtimeConfig,
    renderer::d3d12::{Dx12Renderer, profiler::GpuPass},
};

pub fn run(config: RealtimeConfig) -> Result<(), Box<dyn Error>> {
    // 让窗口、截图工具和 GPU 输出统一使用物理像素，避免 200% 缩放时只截到左上角四分之一。
    unsafe {
        if let Err(error) =
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
        {
            eprintln!("设置 Per-Monitor DPI 感知失败，将使用系统当前 DPI 设置：{error}");
        }
    }
    let event_loop = EventLoop::new()
        .map_err(|error| io::Error::other(format!("创建 winit 事件循环：{error}")))?;
    let mut application = RealtimeApplication {
        config,
        ..Default::default()
    };
    event_loop
        .run_app(&mut application)
        .map_err(|error| io::Error::other(format!("运行 winit 事件循环：{error}")))?;
    if let Some(report) = application.benchmark_result {
        println!("{report}");
    }
    if let Some(message) = application.failure {
        return Err(io::Error::other(message).into());
    }
    Ok(())
}

#[derive(Default)]
struct RealtimeApplication {
    config: RealtimeConfig,
    window: Option<Window>,
    renderer: Option<Dx12Renderer>,
    failure: Option<String>,
    stats_started: Option<Instant>,
    frames_since_stats: u32,
    benchmark: Option<BenchmarkState>,
    benchmark_result: Option<String>,
}

struct BenchmarkState {
    duration: Duration,
    warmup_valid_samples: u32,
    sampling_started: Option<Instant>,
    last_serial: u64,
}

impl BenchmarkState {
    fn new(seconds: u32) -> Self {
        Self {
            duration: Duration::from_secs(seconds as u64),
            warmup_valid_samples: 0,
            sampling_started: None,
            last_serial: 0,
        }
    }
}

impl RealtimeApplication {
    fn fail(&mut self, event_loop: &ActiveEventLoop, message: impl ToString) {
        self.failure = Some(message.to_string());
        self.renderer = None;
        event_loop.exit();
    }
}

impl ApplicationHandler for RealtimeApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let output_size = self.config.output_size.unwrap_or((1280, 720));
        let attributes = Window::default_attributes()
            .with_title("RayTracingDemo - DX12 阶段 8")
            // 这里故意使用物理像素。若使用 LogicalSize，200% DPI 会把默认
            // DX12 工作尺寸隐式放大为 2560x1440，Debug Validation 成本也随之变成约 4 倍。
            .with_inner_size(PhysicalSize::new(output_size.0, output_size.1))
            .with_min_inner_size(PhysicalSize::new(320, 180));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => window,
            Err(error) => return self.fail(event_loop, format!("创建窗口：{error}")),
        };
        let size = window.inner_size();
        let logical_size = size.to_logical::<f64>(window.scale_factor());
        eprintln!(
            "DX12 窗口：DPI scale factor {:.2}，logical {:.0}x{:.0}，physical {}x{}",
            window.scale_factor(),
            logical_size.width,
            logical_size.height,
            size.width,
            size.height
        );
        let renderer =
            match Dx12Renderer::new(&window, size.width.max(1), size.height.max(1), &self.config) {
                Ok(renderer) => renderer,
                Err(error) => return self.fail(event_loop, format!("创建 DX12 后端：{error}")),
            };
        self.renderer = Some(renderer);
        self.benchmark = self.config.benchmark_seconds.map(BenchmarkState::new);
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        if window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                self.renderer = None;
                event_loop.exit();
            }
            WindowEvent::Resized(PhysicalSize { width, height }) => {
                if let Some(renderer) = self.renderer.as_mut()
                    && let Err(error) = renderer.resize(width, height)
                {
                    self.fail(event_loop, format!("调整交换链尺寸：{error}"));
                }
            }
            WindowEvent::RedrawRequested => {
                let mut benchmark_report = None;
                if let Some(renderer) = self.renderer.as_mut() {
                    if let Err(error) = renderer.render() {
                        return self.fail(event_loop, format!("提交 DX12 帧：{error}"));
                    }
                    if let Some(benchmark) = self.benchmark.as_mut() {
                        let serial = renderer.valid_timing_sample_serial();
                        let new_samples = serial.saturating_sub(benchmark.last_serial);
                        benchmark.last_serial = serial;
                        if benchmark.sampling_started.is_none() {
                            benchmark.warmup_valid_samples = benchmark
                                .warmup_valid_samples
                                .saturating_add(new_samples.min(u32::MAX as u64) as u32);
                            if benchmark.warmup_valid_samples >= 120 {
                                renderer.begin_benchmark_measurement();
                                benchmark.sampling_started = Some(Instant::now());
                            }
                        } else if benchmark
                            .sampling_started
                            .is_some_and(|started| started.elapsed() >= benchmark.duration)
                        {
                            renderer.refresh_memory_telemetry();
                            benchmark_report = Some(renderer.benchmark_json(
                                benchmark.duration.as_secs(),
                                benchmark.warmup_valid_samples,
                            ));
                        }
                    }
                    self.frames_since_stats += 1;
                    let now = Instant::now();
                    let started = self.stats_started.get_or_insert(now);
                    let elapsed = now.duration_since(*started);
                    if elapsed >= Duration::from_millis(500) {
                        let fps = self.frames_since_stats as f64 / elapsed.as_secs_f64();
                        window.set_title(&format!(
                            "RayTracingDemo - 阶段 8 | FPS {:.0} | GPU {:.2} ms (p95 {:.2}) | 输出 {}x{} | VRAM {} | AS {:.2} PT {:.2} T {:.2} A {:.2} ({}) | SPP {} | 视图 {} | {} | {}",
                            fps,
                            renderer.gpu_time_ms(),
                            renderer.gpu_time_p95_ms(),
                            renderer.output_width(),
                            renderer.output_height(),
                            renderer.video_memory_title(),
                            renderer.gpu_pass_time_ms(GpuPass::AccelerationStructure),
                            renderer.gpu_pass_time_ms(GpuPass::PathTrace),
                            renderer.gpu_pass_time_ms(GpuPass::Temporal),
                            renderer.gpu_pass_time_ms(GpuPass::Atrous),
                            renderer.atrous_mode_name(),
                            renderer.sample_count(),
                            renderer.debug_view_name(),
                            renderer.raytracing_status(),
                            renderer.shader_status()
                        ));
                        self.stats_started = Some(now);
                        self.frames_since_stats = 0;
                    }
                }
                if let Some(report) = benchmark_report {
                    self.benchmark_result = Some(report);
                    self.renderer = None;
                    event_loop.exit();
                    return;
                }
                window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(renderer) = self.renderer.as_mut() {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::KeyW) => renderer.move_camera(0.08, 0.0, 0.0),
                        PhysicalKey::Code(KeyCode::KeyS) => renderer.move_camera(-0.08, 0.0, 0.0),
                        PhysicalKey::Code(KeyCode::KeyA) => renderer.move_camera(0.0, -0.08, 0.0),
                        PhysicalKey::Code(KeyCode::KeyD) => renderer.move_camera(0.0, 0.08, 0.0),
                        PhysicalKey::Code(KeyCode::Space) => renderer.move_camera(0.0, 0.0, 0.08),
                        PhysicalKey::Code(KeyCode::ShiftLeft) => {
                            renderer.move_camera(0.0, 0.0, -0.08)
                        }
                        PhysicalKey::Code(KeyCode::ArrowLeft) => renderer.rotate_camera(-0.04, 0.0),
                        PhysicalKey::Code(KeyCode::ArrowRight) => renderer.rotate_camera(0.04, 0.0),
                        PhysicalKey::Code(KeyCode::ArrowUp) => renderer.rotate_camera(0.0, 0.04),
                        PhysicalKey::Code(KeyCode::ArrowDown) => renderer.rotate_camera(0.0, -0.04),
                        PhysicalKey::Code(KeyCode::F1) => renderer.cycle_debug_view(),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}
