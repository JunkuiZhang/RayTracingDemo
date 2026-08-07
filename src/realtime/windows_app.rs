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
    dpi::{LogicalSize, PhysicalSize},
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

use crate::renderer::d3d12::Dx12Renderer;

pub fn run() -> Result<(), Box<dyn Error>> {
    // 让窗口、截图工具和 GPU 输出统一使用物理像素，避免 200% 缩放时只截到左上角四分之一。
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let event_loop = EventLoop::new()
        .map_err(|error| io::Error::other(format!("创建 winit 事件循环：{error}")))?;
    let mut application = RealtimeApplication::default();
    event_loop
        .run_app(&mut application)
        .map_err(|error| io::Error::other(format!("运行 winit 事件循环：{error}")))?;
    if let Some(message) = application.failure {
        return Err(io::Error::other(message).into());
    }
    Ok(())
}

#[derive(Default)]
struct RealtimeApplication {
    window: Option<Window>,
    renderer: Option<Dx12Renderer>,
    failure: Option<String>,
    stats_started: Option<Instant>,
    frames_since_stats: u32,
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

        let attributes = Window::default_attributes()
            .with_title("RayTracingDemo - DX12 阶段 6")
            .with_inner_size(LogicalSize::new(1280, 720))
            .with_min_inner_size(LogicalSize::new(320, 180));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => window,
            Err(error) => return self.fail(event_loop, format!("创建窗口：{error}")),
        };
        let size = window.inner_size();
        let renderer = match Dx12Renderer::new(&window, size.width.max(1), size.height.max(1)) {
            Ok(renderer) => renderer,
            Err(error) => return self.fail(event_loop, format!("创建 DX12 后端：{error}")),
        };
        self.renderer = Some(renderer);
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
                if let Some(renderer) = self.renderer.as_mut() {
                    if let Err(error) = renderer.resize(width, height) {
                        self.fail(event_loop, format!("调整交换链尺寸：{error}"));
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(renderer) = self.renderer.as_mut() {
                    if let Err(error) = renderer.render() {
                        return self.fail(event_loop, format!("提交 DX12 帧：{error}"));
                    }
                    self.frames_since_stats += 1;
                    let now = Instant::now();
                    let started = self.stats_started.get_or_insert(now);
                    let elapsed = now.duration_since(*started);
                    if elapsed >= Duration::from_millis(500) {
                        let fps = self.frames_since_stats as f64 / elapsed.as_secs_f64();
                        window.set_title(&format!(
                            "RayTracingDemo - 阶段 6 | FPS {:.0} | GPU {:.3} ms | SPP {} | {} | Shader {}",
                            fps,
                            renderer.gpu_time_ms(),
                            renderer.sample_count(),
                            renderer.raytracing_status(),
                            renderer.shader_status()
                        ));
                        self.stats_started = Some(now);
                        self.frames_since_stats = 0;
                    }
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
                        _ => {}
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left | MouseButton::Right,
                ..
            } => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.reset_history();
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
