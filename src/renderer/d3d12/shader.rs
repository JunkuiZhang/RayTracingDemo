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
    extra_include: Option<PathBuf>,
    last_modified: SystemTime,
}

struct WatchedFile {
    path: PathBuf,
    last_modified: SystemTime,
}

pub struct ReloadedShaders {
    pub raytracing: Vec<u8>,
    pub temporal: Vec<u8>,
    pub atrous: Vec<u8>,
    pub atrous_shared: Vec<u8>,
    pub tonemap: Vec<u8>,
    #[cfg(feature = "streamline")]
    pub dlss_compose: Vec<u8>,
    #[cfg(feature = "streamline-rr")]
    pub rr_input: Vec<u8>,
    #[cfg(feature = "streamline-rr")]
    pub rr_emissive: Vec<u8>,
    #[cfg(feature = "streamline-rr")]
    pub rr_primary_visibility: Vec<u8>,
    #[cfg(feature = "streamline-rr")]
    pub rr_boundary_resolve: Vec<u8>,
    #[cfg(feature = "nrd")]
    pub nrd_prep: Vec<u8>,
    #[cfg(feature = "nrd")]
    pub nrd_compose: Vec<u8>,
}

/// Debug-only shader reloader. A changed source causes the complete compatible
/// shader set to be rebuilt; callers only swap pipelines after every compile and
/// pipeline creation succeeds.
pub struct ShaderReloader {
    sources: Vec<ShaderSource>,
    shader_root: PathBuf,
    dependencies: Vec<WatchedFile>,
    dxc: PathBuf,
    last_check: Instant,
}

impl ShaderReloader {
    pub fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let shader_root = root.join("shaders");
        let cache = root.join("output").join("shader-cache");
        let descriptions = vec![
            (
                "stage3_triangle.hlsl",
                "stage3_triangle.dxil",
                "lib_6_6",
                None,
            ),
            (
                "stage6_temporal.hlsl",
                "stage6_temporal.dxil",
                "cs_6_6",
                None,
            ),
            ("stage6_atrous.hlsl", "stage6_atrous.dxil", "cs_6_6", None),
            (
                "stage8_atrous_shared.hlsl",
                "stage8_atrous_shared.dxil",
                "cs_6_6",
                None,
            ),
            ("stage6_tonemap.hlsl", "stage6_tonemap.dxil", "cs_6_6", None),
        ];
        #[cfg(feature = "streamline")]
        let descriptions = {
            let mut descriptions = descriptions;
            descriptions.push((
                "stage10_dlss_input.hlsl",
                "stage10_dlss_input.dxil",
                "cs_6_6",
                None,
            ));
            descriptions
        };
        #[cfg(feature = "streamline-rr")]
        let descriptions = {
            let mut descriptions = descriptions;
            descriptions.extend([
                (
                    "stage11_rr_input.hlsl",
                    "stage11_rr_input.dxil",
                    "cs_6_6",
                    None,
                ),
                (
                    "stage11_rr_emissive.hlsl",
                    "stage11_rr_emissive.dxil",
                    "cs_6_6",
                    None,
                ),
                (
                    "stage11_rr_primary_visibility.hlsl",
                    "stage11_rr_primary_visibility.dxil",
                    "cs_6_6",
                    None,
                ),
                (
                    "stage11_rr_boundary_resolve.hlsl",
                    "stage11_rr_boundary_resolve.dxil",
                    "cs_6_6",
                    None,
                ),
            ]);
            descriptions
        };
        #[cfg(feature = "nrd")]
        let descriptions = {
            let mut descriptions = descriptions;
            let nrd_shader_root = PathBuf::from(env!("RAY_TRACING_NRD_SHADER_DIR"));
            descriptions.extend([
                (
                    "stage9_nrd_prep.hlsl",
                    "stage9_nrd_prep.dxil",
                    "cs_6_6",
                    Some(nrd_shader_root.clone()),
                ),
                (
                    "stage9_nrd_compose.hlsl",
                    "stage9_nrd_compose.dxil",
                    "cs_6_6",
                    Some(nrd_shader_root),
                ),
            ]);
            descriptions
        };
        let sources = descriptions
            .into_iter()
            .map(|(source, output, target, extra_include)| {
                let source = shader_root.join(source);
                let last_modified = source
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                ShaderSource {
                    source,
                    output: cache.join(output),
                    target,
                    extra_include,
                    last_modified,
                }
            })
            .collect();
        let dependencies = collect_shader_files(&shader_root).unwrap_or_default();
        Self {
            sources,
            shader_root,
            dependencies,
            dxc: PathBuf::from(env!("RAY_TRACING_DXC")),
            last_check: Instant::now(),
        }
    }

    pub fn poll(&mut self) -> Option<Result<ReloadedShaders, String>> {
        if !cfg!(debug_assertions) || self.last_check.elapsed() < CHECK_INTERVAL {
            return None;
        }
        self.last_check = Instant::now();
        let dependencies = match collect_shader_files(&self.shader_root) {
            Ok(dependencies) => dependencies,
            Err(error) => return Some(Err(error)),
        };
        if !dependencies_changed(&self.dependencies, &dependencies) {
            return None;
        }
        if let Some(cache) = self.sources[0].output.parent()
            && let Err(error) = fs::create_dir_all(cache)
        {
            return Some(Err(format!("创建 Shader 缓存目录失败：{error}")));
        }

        for source in &self.sources {
            let mut command = Command::new(&self.dxc);
            command
                .arg(&source.source)
                .args(["-I"])
                .arg(&self.shader_root);
            if let Some(extra_include) = source.extra_include.as_ref() {
                command.args(["-I"]).arg(extra_include);
            }
            let result = command
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
        #[cfg(feature = "nrd")]
        let nrd_base = 5
            + usize::from(cfg!(feature = "streamline"))
            + 4 * usize::from(cfg!(feature = "streamline-rr"));
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
            atrous_shared: match read(3) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            },
            tonemap: match read(4) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            },
            #[cfg(feature = "streamline")]
            dlss_compose: match read(5) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            },
            #[cfg(feature = "streamline-rr")]
            rr_input: match read(6) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            },
            #[cfg(feature = "streamline-rr")]
            rr_emissive: match read(7) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            },
            #[cfg(feature = "streamline-rr")]
            rr_primary_visibility: match read(8) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            },
            #[cfg(feature = "streamline-rr")]
            rr_boundary_resolve: match read(9) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            },
            #[cfg(feature = "nrd")]
            nrd_prep: match read(nrd_base) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            },
            #[cfg(feature = "nrd")]
            nrd_compose: match read(nrd_base + 1) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            },
        };
        for source in &mut self.sources {
            source.last_modified = source
                .source
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
        }
        self.dependencies = dependencies;
        Some(Ok(shaders))
    }
}

fn collect_shader_files(root: &PathBuf) -> Result<Vec<WatchedFile>, String> {
    let mut files = Vec::new();
    collect_shader_files_recursive(root, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn collect_shader_files_recursive(
    root: &PathBuf,
    files: &mut Vec<WatchedFile>,
) -> Result<(), String> {
    let entries = fs::read_dir(root)
        .map_err(|error| format!("读取 Shader 目录 {} 失败：{error}", root.display()))?;
    for entry in entries {
        let path = entry
            .map_err(|error| format!("读取 Shader 目录项失败：{error}"))?
            .path();
        if path.is_dir() {
            collect_shader_files_recursive(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "hlsl" || extension == "hlsli")
        {
            let last_modified = path
                .metadata()
                .and_then(|metadata| metadata.modified())
                .map_err(|error| format!("读取 {} 修改时间失败：{error}", path.display()))?;
            files.push(WatchedFile {
                path,
                last_modified,
            });
        }
    }
    Ok(())
}

fn dependencies_changed(previous: &[WatchedFile], current: &[WatchedFile]) -> bool {
    previous.len() != current.len()
        || previous.iter().zip(current).any(|(previous, current)| {
            previous.path != current.path || current.last_modified > previous.last_modified
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_reloader_tracks_every_runtime_pipeline_source() {
        let reloader = ShaderReloader::new();
        let names = reloader
            .sources
            .iter()
            .map(|source| {
                source
                    .source
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        let expected = vec![
            "stage3_triangle.hlsl",
            "stage6_temporal.hlsl",
            "stage6_atrous.hlsl",
            "stage8_atrous_shared.hlsl",
            "stage6_tonemap.hlsl",
        ];
        #[cfg(feature = "streamline")]
        let expected = {
            let mut expected = expected;
            expected.push("stage10_dlss_input.hlsl");
            expected
        };
        #[cfg(feature = "streamline-rr")]
        let expected = {
            let mut expected = expected;
            expected.extend([
                "stage11_rr_input.hlsl",
                "stage11_rr_emissive.hlsl",
                "stage11_rr_primary_visibility.hlsl",
                "stage11_rr_boundary_resolve.hlsl",
            ]);
            expected
        };
        #[cfg(feature = "nrd")]
        let expected = {
            let mut expected = expected;
            expected.extend(["stage9_nrd_prep.hlsl", "stage9_nrd_compose.hlsl"]);
            expected
        };
        assert_eq!(names, expected);

        #[cfg(feature = "nrd")]
        {
            let nrd_base = 5
                + usize::from(cfg!(feature = "streamline"))
                + 4 * usize::from(cfg!(feature = "streamline-rr"));
            assert!(reloader.sources[nrd_base].extra_include.is_some());
            assert!(reloader.sources[nrd_base + 1].extra_include.is_some());
        }
        #[cfg(feature = "streamline")]
        assert_eq!(names[5], "stage10_dlss_input.hlsl");
    }
}
