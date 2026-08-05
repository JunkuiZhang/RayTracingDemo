use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant, SystemTime},
};

const CHECK_INTERVAL: Duration = Duration::from_millis(500);

/// 监视 HLSL 修改时间，并在运行时调用 DXC 生成新的 DXIL。
pub struct ShaderReloader {
    source: PathBuf,
    output: PathBuf,
    dxc: PathBuf,
    last_modified: SystemTime,
    last_check: Instant,
}

impl ShaderReloader {
    pub fn new() -> Self {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("shaders")
            .join("stage2_gradient.hlsl");
        let output = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("output")
            .join("shader-cache")
            .join("stage2_gradient.dxil");
        let last_modified = source
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        Self {
            source,
            output,
            dxc: PathBuf::from(env!("RAY_TRACING_DXC")),
            last_modified,
            last_check: Instant::now(),
        }
    }

    pub fn poll(&mut self) -> Option<Result<Vec<u8>, String>> {
        if self.last_check.elapsed() < CHECK_INTERVAL {
            return None;
        }
        self.last_check = Instant::now();
        let modified = match self
            .source
            .metadata()
            .and_then(|metadata| metadata.modified())
        {
            Ok(modified) => modified,
            Err(error) => return Some(Err(format!("读取 Shader 时间失败：{error}"))),
        };
        if modified <= self.last_modified {
            return None;
        }

        if let Some(parent) = self.output.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                return Some(Err(format!("创建 Shader 缓存目录失败：{error}")));
            }
        }
        let result = Command::new(&self.dxc)
            .arg(&self.source)
            .args(["-E", "main", "-T", "cs_6_0", "-Fo"])
            .arg(&self.output)
            .args(["-Od", "-Zi", "-Qembed_debug"])
            .output();
        let result = match result {
            Ok(result) => result,
            Err(error) => return Some(Err(format!("启动 DXC 失败：{error}"))),
        };
        if !result.status.success() {
            return Some(Err(format!(
                "Shader 编译失败：{}",
                String::from_utf8_lossy(&result.stderr).trim()
            )));
        }
        match fs::read(&self.output) {
            Ok(shader) => {
                self.last_modified = modified;
                Some(Ok(shader))
            }
            Err(error) => Some(Err(format!("读取 DXIL 失败：{error}"))),
        }
    }
}
