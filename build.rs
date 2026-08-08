use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    let shaders = [
        (
            "shaders/stage3_triangle.hlsl",
            "stage3_triangle.dxil",
            "lib_6_6",
        ),
        (
            "shaders/stage6_temporal.hlsl",
            "stage6_temporal.dxil",
            "cs_6_6",
        ),
        ("shaders/stage6_atrous.hlsl", "stage6_atrous.dxil", "cs_6_6"),
        (
            "shaders/stage6_tonemap.hlsl",
            "stage6_tonemap.dxil",
            "cs_6_6",
        ),
    ];
    for (source, _, _) in shaders {
        println!("cargo:rerun-if-changed={source}");
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let dxc = find_dxc().expect("没有找到 dxc.exe，请安装 Windows SDK");
    let output_directory = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for (source, output, target) in shaders {
        compile_shader(&dxc, source, &output_directory.join(output), target);
    }
    println!("cargo:rustc-env=RAY_TRACING_DXC={}", dxc.display());
}

fn compile_shader(dxc: &Path, source: &str, output: &Path, target: &str) {
    let mut command = Command::new(dxc);
    command.args([source, "-T", target, "-HV", "2021", "-Fo"]);
    command.arg(output);
    if env::var("PROFILE").as_deref() == Ok("release") {
        command.arg("-O3");
    } else {
        command.args(["-Od", "-Zi", "-Qembed_debug"]);
    }
    let result = command.output().expect("无法启动 dxc.exe");
    if !result.status.success() {
        panic!(
            "HLSL 编译失败：{source} ({target})\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

fn find_dxc() -> Option<PathBuf> {
    if let Some(path) = env::var_os("DXC_PATH").map(PathBuf::from)
        && path.is_file()
    {
        return Some(path);
    }

    let kits_root = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut candidates = fs::read_dir(kits_root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("x64").join("dxc.exe"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.pop()
}
