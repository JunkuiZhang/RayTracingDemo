# RayTracingDemo

这是一个使用 Rust 编写的光线追踪项目。项目最初是 CPU 单帧路径追踪 Demo，目前正在按照 [实时 DXR 渲染器实施方案](docs/实时DXR渲染器实施方案.md) 逐步升级为 Windows 上的实时 DX12/DXR 渲染器。

## 环境要求

- Windows 10 或 Windows 11。
- 最新稳定版 Rust，使用 MSVC 工具链。
- Visual Studio Build Tools，并安装“使用 C++ 的桌面开发”和 Windows SDK。

## 阶段 0：CPU 参考渲染

阶段 0 冻结了原有 CPU 路径追踪器，用于验证后续 GPU 渲染的相机、材质、光照和最终颜色。渲染使用固定 Cornell Box、固定相机和固定随机种子，因此相同参数会得到确定一致的图片。

运行默认的 1 SPP CPU 参考渲染和降噪：

```powershell
cargo run --release -- --cpu-reference
```

生成指定采样数的原始参考图：

```powershell
cargo run --release -- --cpu-reference --samples 16 --skip-denoise
```

完整选项：

```text
--samples <数量>        每像素采样数，默认 1
--seed <整数或十六进制> 固定随机种子
--output-dir <目录>     输出目录，默认 output/cpu-reference
--skip-denoise          跳过 CPU 降噪，只保存原始路径追踪结果
```

阶段 0 固定生成的 1、16、500 SPP 图片及项目早期实验图片统一保存在 [`assets/reference`](assets/reference/README.md)。

## 阶段 1：DX12 实时窗口

Windows 上不带参数运行时，程序会启动可调整大小的 DX12 窗口，并持续显示深蓝色清屏结果：

```powershell
cargo run
```

Debug 构建会尝试启用 D3D12 Debug Layer、GPU-Based Validation、Info Queue 和 DRED。若系统没有安装 Windows“图形工具”可选功能，DXGI 调试 Factory 会自动回退，不影响普通窗口运行。

性能运行使用 Release 构建：

```powershell
cargo run --release
```

阶段 1 已实现三缓冲交换链、每帧独立 Command Allocator、Fence 同步、垂直同步 Present，以及窗口缩放、最小化和恢复。CPU 参考模式仍通过 `--cpu-reference` 启动。

如需用 PIX 捕获，在 PIX 中选择 Launch Win32，目标程序填写 `target\debug\ray_tracing_demo.exe`，启动后捕获任意一帧。PIX 和 Windows“图形工具”需要另行安装。

## 检查项目

```powershell
cargo fmt -- --check
cargo check
cargo test
```
