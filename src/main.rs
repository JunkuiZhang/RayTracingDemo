use std::{env, path::PathBuf, process::ExitCode};

use cpu_reference::CpuReferenceConfig;

mod camera;
mod cpu_reference;
mod data;
mod entity;
mod material;
mod realtime;
mod renderer;
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
        Ok(Command::Realtime) => match realtime::run() {
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
    Realtime,
    CpuReference(CpuReferenceConfig),
    Help,
}

fn parse_arguments(arguments: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Ok(Command::Realtime);
    };
    if command == "--help" || command == "-h" {
        return Ok(Command::Help);
    }
    if command != "--cpu-reference" {
        return Err(format!("未知命令：{command}"));
    }

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
         cargo run --release -- --cpu-reference [选项]\n\n\
         选项：\n  \
         --samples <数量>       每像素采样数，默认 1\n  \
         --seed <整数或十六进制> 固定随机种子\n  \
         --output-dir <目录>    输出目录，默认 output/cpu-reference\n  \
         --skip-denoise         只保存原始路径追踪结果\n  \
         --help, -h             显示帮助"
    );
}
