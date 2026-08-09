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
    println!("cargo:rerun-if-changed=native/nrd_bridge/CMakeLists.txt");
    println!("cargo:rerun-if-changed=native/nrd_bridge/include/nrd_bridge.h");
    println!("cargo:rerun-if-changed=native/nrd_bridge/src/nrd_bridge.cpp");
    println!("cargo:rerun-if-changed=shaders/stage9_nrd_prep.hlsl");
    println!("cargo:rerun-if-changed=shaders/stage9_nrd_compose.hlsl");
    println!("cargo:rerun-if-env-changed=NRD_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=NRI_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=MATHLIB_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=SHADERMAKE_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=D3D12MA_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=VSDEVCMD_BAT");
    println!("cargo:rerun-if-env-changed=CMAKE");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        if env::var_os("CARGO_FEATURE_NRD").is_some() {
            panic!("NRD feature 仅支持 Windows D3D12 目标");
        }
        return;
    }

    let dxc = find_dxc().expect("没有找到 dxc.exe，请安装 Windows SDK");
    let output_directory = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for (source, output, target) in shaders {
        compile_shader(&dxc, source, &output_directory.join(output), target, None);
    }
    if env::var_os("CARGO_FEATURE_NRD").is_some() {
        let nrd_shader_directory = dependency_path(
            "NRD_SOURCE_DIR",
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("external/nrd-v4.17.3"),
        )
        .join("Shaders");
        if !nrd_shader_directory.is_dir() {
            panic!(
                "NRD shader include directory {} 不存在；请先运行 scripts/fetch_nrd.ps1",
                nrd_shader_directory.display()
            );
        }
        compile_shader(
            &dxc,
            "shaders/stage9_nrd_prep.hlsl",
            &output_directory.join("stage9_nrd_prep.dxil"),
            "cs_6_6",
            Some(&nrd_shader_directory),
        );
        compile_shader(
            &dxc,
            "shaders/stage9_nrd_compose.hlsl",
            &output_directory.join("stage9_nrd_compose.dxil"),
            "cs_6_6",
            Some(&nrd_shader_directory),
        );
    }
    deploy_winpix_runtime(&output_directory);
    println!("cargo:rustc-env=RAY_TRACING_DXC={}", dxc.display());
    println!("cargo:rustc-env=WINPIX_RUNTIME_VERSION=1.0.240308001");

    if env::var_os("CARGO_FEATURE_NRD").is_some() {
        build_nrd_bridge(&output_directory, &dxc);
    }
}

fn build_nrd_bridge(output_directory: &Path, dxc: &Path) {
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let nrd_source = dependency_path(
        "NRD_SOURCE_DIR",
        &repository_root.join("external/nrd-v4.17.3"),
    );
    let nri_source = dependency_path("NRI_SOURCE_DIR", &nrd_source.join("_deps/NRI"));
    let mathlib_source = dependency_path("MATHLIB_SOURCE_DIR", &nrd_source.join("_deps/MathLib"));
    let shadermake_source = dependency_path(
        "SHADERMAKE_SOURCE_DIR",
        &nrd_source.join("_deps/ShaderMake"),
    );
    let d3d12ma_source = dependency_path(
        "D3D12MA_SOURCE_DIR",
        &repository_root.join("external/d3d12ma-96e58ad6"),
    );
    for (name, path) in [
        ("NRD_SOURCE_DIR", &nrd_source),
        ("NRI_SOURCE_DIR", &nri_source),
        ("MATHLIB_SOURCE_DIR", &mathlib_source),
        ("SHADERMAKE_SOURCE_DIR", &shadermake_source),
        ("D3D12MA_SOURCE_DIR", &d3d12ma_source),
    ] {
        if !path.is_dir() {
            panic!(
                "{name}={} 不存在；请先运行 scripts/fetch_nrd.ps1 并准备固定的 D3D12MemoryAllocator 源码",
                path.display()
            );
        }
    }

    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let build_type = if profile == "release" {
        "Release"
    } else {
        "Debug"
    };
    let build_directory = output_directory.join("nrd-cmake-vs");
    let source_directory = repository_root.join("native/nrd_bridge");
    let cmake = env::var_os("CMAKE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cmake"));
    let vsdevcmd = find_vsdevcmd();

    let configure_args = [
        "-S".to_string(),
        source_directory.display().to_string(),
        "-B".to_string(),
        build_directory.display().to_string(),
        "-G".to_string(),
        "Visual Studio 17 2022".to_string(),
        "-A".to_string(),
        "x64".to_string(),
        format!("-DCMAKE_BUILD_TYPE={build_type}"),
        format!("-DNRD_SOURCE_DIR={}", nrd_source.display()),
        format!("-DNRI_SOURCE_DIR={}", nri_source.display()),
        format!("-DMATHLIB_SOURCE_DIR={}", mathlib_source.display()),
        format!("-DSHADERMAKE_SOURCE_DIR={}", shadermake_source.display()),
        format!("-DD3D12MA_SOURCE_DIR={}", d3d12ma_source.display()),
        format!("-DDXC_PATH={}", dxc.display()),
    ];
    run_cmake(&cmake, &vsdevcmd, &configure_args, "configure NRD bridge");

    let build_args = [
        "--build".to_string(),
        build_directory.display().to_string(),
        "--target".to_string(),
        "nrd_bridge".to_string(),
        "--config".to_string(),
        build_type.to_string(),
        "-j".to_string(),
        "4".to_string(),
    ];
    run_cmake(&cmake, &vsdevcmd, &build_args, "build NRD bridge");

    let library_directories = [
        build_directory.join("lib").join(build_type),
        build_directory.join("nrd").join(build_type),
        build_directory.join("_deps/nri-build").join(build_type),
        build_directory
            .join("_deps/shadermake-build")
            .join(build_type),
        build_directory.join("lib"),
        build_directory.join("nrd"),
        build_directory.join("_deps/nri-build"),
        build_directory.join("_deps/shadermake-build"),
    ];
    for directory in library_directories {
        println!("cargo:rustc-link-search=native={}", directory.display());
    }
    for library in [
        "nrd_bridge",
        "NRD",
        "NRI",
        "NRI_D3D12",
        "NRI_Validation",
        "NRI_Shared",
        "ShaderMakeBlob",
    ] {
        println!("cargo:rustc-link-lib=static={library}");
    }
    for library in ["d3d12", "dxgi", "dxguid", "uuid", "ole32"] {
        println!("cargo:rustc-link-lib={library}");
    }
}

fn dependency_path(name: &str, default: &Path) -> PathBuf {
    env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| default.to_path_buf())
}

fn find_vsdevcmd() -> PathBuf {
    if let Some(path) = env::var_os("VSDEVCMD_BAT").map(PathBuf::from)
        && path.is_file()
    {
        return path;
    }
    for path in [
        r"C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\Tools\VsDevCmd.bat",
        r"C:\Program Files\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat",
        r"C:\Program Files\Microsoft Visual Studio\2022\Professional\Common7\Tools\VsDevCmd.bat",
        r"C:\Program Files\Microsoft Visual Studio\2022\Enterprise\Common7\Tools\VsDevCmd.bat",
    ] {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    panic!("未找到 VsDevCmd.bat；请设置 VSDEVCMD_BAT 以便离线构建 NRD bridge");
}

fn run_cmake(cmake: &Path, vsdevcmd: &Path, arguments: &[String], action: &str) {
    let script_path = PathBuf::from(env::var_os("OUT_DIR").unwrap())
        .join(format!("nrd-{}.cmd", action.replace(' ', "-")));
    let arguments = arguments
        .iter()
        .map(|argument| quote_cmd_arg(Path::new(argument)))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        "@echo off\r\ncall {} -arch=x64\r\nif errorlevel 1 exit /b %errorlevel%\r\n{} {}\r\n",
        quote_cmd_arg(vsdevcmd),
        quote_cmd_arg(cmake),
        arguments
    );
    fs::write(&script_path, script)
        .unwrap_or_else(|error| panic!("写入 {action} 脚本失败: {error}"));
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/s", "/c", "call"])
        .arg(&script_path)
        .output()
        .unwrap_or_else(|error| panic!("启动 {action} 失败: {error}"));
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        panic!("{action} 失败，退出码 {}", output.status);
    }
}

fn quote_cmd_arg(path: &Path) -> String {
    let value = path.to_string_lossy();
    if value.contains(' ') || value.contains('&') || value.contains('(') || value.contains(')') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.into_owned()
    }
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

fn compile_shader(
    dxc: &Path,
    source: &str,
    output: &Path,
    target: &str,
    extra_include: Option<&Path>,
) {
    let mut command = Command::new(dxc);
    command.args([source, "-I", "shaders"]);
    if let Some(extra_include) = extra_include {
        command.args(["-I"]).arg(extra_include);
    }
    command.args(["-T", target, "-HV", "2021", "-Fo"]);
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
