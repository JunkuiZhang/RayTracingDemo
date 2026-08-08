use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant, SystemTime},
};

const CHECK_INTERVAL: Duration = Duration::from_millis(500);

struct ShaderSource {
    source: PathBuf,
    output: PathBuf,
    target: &'static str,
    last_modified: SystemTime,
}

pub struct ReloadedShaders {
    pub raytracing: Vec<u8>,
    pub temporal: Vec<u8>,
    pub atrous: Vec<u8>,
    pub tonemap: Vec<u8>,
}

/// Debug-only shader reloader. A changed source causes the complete compatible
/// shader set to be rebuilt; callers only swap pipelines after every compile and
/// pipeline creation succeeds.
pub struct ShaderReloader {
    sources: Vec<ShaderSource>,
    dxc: PathBuf,
    last_check: Instant,
}

impl ShaderReloader {
    pub fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cache = root.join("output").join("shader-cache");
        let descriptions = [
            ("stage3_triangle.hlsl", "stage3_triangle.dxil", "lib_6_6"),
            ("stage6_temporal.hlsl", "stage6_temporal.dxil", "cs_6_6"),
            ("stage6_atrous.hlsl", "stage6_atrous.dxil", "cs_6_6"),
            ("stage6_tonemap.hlsl", "stage6_tonemap.dxil", "cs_6_6"),
        ];
        let sources = descriptions
            .into_iter()
            .map(|(source, output, target)| {
                let source = root.join("shaders").join(source);
                let last_modified = source
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                ShaderSource {
                    source,
                    output: cache.join(output),
                    target,
                    last_modified,
                }
            })
            .collect();
        Self {
            sources,
            dxc: PathBuf::from(env!("RAY_TRACING_DXC")),
            last_check: Instant::now(),
        }
    }

    pub fn poll(&mut self) -> Option<Result<ReloadedShaders, String>> {
        if !cfg!(debug_assertions) || self.last_check.elapsed() < CHECK_INTERVAL {
            return None;
        }
        self.last_check = Instant::now();
        let modified = self
            .sources
            .iter()
            .map(|source| {
                source
                    .source
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .map_err(|error| {
                        format!("读取 {} 修改时间失败：{error}", source.source.display())
                    })
            })
            .collect::<Result<Vec<_>, _>>();
        let modified = match modified {
            Ok(modified) => modified,
            Err(error) => return Some(Err(error)),
        };
        if !modified
            .iter()
            .zip(&self.sources)
            .any(|(modified, source)| *modified > source.last_modified)
        {
            return None;
        }
        if let Some(cache) = self.sources[0].output.parent()
            && let Err(error) = fs::create_dir_all(cache)
        {
            return Some(Err(format!("创建 Shader 缓存目录失败：{error}")));
        }

        for source in &self.sources {
            let result = Command::new(&self.dxc)
                .arg(&source.source)
                .args(["-T", source.target, "-HV", "2021", "-Fo"])
                .arg(&source.output)
                .args(["-Od", "-Zi", "-Qembed_debug"])
                .output();
            let result = match result {
                Ok(result) => result,
                Err(error) => return Some(Err(format!("启动 DXC 失败：{error}"))),
            };
            if !result.status.success() {
                return Some(Err(format!(
                    "Shader 编译失败：{} ({})\n{}",
                    source.source.display(),
                    source.target,
                    String::from_utf8_lossy(&result.stderr).trim()
                )));
            }
        }
        let read = |index: usize| {
            fs::read(&self.sources[index].output).map_err(|error| {
                format!(
                    "读取 {} 失败：{error}",
                    self.sources[index].output.display()
                )
            })
        };
        let shaders = ReloadedShaders {
            raytracing: match read(0) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            },
            temporal: match read(1) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            },
            atrous: match read(2) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            },
            tonemap: match read(3) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            },
        };
        for (source, modified) in self.sources.iter_mut().zip(modified) {
            source.last_modified = modified;
        }
        Some(Ok(shaders))
    }
}
