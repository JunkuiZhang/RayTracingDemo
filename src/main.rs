use std::{env, path::PathBuf, process::ExitCode};

use cpu_reference::CpuReferenceConfig;

mod camera;
mod cpu_reference;
mod data;
mod entity;
mod material;
mod realtime;
mod renderer;
mod scene;
mod settings;
mod some_math;
mod systems;
mod world;

fn main() -> ExitCode {
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
    let realtime_requested = arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--model" | "--animate-model"));
    if cpu_reference_requested && realtime_requested {
        return Err("--cpu-reference 不能与 --model 或 --animate-model 同时使用".to_string());
    }

    if !cpu_reference_requested {
        let mut config = realtime::RealtimeConfig::default();
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
                _ => return Err(format!("未知参数：{argument}")),
            }
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

fn print_help() {
    println!(
        "RayTracingDemo\n\n\
         用法：\n  \
         cargo run --release                 启动实时 DX12 窗口\n  \
         cargo run --release -- --model <路径> [--animate-model]\n  \
         cargo run --release -- --cpu-reference [选项]\n\n\
         选项：\n  \
         --samples <数量>       每像素采样数，默认 1\n  \
         --seed <整数或十六进制> 固定随机种子\n  \
         --output-dir <目录>    输出目录，默认 output/cpu-reference\n  \
         --skip-denoise         只保存原始路径追踪结果\n  \
         --help, -h             显示帮助"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Ok(Command::Realtime(_))
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
}
