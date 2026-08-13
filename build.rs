use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};

fn main() {
    emit_build_provenance();
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
        (
            "shaders/stage11_stable_plane_build.hlsl",
            "stage11_stable_plane_build.dxil",
            "cs_6_6",
        ),
        (
            "shaders/stage10_dlss_input.hlsl",
            "stage10_dlss_input.dxil",
            "cs_6_6",
        ),
        (
            "shaders/stage11_rr_input.hlsl",
            "stage11_rr_input.dxil",
            "cs_6_6",
        ),
        (
            "shaders/stage11_rr_stable_input.hlsl",
            "stage11_rr_stable_input.dxil",
            "cs_6_6",
        ),
        (
            "shaders/stage11_rr_emissive.hlsl",
            "stage11_rr_emissive.dxil",
            "cs_6_6",
        ),
        (
            "shaders/stage11_rr_primary_visibility.hlsl",
            "stage11_rr_primary_visibility.dxil",
            "cs_6_6",
        ),
        (
            "shaders/stage11_rr_boundary_resolve.hlsl",
            "stage11_rr_boundary_resolve.dxil",
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
    println!("cargo:rerun-if-changed=shaders/stage11_nrd_stable_prep.hlsl");
    println!("cargo:rerun-if-changed=shaders/stage11_nrd_stable_compose.hlsl");
    println!("cargo:rerun-if-env-changed=NRD_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=NRI_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=MATHLIB_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=SHADERMAKE_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=D3D12MA_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=VSDEVCMD_BAT");
    println!("cargo:rerun-if-env-changed=CMAKE");
    println!("cargo:rerun-if-env-changed=STREAMLINE_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_STREAMLINE_RR");
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
        println!(
            "cargo:rustc-env=RAY_TRACING_NRD_SHADER_DIR={}",
            nrd_shader_directory.display()
        );
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
        compile_shader(
            &dxc,
            "shaders/stage11_nrd_stable_prep.hlsl",
            &output_directory.join("stage11_nrd_stable_prep.dxil"),
            "cs_6_6",
            Some(&nrd_shader_directory),
        );
        compile_shader(
            &dxc,
            "shaders/stage11_nrd_stable_compose.hlsl",
            &output_directory.join("stage11_nrd_stable_compose.dxil"),
            "cs_6_6",
            Some(&nrd_shader_directory),
        );
    }
    deploy_winpix_runtime(&output_directory);
    if env::var_os("CARGO_FEATURE_STREAMLINE").is_some() {
        validate_streamline_sdk(
            &output_directory,
            env::var_os("CARGO_FEATURE_STREAMLINE_RR").is_some(),
        );
        build_streamline_bridge(&output_directory);
    }
    println!("cargo:rustc-env=RAY_TRACING_DXC={}", dxc.display());
    println!("cargo:rustc-env=WINPIX_RUNTIME_VERSION=1.0.240308001");

    if env::var_os("CARGO_FEATURE_NRD").is_some() {
        build_nrd_bridge(&output_directory, &dxc);
    }
}

fn emit_build_provenance() {
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let git_value = |arguments: &[&str]| {
        Command::new("git")
            .args(arguments)
            .current_dir(repository_root)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let head = git_value(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unavailable".into());
    let tree = git_value(&["rev-parse", "HEAD^{tree}"]).unwrap_or_else(|| "unavailable".into());
    let dirty = Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .current_dir(repository_root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_none_or(|output| !output.stdout.is_empty());
    let mut features = env::vars()
        .filter_map(|(name, _)| {
            name.strip_prefix("CARGO_FEATURE_")
                .map(|feature| feature.to_ascii_lowercase().replace('_', "-"))
        })
        .filter(|feature| feature != "default")
        .collect::<Vec<_>>();
    features.sort_unstable();

    // Formal acceptance compares these values with the current clean Git tree.
    // Recording them inside the executable prevents a stale binary from being
    // attributed to whichever checkout happens to launch the runner later.
    println!("cargo:rustc-env=RAY_TRACING_BUILD_GIT_HEAD={head}");
    println!("cargo:rustc-env=RAY_TRACING_BUILD_GIT_TREE={tree}");
    println!("cargo:rustc-env=RAY_TRACING_BUILD_GIT_DIRTY={dirty}");
    println!(
        "cargo:rustc-env=RAY_TRACING_BUILD_FEATURES={}",
        features.join(",")
    );
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Some(symbolic_ref) = git_value(&["symbolic-ref", "--quiet", "HEAD"]) {
        println!("cargo:rerun-if-changed=.git/{symbolic_ref}");
    }
}

fn validate_streamline_sdk(output_directory: &Path, rr_enabled: bool) {
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sdk = dependency_path(
        "STREAMLINE_SOURCE_DIR",
        &repository_root.join("external/streamline-v2.12.0"),
    );
    validate_streamline_lock(repository_root, &sdk, rr_enabled);
    let required = [
        "include/sl.h",
        "include/sl_consts.h",
        "include/sl_core_api.h",
        "include/sl_core_types.h",
        "include/sl_dlss.h",
        "include/sl_reflex.h",
        "include/sl_pcl.h",
        "include/sl_hooks.h",
        "lib/x64/sl.interposer.lib",
    ];
    for relative in required {
        if !sdk.join(relative).is_file() {
            panic!(
                "Streamline SDK 文件缺失：{}；请先运行 scripts/fetch_streamline.ps1",
                sdk.join(relative).display()
            );
        }
    }
    let flavor = if env::var("PROFILE").as_deref() == Ok("release") {
        ""
    } else {
        "development/"
    };
    let mut runtime_files = vec![
        "sl.interposer.dll",
        "sl.common.dll",
        "sl.dlss.dll",
        "sl.reflex.dll",
        "sl.pcl.dll",
        "nvngx_dlss.dll",
    ];
    if rr_enabled {
        runtime_files.extend(["sl.dlss_d.dll", "nvngx_dlssd.dll"]);
    }
    for name in runtime_files.iter() {
        let relative = format!("bin/x64/{flavor}{name}");
        if !sdk.join(&relative).is_file() {
            panic!(
                "Streamline {} DLL 缺失：{}；请先运行 scripts/fetch_streamline.ps1",
                if flavor.is_empty() {
                    "production"
                } else {
                    "development"
                },
                sdk.join(&relative).display()
            );
        }
    }
    let profile_directory = output_directory
        .ancestors()
        .nth(3)
        .expect("无法从 OUT_DIR 定位 Cargo profile 输出目录");
    for name in runtime_files {
        copy_if_changed(
            &sdk.join(format!("bin/x64/{flavor}{name}")),
            &profile_directory.join(name),
        );
    }
    for (source, destination) in [
        ("license.txt", "Streamline.LICENSE.txt"),
        ("3rd-party-licenses.md", "Streamline.ThirdPartyLicenses.md"),
        (
            "bin/x64/reflex.license.txt",
            "Streamline.Reflex.LICENSE.txt",
        ),
        (
            "bin/x64/nvngx_dlss.license.txt",
            "Streamline.NvngxDlss.LICENSE.txt",
        ),
    ] {
        copy_if_changed(&sdk.join(source), &profile_directory.join(destination));
    }
    println!(
        "cargo:rustc-env=RAY_TRACING_STREAMLINE_SOURCE_DIR={}",
        sdk.display()
    );
}

fn validate_streamline_lock(repository_root: &Path, sdk: &Path, rr_enabled: bool) {
    let lock_path = repository_root.join("third_party/streamline/version.lock.json");
    println!("cargo:rerun-if-changed={}", lock_path.display());
    let lock: serde_json::Value = serde_json::from_slice(
        &fs::read(&lock_path)
            .unwrap_or_else(|error| panic!("读取 {} 失败：{error}", lock_path.display())),
    )
    .unwrap_or_else(|error| panic!("解析 {} 失败：{error}", lock_path.display()));
    if lock.get("version").and_then(serde_json::Value::as_str) != Some("2.12.0") {
        panic!("Streamline lock version 必须固定为 2.12.0");
    }
    for group in ["files", "licenses"] {
        let entries = lock
            .get(group)
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("Streamline lock 缺少 {group} 数组"));
        for entry in entries {
            let optional_feature = entry
                .get("optional_feature")
                .and_then(serde_json::Value::as_str);
            if optional_feature == Some("streamline-rr") && !rr_enabled {
                continue;
            }
            let relative = entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| panic!("Streamline lock {group} 条目缺少 path"));
            let relative_path = Path::new(relative);
            if relative_path.is_absolute()
                || relative_path
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                panic!("Streamline lock 包含不安全路径：{relative}");
            }
            let expected = entry
                .get("sha256")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| panic!("Streamline lock {relative} 缺少 sha256"));
            let path = sdk.join(relative_path);
            println!("cargo:rerun-if-changed={}", path.display());
            let actual = file_sha256(&path);
            if !actual.eq_ignore_ascii_case(expected) {
                panic!(
                    "Streamline SDK hash 不匹配：{}；expected={} actual={}；请重新运行 scripts/fetch_streamline.ps1",
                    path.display(),
                    expected,
                    actual
                );
            }
        }
    }
}

fn file_sha256(path: &Path) -> String {
    let mut file = fs::File::open(path)
        .unwrap_or_else(|error| panic!("读取 Streamline 文件 {} 失败：{error}", path.display()));
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).unwrap_or_else(|error| {
            panic!("哈希 Streamline 文件 {} 失败：{error}", path.display())
        });
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    format!("{:x}", hasher.finalize())
}

fn build_streamline_bridge(output_directory: &Path) {
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sdk = dependency_path(
        "STREAMLINE_SOURCE_DIR",
        &repository_root.join("external/streamline-v2.12.0"),
    );
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let build_type = if profile == "release" {
        "Release"
    } else {
        "Debug"
    };
    let build_directory = output_directory.join("streamline-cmake-vs");
    let source_directory = repository_root.join("native/streamline_bridge");
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
        format!("-DSTREAMLINE_SOURCE_DIR={}", sdk.display()),
        format!("-DSTREAMLINE_LIB_DIR={}", sdk.join("lib/x64").display()),
        format!(
            "-DSTREAMLINE_ENABLE_RR={}",
            if env::var_os("CARGO_FEATURE_STREAMLINE_RR").is_some() {
                "ON"
            } else {
                "OFF"
            }
        ),
    ];
    run_cmake(
        &cmake,
        &vsdevcmd,
        &configure_args,
        "configure Streamline bridge",
    );
    let build_args = [
        "--build".to_string(),
        build_directory.display().to_string(),
        "--target".to_string(),
        "streamline_bridge".to_string(),
        "--config".to_string(),
        build_type.to_string(),
        "-j".to_string(),
        "4".to_string(),
    ];
    run_cmake(&cmake, &vsdevcmd, &build_args, "build Streamline bridge");
    for directory in [
        build_directory.join("lib"),
        build_directory.join("lib").join(build_type),
        sdk.join("lib/x64"),
    ] {
        println!("cargo:rustc-link-search=native={}", directory.display());
    }
    println!("cargo:rustc-link-lib=static=streamline_bridge");
    println!("cargo:rustc-link-lib=static=sl.interposer");
    for library in ["d3d12", "dxgi", "dxguid", "ole32"] {
        println!("cargo:rustc-link-lib={library}");
    }
    println!("cargo:rerun-if-changed=native/streamline_bridge/CMakeLists.txt");
    println!("cargo:rerun-if-changed=native/streamline_bridge/include/streamline_bridge.h");
    println!("cargo:rerun-if-changed=native/streamline_bridge/src/streamline_bridge.cpp");
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
    deploy_nrd_notices(
        output_directory,
        &nrd_source,
        &nri_source,
        &mathlib_source,
        &shadermake_source,
        &d3d12ma_source,
    );

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

fn deploy_nrd_notices(
    output_directory: &Path,
    nrd_source: &Path,
    nri_source: &Path,
    mathlib_source: &Path,
    shadermake_source: &Path,
    d3d12ma_source: &Path,
) {
    let profile_directory = output_directory
        .ancestors()
        .nth(3)
        .expect("无法从 OUT_DIR 定位 Cargo profile 输出目录");
    for (source, destination) in [
        (nrd_source.join("LICENSE.txt"), "NVIDIA-NRD.LICENSE.txt"),
        (nri_source.join("LICENSE.txt"), "NVIDIA-NRI.LICENSE.txt"),
        (
            mathlib_source.join("LICENSE.txt"),
            "NVIDIA-MathLib.LICENSE.txt",
        ),
        (
            shadermake_source.join("LICENSE.txt"),
            "NVIDIA-ShaderMake.LICENSE.txt",
        ),
        (
            shadermake_source.join("ThirdPartyLicenses.txt"),
            "NVIDIA-ShaderMake.ThirdPartyLicenses.txt",
        ),
        (
            d3d12ma_source.join("LICENSE.txt"),
            "D3D12MemoryAllocator.LICENSE.txt",
        ),
        (
            d3d12ma_source.join("NOTICES.txt"),
            "D3D12MemoryAllocator.NOTICES.txt",
        ),
    ] {
        println!("cargo:rerun-if-changed={}", source.display());
        copy_if_changed(&source, &profile_directory.join(destination));
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
