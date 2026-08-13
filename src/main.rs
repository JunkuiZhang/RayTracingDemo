#![recursion_limit = "256"]

use std::{env, path::PathBuf, process::ExitCode};

use cpu_reference::CpuReferenceConfig;

mod as_policy;
mod camera;
mod cpu_reference;
mod data;
mod debug_view;
mod entity;
mod material;
#[allow(dead_code)] // P2 consumes this tested CPU/HLSL identity contract.
mod path_space;
mod realtime;
// 9A keeps the ABI/guide structs available for the staged 9B/9C integration;
// some fields are intentionally not consumed until the optional backend exists.
#[allow(dead_code)]
mod reconstruction;
mod renderer;
mod resolution;
mod scene;
mod settings;
mod some_math;
#[cfg(feature = "streamline")]
#[allow(dead_code)] // 10C/10D consume the ABI types from the renderer.
mod streamline;
mod systems;
#[allow(dead_code)] // 10D consumes the contract from the renderer path.
mod upscaler;
mod world;

use debug_view::DebugView;
use resolution::{
    DynamicResolutionConfig, DynamicResolutionConfigError, RenderScale, RenderScaleError,
    ResolutionMode,
};

fn main() -> ExitCode {
    #[cfg(feature = "streamline")]
    let _streamline_sdk_version = streamline::SDK_VERSION;
    match parse_arguments(env::args().skip(1)) {
        Ok(Command::CpuReference(config)) => match cpu_reference::run(config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("CPU 参考渲染失败：{error}");
                ExitCode::FAILURE
            }
        },
        Ok(Command::Realtime(config)) => match realtime::run(config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("实时 DX12 渲染器启动失败：{error}");
                ExitCode::FAILURE
            }
        },
        Ok(Command::Help) => {
            print_help();
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("参数错误：{error}\n");
            print_help();
            ExitCode::FAILURE
        }
    }
}

enum Command {
    Realtime(realtime::RealtimeConfig),
    CpuReference(CpuReferenceConfig),
    Help,
}

fn parse_arguments(arguments: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        if arguments.len() == 1 {
            return Ok(Command::Help);
        }
        return Err("--help 不能与其他参数同时使用".to_string());
    }
    let cpu_reference_requested = arguments
        .iter()
        .any(|argument| argument == "--cpu-reference");
    let realtime_requested = arguments.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--model"
                | "--animate-model"
                | "--benchmark-seconds"
                | "--capture-output"
                | "--capture-after-spp"
                | "--debug-view"
                | "--atrous-mode"
                | "--output-size"
                | "--render-scale"
                | "--dynamic-resolution"
                | "--target-gpu-ms"
                | "--command-recording-mode"
                | "--acceleration-structure-mode"
                | "--denoiser"
                | "--upscaler"
                | "--reflex-mode"
                | "--streamline-application-id"
        )
    });
    if cpu_reference_requested && realtime_requested {
        return Err(
            "--cpu-reference 不能与实时渲染选项（--model、--animate-model、--benchmark-seconds、--capture-output、--capture-after-spp、--debug-view、--atrous-mode、--output-size、--render-scale、--dynamic-resolution、--target-gpu-ms、--command-recording-mode、--acceleration-structure-mode、--denoiser、--upscaler、--reflex-mode、--streamline-application-id）同时使用"
                .to_string(),
        );
    }

    if !cpu_reference_requested {
        let dynamic_requested = arguments
            .iter()
            .any(|argument| argument == "--dynamic-resolution");
        let render_scale_requested = arguments
            .iter()
            .any(|argument| argument == "--render-scale");
        let upscaler_requested = arguments.iter().any(|argument| argument == "--upscaler");
        let target_requested = arguments
            .iter()
            .any(|argument| argument == "--target-gpu-ms");
        let capture_requested = arguments
            .iter()
            .any(|argument| argument == "--capture-output");
        let capture_spp_requested = arguments
            .iter()
            .any(|argument| argument == "--capture-after-spp");
        let benchmark_requested = arguments
            .iter()
            .any(|argument| argument == "--benchmark-seconds");
        if dynamic_requested && render_scale_requested {
            return Err("--dynamic-resolution 不能与 --render-scale 同时使用".to_string());
        }
        if target_requested && !dynamic_requested {
            return Err("--target-gpu-ms 只能与 --dynamic-resolution 一起使用".to_string());
        }
        if capture_requested && benchmark_requested {
            return Err("--capture-output 不能与 --benchmark-seconds 同时使用".to_string());
        }
        if capture_spp_requested && !capture_requested {
            return Err("--capture-after-spp 只能与 --capture-output 一起使用".to_string());
        }
        let mut config = realtime::RealtimeConfig::default();
        let mut dynamic_target = DynamicResolutionConfig::default();
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--model" => {
                    let value = arguments.next().ok_or("--model 缺少路径")?;
                    let path = PathBuf::from(&value);
                    if !path.is_file() {
                        return Err(format!("模型文件不存在或不是文件：{}", path.display()));
                    }
                    config.model_path = Some(path);
                }
                "--animate-model" => config.animate_model = true,
                "--benchmark-seconds" => {
                    let value = arguments.next().ok_or("--benchmark-seconds 缺少数值")?;
                    config.benchmark_seconds = Some(parse_benchmark_seconds(&value)?);
                }
                "--capture-output" => {
                    let value = arguments.next().ok_or("--capture-output 缺少 PNG 路径")?;
                    let path = PathBuf::from(value);
                    if path.exists() {
                        return Err(format!("capture 输出已存在，拒绝覆盖：{}", path.display()));
                    }
                    config.capture_output = Some(path);
                }
                "--capture-after-spp" => {
                    let value = arguments.next().ok_or("--capture-after-spp 缺少数值")?;
                    config.capture_after_spp = Some(parse_capture_after_spp(&value)?);
                }
                "--debug-view" => {
                    let value = arguments.next().ok_or("--debug-view 缺少名称")?;
                    config.debug_view = parse_debug_view(&value)?;
                }
                "--atrous-mode" => {
                    let value = arguments.next().ok_or("--atrous-mode 缺少模式")?;
                    config.atrous_mode = parse_atrous_mode(&value)?;
                }
                "--output-size" => {
                    let value = arguments.next().ok_or("--output-size 缺少尺寸")?;
                    config.output_size = Some(parse_output_size(&value)?);
                }
                "--render-scale" => {
                    let value = arguments.next().ok_or("--render-scale 缺少比例")?;
                    config.resolution_mode = ResolutionMode::Fixed(parse_render_scale(&value)?);
                }
                "--dynamic-resolution" => {
                    config.resolution_mode = ResolutionMode::Dynamic(dynamic_target);
                }
                "--target-gpu-ms" => {
                    let value = arguments.next().ok_or("--target-gpu-ms 缺少毫秒数")?;
                    dynamic_target = parse_target_gpu_ms(&value)?;
                    config.resolution_mode = ResolutionMode::Dynamic(dynamic_target);
                }
                "--command-recording-mode" => {
                    let value = arguments
                        .next()
                        .ok_or("--command-recording-mode 缺少模式")?;
                    config.command_recording_mode = parse_command_recording_mode(&value)?;
                }
                "--acceleration-structure-mode" => {
                    let value = arguments
                        .next()
                        .ok_or("--acceleration-structure-mode 缺少模式")?;
                    config.acceleration_structure_mode = parse_acceleration_structure_mode(&value)?;
                }
                "--denoiser" => {
                    let value = arguments.next().ok_or("--denoiser 缺少后端")?;
                    config.denoiser = parse_denoiser_backend(&value)?;
                }
                "--upscaler" => {
                    let value = arguments.next().ok_or("--upscaler 缺少模式")?;
                    config.upscaler = parse_upscaler_mode(&value)?;
                    config.requested_upscaler = Some(config.upscaler);
                }
                "--reflex-mode" => {
                    let value = arguments.next().ok_or("--reflex-mode 缺少模式")?;
                    config.reflex_mode = parse_reflex_mode(&value)?;
                }
                "--streamline-application-id" => {
                    let value = arguments
                        .next()
                        .ok_or("--streamline-application-id 缺少数值")?;
                    let application_id = value
                        .parse::<u32>()
                        .map_err(|_| "--streamline-application-id 必须是非零 u32".to_string())?;
                    if application_id == 0 {
                        return Err("--streamline-application-id 必须是非零 u32".to_string());
                    }
                    config.streamline_application_id = Some(application_id);
                }
                _ => return Err(format!("未知参数：{argument}")),
            }
        }
        if config.denoiser == reconstruction::DenoiserBackend::DlssRayReconstruction {
            if !upscaler_requested {
                config.upscaler = upscaler::UpscalerMode::DlssQuality;
            } else if !matches!(
                config.upscaler,
                upscaler::UpscalerMode::DlssQuality
                    | upscaler::UpscalerMode::DlssBalanced
                    | upscaler::UpscalerMode::DlssPerformance
            ) {
                return Err(
                    "--denoiser dlss-rr 只支持 dlss-quality、dlss-balanced 或 dlss-performance；RR 已融合降噪与超分，不能与 native/DLAA 或 DLSS SR 串联"
                        .to_string(),
                );
            }
        }
        // Validate the resolved mode, not only an explicitly requested one.
        // RR implicitly selects Quality, so checking before this point would
        // accidentally let dynamic resolution/render scale bypass the fixed
        // optimal-extent contract used by the first RR increment.
        if config.upscaler != upscaler::UpscalerMode::Native {
            if dynamic_requested {
                return Err("非 Native upscaler 不能与 --dynamic-resolution 同时使用".to_string());
            }
            if render_scale_requested {
                return Err("非 Native upscaler 的内部尺寸由 Streamline optimal settings 决定，不能与 --render-scale 同时使用".to_string());
            }
        }
        if config.capture_output.is_none() && config.capture_after_spp.is_some() {
            return Err("--capture-after-spp 只能与 --capture-output 一起使用".to_string());
        }
        return Ok(Command::Realtime(config));
    }

    let mut arguments = arguments.into_iter();
    let mut config = CpuReferenceConfig::default();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--samples" => {
                let value = arguments.next().ok_or("--samples 缺少数值")?;
                config.samples_per_pixel = value
                    .parse::<usize>()
                    .map_err(|_| format!("无效的采样数：{value}"))?;
                if config.samples_per_pixel == 0 {
                    return Err("采样数必须大于零".to_string());
                }
            }
            "--seed" => {
                let value = arguments.next().ok_or("--seed 缺少数值")?;
                config.seed = parse_seed(&value)?;
            }
            "--output-dir" => {
                let value = arguments.next().ok_or("--output-dir 缺少路径")?;
                config.output_dir = PathBuf::from(value);
            }
            "--skip-denoise" => config.denoise = false,
            "--cpu-reference" => {}
            _ => return Err(format!("未知参数：{argument}")),
        }
    }
    Ok(Command::CpuReference(config))
}

fn parse_seed(value: &str) -> Result<u64, String> {
    let parsed = if let Some(hexadecimal) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hexadecimal, 16)
    } else {
        value.parse::<u64>()
    };
    parsed.map_err(|_| format!("无效的随机种子：{value}"))
}

const MAX_BENCHMARK_SECONDS: u32 = 3600;

fn parse_benchmark_seconds(value: &str) -> Result<u32, String> {
    let seconds = value
        .parse::<u32>()
        .map_err(|_| format!("无效的 benchmark 时长：{value}"))?;
    if seconds == 0 {
        return Err("benchmark 时长必须大于零".to_string());
    }
    if seconds > MAX_BENCHMARK_SECONDS {
        return Err(format!("benchmark 时长不能超过 {MAX_BENCHMARK_SECONDS} 秒"));
    }
    Ok(seconds)
}

fn parse_atrous_mode(value: &str) -> Result<realtime::AtrousMode, String> {
    match value {
        "baseline" => Ok(realtime::AtrousMode::Baseline),
        "shared" => Ok(realtime::AtrousMode::Shared),
        _ => Err(format!(
            "无效的 À-Trous 模式：{value}（仅支持 baseline 或 shared）"
        )),
    }
}

fn parse_output_size(value: &str) -> Result<(u32, u32), String> {
    let (width, height) = value
        .split_once('x')
        .or_else(|| value.split_once('X'))
        .ok_or_else(|| format!("无效的输出尺寸：{value}（格式应为 WIDTHxHEIGHT）"))?;
    let width = width
        .parse::<u32>()
        .map_err(|_| format!("无效的输出宽度：{width}"))?;
    let height = height
        .parse::<u32>()
        .map_err(|_| format!("无效的输出高度：{height}"))?;
    if !(320..=7680).contains(&width) || !(180..=4320).contains(&height) {
        return Err("输出尺寸必须在 320x180 至 7680x4320 范围内".to_string());
    }
    Ok((width, height))
}

fn parse_render_scale(value: &str) -> Result<RenderScale, String> {
    let parsed = value
        .parse::<f32>()
        .map_err(|_| format!("无效的 --render-scale：{value}（允许范围为 0.5..1.0 的有限数值）"))?;
    RenderScale::new(parsed).map_err(|error| {
        let reason = match error {
            RenderScaleError::NotFinite => "必须是有限数值",
            RenderScaleError::BelowMinimum => "不能小于 0.5",
            RenderScaleError::AboveMaximum => "不能大于 1.0",
        };
        format!("无效的 --render-scale：{value}（{reason}，允许范围为 0.5..1.0）")
    })
}

fn parse_target_gpu_ms(value: &str) -> Result<DynamicResolutionConfig, String> {
    let parsed = value.parse::<f64>().map_err(|_| {
        format!("无效的 --target-gpu-ms：{value}（允许范围为 4.0..50.0 的有限数值）")
    })?;
    DynamicResolutionConfig::from_milliseconds(parsed).map_err(|error| {
        let reason = match error {
            DynamicResolutionConfigError::NotFinite => "必须是有限数值",
            DynamicResolutionConfigError::BelowMinimum => "不能小于 4.0",
            DynamicResolutionConfigError::AboveMaximum => "不能大于 50.0",
        };
        format!("无效的 --target-gpu-ms：{value}（{reason}，允许范围为 4.0..50.0）")
    })
}

fn parse_capture_after_spp(value: &str) -> Result<u32, String> {
    let spp = value
        .parse::<u32>()
        .map_err(|_| format!("无效的 --capture-after-spp：{value}（允许范围为 1..4096）"))?;
    if !(1..=4096).contains(&spp) {
        return Err(format!(
            "无效的 --capture-after-spp：{value}（允许范围为 1..4096）"
        ));
    }
    Ok(spp)
}

fn parse_debug_view(value: &str) -> Result<DebugView, String> {
    DebugView::from_name(value).ok_or_else(|| {
        format!(
            "无效的 --debug-view：{value}（支持 final、raw、albedo、normal-roughness、depth、motion、variance、history-rejection、history-length、object-material-id、specular-hit-distance、nrd-validation、specular-motion、rr-primary-emissive）"
        )
    })
}

fn parse_command_recording_mode(value: &str) -> Result<realtime::CommandRecordingMode, String> {
    match value {
        "baseline" => Ok(realtime::CommandRecordingMode::Baseline),
        "optimized" => Ok(realtime::CommandRecordingMode::Optimized),
        _ => Err(format!(
            "无效的命令记录模式：{value}（仅支持 baseline 或 optimized）"
        )),
    }
}

fn parse_acceleration_structure_mode(
    value: &str,
) -> Result<realtime::AccelerationStructureMode, String> {
    match value {
        "baseline" => Ok(realtime::AccelerationStructureMode::Baseline),
        "optimized" => Ok(realtime::AccelerationStructureMode::Optimized),
        _ => Err(format!(
            "无效的加速结构模式：{value}（仅支持 baseline 或 optimized）"
        )),
    }
}

fn parse_denoiser_backend(value: &str) -> Result<reconstruction::DenoiserBackend, String> {
    match value {
        "svgf" => Ok(reconstruction::DenoiserBackend::Svgf),
        "nrd-reblur" => Ok(reconstruction::DenoiserBackend::NrdReblur),
        "dlss-rr" => Ok(reconstruction::DenoiserBackend::DlssRayReconstruction),
        _ => Err(format!(
            "无效的降噪后端：{value}（仅支持 svgf、nrd-reblur 或 dlss-rr）"
        )),
    }
}

fn parse_upscaler_mode(value: &str) -> Result<upscaler::UpscalerMode, String> {
    match value {
        "native" => Ok(upscaler::UpscalerMode::Native),
        "dlaa" => Ok(upscaler::UpscalerMode::Dlaa),
        "dlss-quality" => Ok(upscaler::UpscalerMode::DlssQuality),
        "dlss-balanced" => Ok(upscaler::UpscalerMode::DlssBalanced),
        "dlss-performance" => Ok(upscaler::UpscalerMode::DlssPerformance),
        _ => Err(format!(
            "无效的 upscaler：{value}（仅支持 native、dlaa、dlss-quality、dlss-balanced 或 dlss-performance）"
        )),
    }
}

fn parse_reflex_mode(value: &str) -> Result<realtime::ReflexMode, String> {
    match value {
        "off" => Ok(realtime::ReflexMode::Off),
        "on" => Ok(realtime::ReflexMode::On),
        "on-boost" => Ok(realtime::ReflexMode::OnBoost),
        _ => Err(format!(
            "无效的 Reflex 模式：{value}（仅支持 off、on 或 on-boost）"
        )),
    }
}

fn print_help() {
    println!(
        "RayTracingDemo\n\n\
         用法：\n  \
         cargo run --release                 启动实时 DX12 窗口\n  \
         cargo run --release -- --model <路径> [--animate-model] [--benchmark-seconds <秒> | --capture-output <PNG>] [--capture-after-spp <SPP>] [--debug-view <名称>] [--atrous-mode <模式>] [--output-size <宽x高>] [--render-scale <比例> | --dynamic-resolution [--target-gpu-ms <毫秒>]] [--command-recording-mode <模式>] [--acceleration-structure-mode <模式>] [--denoiser <后端>] [--upscaler <模式>] [--streamline-application-id <ID>]\n  \
         cargo run --release -- --cpu-reference [选项]\n\n\
         选项：\n  \
         --samples <数量>       每像素采样数，默认 1\n  \
         --seed <整数或十六进制> 固定随机种子\n  \
         --output-dir <目录>    输出目录，默认 output/cpu-reference\n  \
         --skip-denoise         只保存原始路径追踪结果\n  \
         --benchmark-seconds <秒> 预热后输出固定格式 GPU JSON 报告（1..3600）\n  \
         --capture-output <PNG>   fence-safe 截图并输出一行 capture JSON\n  \
         --capture-after-spp <SPP> 截图目标 SPP（1..4096，默认 128）\n  \
         --debug-view <名称>      final/raw/albedo/normal-roughness/depth/motion/variance/history-rejection/history-length/object-material-id/specular-hit-distance/nrd-validation/specular-motion\n  \
         --atrous-mode <模式>      À-Trous 路径：baseline 或 shared，默认 baseline\n  \
         --output-size <宽x高>     窗口物理像素尺寸，范围 320x180..7680x4320\n  \
         --render-scale <比例>    固定内部渲染比例，有限数值 0.5..1.0，默认 1.0\n  \
         --dynamic-resolution     使用 GPU Total timestamp 动态调整 0.67..1.0\n  \
         --target-gpu-ms <毫秒>   动态目标，有限数值 4.0..50.0，默认 14.5\n  \
         --command-recording-mode <模式> 命令记录：baseline 或 optimized，默认 optimized\n  \
         --acceleration-structure-mode <模式> AS 策略：baseline 或 optimized，默认 baseline\n  \
         --denoiser <后端>       重建后端：svgf、nrd-reblur 或 dlss-rr，默认 svgf；RR 未指定时使用 dlss-quality\n  \
         --upscaler <模式>       上采样：native、dlaa、dlss-quality、dlss-balanced、dlss-performance，默认 native\n  \
         --reflex-mode <模式>    Reflex：off、on 或 on-boost，默认 on；feature-off 时 unavailable\n  \
         --streamline-application-id <ID> 可选的 NVIDIA 分配 NGX application ID；默认使用内置 Project ID\n  \\
         --help, -h             显示帮助"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::realtime::RealtimeConfig;

    #[test]
    fn parses_decimal_and_hexadecimal_seeds() {
        assert_eq!(parse_seed("42").unwrap(), 42);
        assert_eq!(parse_seed("0x2A").unwrap(), 42);
        assert!(parse_seed("0xGG").is_err());
    }

    #[test]
    fn rejects_zero_cpu_samples() {
        let result = parse_arguments([
            "--cpu-reference".to_string(),
            "--samples".to_string(),
            "0".to_string(),
        ]);
        assert!(matches!(result, Err(message) if message.contains("必须大于零")));
    }

    #[test]
    fn realtime_is_the_default_command() {
        assert!(matches!(
            parse_arguments(Vec::<String>::new()),
            Ok(Command::Realtime(RealtimeConfig {
                command_recording_mode: crate::realtime::CommandRecordingMode::Optimized,
                ..
            }))
        ));
    }

    #[test]
    fn rejects_missing_model_path() {
        let result = parse_arguments(["--model".to_string(), "does-not-exist.gltf".to_string()]);
        assert!(matches!(result, Err(message) if message.contains("模型文件不存在")));
    }

    #[test]
    fn rejects_realtime_options_with_cpu_reference() {
        let result =
            parse_arguments(["--cpu-reference".to_string(), "--animate-model".to_string()]);
        assert!(matches!(result, Err(message) if message.contains("不能与")));
    }

    #[test]
    fn parses_and_bounds_benchmark_duration() {
        let command =
            parse_arguments(["--benchmark-seconds".to_string(), "30".to_string()]).unwrap();
        assert!(matches!(
            command,
            Command::Realtime(RealtimeConfig {
                benchmark_seconds: Some(30),
                ..
            })
        ));
        assert!(parse_benchmark_seconds("0").is_err());
        assert!(parse_benchmark_seconds("3601").is_err());
        assert!(parse_benchmark_seconds("abc").is_err());
    }

    #[test]
    fn benchmark_option_is_not_allowed_with_cpu_reference() {
        let result = parse_arguments([
            "--cpu-reference".to_string(),
            "--benchmark-seconds".to_string(),
            "30".to_string(),
        ]);
        assert!(matches!(result, Err(message) if message.contains("不能与")));
    }

    #[test]
    fn parses_and_rejects_atrous_modes() {
        let command = parse_arguments(["--atrous-mode".to_string(), "shared".to_string()]).unwrap();
        assert!(matches!(
            command,
            Command::Realtime(RealtimeConfig {
                atrous_mode: crate::realtime::AtrousMode::Shared,
                ..
            })
        ));
        assert!(parse_atrous_mode("baseline").is_ok());
        assert!(parse_atrous_mode("other").is_err());
    }

    #[test]
    fn parses_and_bounds_output_size() {
        let command =
            parse_arguments(["--output-size".to_string(), "1600x900".to_string()]).unwrap();
        assert!(matches!(
            command,
            Command::Realtime(RealtimeConfig {
                output_size: Some((1600, 900)),
                ..
            })
        ));
        assert_eq!(parse_output_size("1920X1080"), Ok((1920, 1080)));
        assert!(parse_output_size("1920").is_err());
        assert!(parse_output_size("0x1080").is_err());
        assert!(parse_output_size("8000x4500").is_err());
    }

    #[test]
    fn render_scale_defaults_parses_and_rejects_invalid_values() {
        assert!(matches!(
            parse_arguments(Vec::<String>::new()),
            Ok(Command::Realtime(RealtimeConfig {
                resolution_mode: ResolutionMode::Fixed(render_scale),
                ..
            })) if render_scale == RenderScale::NATIVE
        ));
        let command = parse_arguments(["--render-scale".to_string(), "0.67".to_string()]).unwrap();
        assert!(matches!(
            command,
            Command::Realtime(RealtimeConfig {
                resolution_mode: ResolutionMode::Fixed(render_scale),
                ..
            })
                if (render_scale.get() - 0.67).abs() < f32::EPSILON
        ));
        for value in ["NaN", "inf", "-inf", "0.49", "1.01", "text"] {
            assert!(matches!(
                parse_arguments(["--render-scale".to_string(), value.to_string()]),
                Err(error) if error.contains("--render-scale") && error.contains("0.5..1.0")
            ));
        }
        assert!(parse_arguments(["--render-scale".to_string()]).is_err());
    }

    #[test]
    fn dynamic_resolution_defaults_to_native_and_parses_target() {
        let command = parse_arguments(["--dynamic-resolution".to_string()]).unwrap();
        assert!(matches!(
            command,
            Command::Realtime(RealtimeConfig {
                resolution_mode: ResolutionMode::Dynamic(config),
                ..
            }) if config.target_gpu_time_us() == 14_500
        ));
        let command = parse_arguments([
            "--dynamic-resolution".to_string(),
            "--target-gpu-ms".to_string(),
            "4.0".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            command,
            Command::Realtime(RealtimeConfig {
                resolution_mode: ResolutionMode::Dynamic(config),
                ..
            }) if config.target_gpu_time_us() == 4_000
        ));
    }

    #[test]
    fn dynamic_resolution_rejects_fixed_scale_and_target_without_dynamic_mode() {
        assert!(
            parse_arguments([
                "--dynamic-resolution".to_string(),
                "--render-scale".to_string(),
                "1.0".to_string(),
            ])
            .is_err()
        );
        assert!(parse_arguments(["--target-gpu-ms".to_string(), "14.5".to_string(),]).is_err());
        for value in [
            "NaN", "inf", "-inf", "3.9", "3.9996", "50.0004", "50.1", "text",
        ] {
            assert!(
                parse_arguments([
                    "--dynamic-resolution".to_string(),
                    "--target-gpu-ms".to_string(),
                    value.to_string(),
                ])
                .is_err()
            );
        }
    }

    #[test]
    fn render_scale_conflicts_with_cpu_reference() {
        let result = parse_arguments([
            "--cpu-reference".to_string(),
            "--render-scale".to_string(),
            "0.75".to_string(),
        ]);
        assert!(matches!(result, Err(message) if message.contains("不能与")));
    }

    #[test]
    fn parses_and_rejects_command_recording_modes() {
        let command = parse_arguments([
            "--command-recording-mode".to_string(),
            "optimized".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            command,
            Command::Realtime(RealtimeConfig {
                command_recording_mode: crate::realtime::CommandRecordingMode::Optimized,
                ..
            })
        ));
        assert!(parse_command_recording_mode("baseline").is_ok());
        assert!(parse_command_recording_mode("other").is_err());
        assert!(parse_arguments(["--command-recording-mode".to_string()]).is_err());
    }

    #[test]
    fn acceleration_structure_mode_defaults_to_baseline_and_parses() {
        assert!(matches!(
            parse_arguments(Vec::<String>::new()),
            Ok(Command::Realtime(RealtimeConfig {
                acceleration_structure_mode: crate::realtime::AccelerationStructureMode::Baseline,
                command_recording_mode: crate::realtime::CommandRecordingMode::Optimized,
                ..
            }))
        ));
        let command = parse_arguments([
            "--acceleration-structure-mode".to_string(),
            "optimized".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            command,
            Command::Realtime(RealtimeConfig {
                acceleration_structure_mode: crate::realtime::AccelerationStructureMode::Optimized,
                ..
            })
        ));
        assert!(parse_acceleration_structure_mode("unknown").is_err());
        assert!(parse_arguments(["--acceleration-structure-mode".to_string()]).is_err());
    }

    #[test]
    fn acceleration_structure_mode_conflicts_with_cpu_reference() {
        let result = parse_arguments([
            "--cpu-reference".to_string(),
            "--acceleration-structure-mode".to_string(),
            "baseline".to_string(),
        ]);
        assert!(matches!(result, Err(message) if message.contains("不能与")));
    }

    #[test]
    fn denoiser_defaults_to_svgf_and_parses_explicit_backend() {
        assert!(matches!(
            parse_arguments(Vec::<String>::new()),
            Ok(Command::Realtime(RealtimeConfig {
                denoiser: crate::reconstruction::DenoiserBackend::Svgf,
                ..
            }))
        ));
        let command = parse_arguments(["--denoiser".to_string(), "nrd-reblur".to_string()]);
        assert!(matches!(
            command,
            Ok(Command::Realtime(RealtimeConfig {
                denoiser: crate::reconstruction::DenoiserBackend::NrdReblur,
                ..
            }))
        ));
        let command = parse_arguments(["--denoiser".to_string(), "dlss-rr".to_string()]);
        assert!(matches!(
            command,
            Ok(Command::Realtime(RealtimeConfig {
                denoiser: crate::reconstruction::DenoiserBackend::DlssRayReconstruction,
                upscaler: crate::upscaler::UpscalerMode::DlssQuality,
                requested_upscaler: None,
                ..
            }))
        ));
        let command = parse_arguments([
            "--denoiser".to_string(),
            "dlss-rr".to_string(),
            "--upscaler".to_string(),
            "dlss-balanced".to_string(),
        ]);
        assert!(matches!(
            command,
            Ok(Command::Realtime(RealtimeConfig {
                upscaler: crate::upscaler::UpscalerMode::DlssBalanced,
                requested_upscaler: Some(crate::upscaler::UpscalerMode::DlssBalanced),
                ..
            }))
        ));
        assert!(
            parse_arguments([
                "--denoiser".to_string(),
                "dlss-rr".to_string(),
                "--upscaler".to_string(),
                "native".to_string(),
            ])
            .is_err()
        );
        assert!(parse_denoiser_backend("invalid").is_err());
        assert!(parse_arguments(["--denoiser".to_string()]).is_err());
    }

    #[test]
    fn denoiser_is_covered_by_cpu_reference_conflict_guard() {
        let result = parse_arguments([
            "--cpu-reference".to_string(),
            "--denoiser".to_string(),
            "svgf".to_string(),
        ]);
        assert!(matches!(result, Err(message) if message.contains("不能与")));
    }

    #[test]
    fn implicit_rr_quality_obeys_fixed_extent_conflicts() {
        for conflicting_option in ["--dynamic-resolution", "--render-scale"] {
            let mut arguments = vec!["--denoiser".to_string(), "dlss-rr".to_string()];
            arguments.push(conflicting_option.to_string());
            if conflicting_option == "--render-scale" {
                arguments.push("0.67".to_string());
            }
            let error = match parse_arguments(arguments) {
                Err(error) => error,
                Ok(_) => {
                    panic!("implicit RR Quality must not bypass fixed optimal-extent validation")
                }
            };
            assert!(
                error.contains("不能与"),
                "unexpected conflict diagnostic: {error}"
            );
        }
    }

    #[test]
    fn upscaler_defaults_to_native_and_parses_all_modes() {
        assert!(matches!(
            parse_arguments(Vec::<String>::new()),
            Ok(Command::Realtime(RealtimeConfig {
                upscaler: crate::upscaler::UpscalerMode::Native,
                ..
            }))
        ));
        for (value, expected) in [
            ("native", crate::upscaler::UpscalerMode::Native),
            ("dlaa", crate::upscaler::UpscalerMode::Dlaa),
            ("dlss-quality", crate::upscaler::UpscalerMode::DlssQuality),
            ("dlss-balanced", crate::upscaler::UpscalerMode::DlssBalanced),
            (
                "dlss-performance",
                crate::upscaler::UpscalerMode::DlssPerformance,
            ),
        ] {
            let command = parse_arguments(["--upscaler".to_string(), value.to_string()]).unwrap();
            assert!(matches!(
                command,
                Command::Realtime(RealtimeConfig { upscaler, .. }) if upscaler == expected
            ));
        }
        assert!(parse_upscaler_mode("invalid").is_err());
        assert!(parse_arguments(["--upscaler".to_string()]).is_err());
    }

    #[test]
    fn reflex_defaults_and_parses_all_modes() {
        assert!(matches!(
            parse_arguments(Vec::<String>::new()),
            Ok(Command::Realtime(RealtimeConfig {
                reflex_mode: crate::realtime::ReflexMode::On,
                ..
            }))
        ));
        for (value, expected) in [
            ("off", crate::realtime::ReflexMode::Off),
            ("on", crate::realtime::ReflexMode::On),
            ("on-boost", crate::realtime::ReflexMode::OnBoost),
        ] {
            let command =
                parse_arguments(["--reflex-mode".to_string(), value.to_string()]).unwrap();
            assert!(matches!(
                command,
                Command::Realtime(RealtimeConfig { reflex_mode, .. }) if reflex_mode == expected
            ));
        }
        assert!(parse_reflex_mode("invalid").is_err());
        assert!(parse_arguments(["--reflex-mode".to_string()]).is_err());
    }

    #[test]
    fn upscaler_owns_the_dlss_internal_extent() {
        assert!(
            parse_arguments([
                "--upscaler".to_string(),
                "dlss-quality".to_string(),
                "--render-scale".to_string(),
                "0.67".to_string(),
            ])
            .is_err()
        );
        assert!(
            parse_arguments([
                "--upscaler".to_string(),
                "dlss-quality".to_string(),
                "--dynamic-resolution".to_string(),
            ])
            .is_err()
        );
        assert!(
            parse_arguments([
                "--upscaler".to_string(),
                "native".to_string(),
                "--dynamic-resolution".to_string(),
            ])
            .is_ok()
        );
    }

    #[test]
    fn dlss_defaults_to_a_stable_project_identity_and_accepts_an_application_id_override() {
        assert!(parse_arguments(["--upscaler".to_string(), "dlss-quality".to_string()]).is_ok());
        let project_id = crate::realtime::STREAMLINE_PROJECT_ID;
        assert_eq!(project_id.len(), 36);
        assert!(project_id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        }));
        assert!(!crate::realtime::STREAMLINE_ENGINE_VERSION.is_empty());
        assert!(
            parse_arguments([
                "--upscaler".to_string(),
                "dlss-quality".to_string(),
                "--streamline-application-id".to_string(),
                "0".to_string(),
            ])
            .is_err()
        );
        assert!(matches!(
            parse_arguments([
                "--upscaler".to_string(),
                "dlss-quality".to_string(),
                "--streamline-application-id".to_string(),
                "12345".to_string(),
            ]),
            Ok(Command::Realtime(RealtimeConfig {
                streamline_application_id: Some(12345),
                ..
            }))
        ));
    }

    #[test]
    fn upscaler_is_covered_by_cpu_reference_conflict_guard() {
        let result = parse_arguments([
            "--cpu-reference".to_string(),
            "--upscaler".to_string(),
            "native".to_string(),
        ]);
        assert!(matches!(result, Err(message) if message.contains("不能与")));
    }
}
