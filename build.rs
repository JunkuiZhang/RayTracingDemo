use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=shaders/stage2_gradient.hlsl");
    println!("cargo:rerun-if-changed=shaders/stage3_triangle.hlsl");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let dxc = find_dxc().expect("没有找到 dxc.exe，请安装 Windows SDK");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("stage2_gradient.dxil");
    let mut command = Command::new(&dxc);
    command.args([
        "shaders/stage2_gradient.hlsl",
        "-E",
        "main",
        "-T",
        "cs_6_0",
        "-Fo",
    ]);
    command.arg(&output);
    if env::var("PROFILE").as_deref() == Ok("release") {
        command.arg("-O3");
    } else {
        command.args(["-Od", "-Zi", "-Qembed_debug"]);
    }

    let result = command.output().expect("无法启动 dxc.exe");
    if !result.status.success() {
        panic!(
            "HLSL 编译失败：\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    println!("cargo:rustc-env=RAY_TRACING_DXC={}", dxc.display());

    // 阶段 3 先把光追库编译为 DXIL，后续状态对象会复用这些导出函数。
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("stage3_triangle.dxil");
    let mut command = Command::new(&dxc);
    command.args(["shaders/stage3_triangle.hlsl", "-T", "lib_6_3", "-Fo"]);
    command.arg(&output);
    if env::var("PROFILE").as_deref() == Ok("release") {
        command.arg("-O3");
    } else {
        command.args(["-Od", "-Zi", "-Qembed_debug"]);
    }
    let result = command.output().expect("无法启动 dxc.exe");
    if !result.status.success() {
        panic!(
            "DXR Shader 编译失败：\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

fn find_dxc() -> Option<PathBuf> {
    if let Some(path) = env::var_os("DXC_PATH").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
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
