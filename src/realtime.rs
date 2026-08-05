use std::error::Error;

#[cfg(target_os = "windows")]
mod windows_app;

/// 启动实时渲染窗口。
#[cfg(target_os = "windows")]
pub fn run() -> Result<(), Box<dyn Error>> {
    windows_app::run()
}

/// 非 Windows 平台仍可编译和运行 CPU 参考模式。
#[cfg(not(target_os = "windows"))]
pub fn run() -> Result<(), Box<dyn Error>> {
    Err("实时渲染器只支持 Windows 10/11".into())
}
