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
            "shaders/stage8_atrous_shared.hlsl",
            "stage8_atrous_shared.dxil",
            "cs_6_6",
        ),
        (
            "shaders/stage6_tonemap.hlsl",
            "stage6_tonemap.dxil",
            "cs_6_6",
        ),
    ];
    for (source, _, _) in shaders {
        println!("cargo:rerun-if-changed={source}");
    }
    // Shared .hlsli files are dependencies of every DXIL entry point. Watching
    // the directory keeps build-time shader recompilation in sync with the
    // debug runtime hot-reload path.
    println!("cargo:rerun-if-changed=shaders");
    println!("cargo:rerun-if-changed=third_party/winpix/x64/WinPixEventRuntime.dll");
    println!("cargo:rerun-if-changed=third_party/winpix/LICENSE.txt");
    println!("cargo:rerun-if-changed=third_party/winpix/ThirdPartyNotices.txt");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let dxc = find_dxc().expect("没有找到 dxc.exe，请安装 Windows SDK");
    let output_directory = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for (source, output, target) in shaders {
        compile_shader(&dxc, source, &output_directory.join(output), target);
    }
    deploy_winpix_runtime(&output_directory);
    println!("cargo:rustc-env=RAY_TRACING_DXC={}", dxc.display());
    println!("cargo:rustc-env=WINPIX_RUNTIME_VERSION=1.0.240308001");
}

fn deploy_winpix_runtime(output_directory: &Path) {
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64") {
        println!(
            "cargo:warning=WinPixEventRuntime 仅随仓库提供 x64 版本，当前目标不会部署 PIX runtime"
        );
        return;
    }

    // OUT_DIR is target[/triple]/<profile>/build/<package-hash>/out. Cargo
    // launches the executable from the profile directory, so keep the runtime
    // beside the executable as required by WinPixEventRuntime.
    let profile_directory = output_directory
        .ancestors()
        .nth(3)
        .expect("无法从 OUT_DIR 定位 Cargo profile 输出目录");
    for (source, destination) in [
        (
            Path::new("third_party/winpix/x64/WinPixEventRuntime.dll"),
            "WinPixEventRuntime.dll",
        ),
        (
            Path::new("third_party/winpix/LICENSE.txt"),
            "WinPixEventRuntime.LICENSE.txt",
        ),
        (
            Path::new("third_party/winpix/ThirdPartyNotices.txt"),
            "WinPixEventRuntime.ThirdPartyNotices.txt",
        ),
    ] {
        let destination = profile_directory.join(destination);
        copy_if_changed(source, &destination);
    }
}

fn copy_if_changed(source: &Path, destination: &Path) {
    let source_bytes =
        fs::read(source).unwrap_or_else(|error| panic!("读取 {}：{error}", source.display()));
    if let Ok(destination_bytes) = fs::read(destination)
        && destination_bytes == source_bytes
    {
        return;
    }
    fs::write(destination, source_bytes)
        .unwrap_or_else(|error| panic!("写入 {}：{error}", destination.display()));
}

fn compile_shader(dxc: &Path, source: &str, output: &Path, target: &str) {
    let mut command = Command::new(dxc);
    command.args([source, "-I", "shaders", "-T", target, "-HV", "2021", "-Fo"]);
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
