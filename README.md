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

## 阶段 2：Compute Shader 基础设施

默认窗口现在由 Compute Shader 写入动态渐变纹理，再复制到交换链显示。窗口标题每半秒更新一次 FPS、GPU Pass 耗时和 Shader 状态：

```text
RayTracingDemo - 阶段 2 | FPS 240 | GPU 0.422 ms | Shader 内嵌 DXIL
```

构建脚本会从 Windows SDK 中自动查找 `dxc.exe`，并将 [`shaders/stage2_gradient.hlsl`](shaders/stage2_gradient.hlsl) 编译为 DXIL。运行 Debug 窗口时修改并保存该 HLSL 文件，程序会自动重新编译和替换 Compute Pipeline；编译失败时保留上一版本，并在标准错误中输出完整的 DXC 信息。

阶段 2 同时加入：

- 统一的资源状态跟踪和传统 Resource Barrier。
- RTV 与 Shader 可见描述符堆包装。
- 持久映射、按三帧上下文分区的 Upload Ring。
- GPU Timestamp Query 和 Readback Buffer。
- `output/shader-cache` 下的开发期 Shader 编译缓存。

## 阶段 3：第一个硬件光追三角形

默认实时窗口现在使用真正的 DXR 管线，而不是 Compute Shader 模拟光线追踪。启动时会依次完成：

- 检测 `D3D12_OPTIONS5` 并在标题显示 DXR Tier；
- 上传单三角形顶点与索引；
- 构建三角形 BLAS 和单实例 TLAS；
- 创建包含 `RayGen`、`Miss`、`ClosestHit` 的 DXR State Object；
- 创建 Shader Table 并逐帧调用 `DispatchRays`；
- 将命中法线颜色或深蓝背景复制到交换链。

阶段 3 Shader 位于 [`shaders/stage3_triangle.hlsl`](shaders/stage3_triangle.hlsl)，由构建脚本以 `lib_6_3` 目标编译。运行后应看到蓝色法线三角形和深蓝背景，窗口标题显示 `阶段 3`、FPS、GPU 时间与实际 DXR Tier。

## 阶段 4：Cornell Box GPU 路径追踪

实时窗口已切换为 Cornell Box 1 SPP 路径追踪，包含白色、红色、绿色漫反射墙面、面积光源、金属盒和玻璃盒。Shader 最多递归四层，并使用面积光源 NEE、独立阴影射线和幂启发式 MIS 降低直接光噪声。

相机控制：

- `W`、`A`、`S`、`D`：水平移动；
- `Space`、左 `Shift`：升高和降低；
- 方向键：旋转视角。

每帧使用不同随机种子。RayGen 同时输出第一交点反照率、世界法线和线性距离 G-buffer，供后续渐进积累、历史重投影和降噪使用。阶段 4 尚不保留历史帧，因此当前画面会呈现实时 1 SPP 噪声；阶段 5 将加入渐进积累。

## 阶段 5：渐进积累

阶段 5 新增全分辨率 `R32G32B32A32_FLOAT` 历史纹理。静止相机时，每帧使用新的随机种子并按 `1/N` 累计，标题中的 `SPP` 显示当前累计样本数；按键移动或旋转相机会把样本计数归零，下一帧自动从新视角开始累计。窗口进程启用 Per-Monitor DPI Awareness V2，200% 缩放下截图和窗口物理尺寸保持一致。

## 阶段 6：时空降噪基础资源

阶段 6 首先建立 GPU 时空降噪所需的历史资源：`R16G16_FLOAT` 历史亮度矩、`R16G16_FLOAT` 相机运动矢量，以及上一帧深度和法线。RayGen 在渐进积累后执行 3×3 邻域颜色范围裁剪，并依据深度差与法线夹角判断历史是否有效；每帧结束后复制当前 G-buffer 作为下一帧历史，为后续的重投影、方差估计和 À-Trous 空间滤波提供输入。相机移动或旋转时会清零样本计数，避免继续使用旧视角的统计结果。

本子阶段增加独立的上一帧累计纹理，并根据相机位移和旋转估算屏幕历史采样偏移，避免在同一 UAV 上同时读写累计结果。

当前空间滤波采用两级步长的 À-Trous 风格 3×3 邻域采样，并结合深度、法线、亮度方差计算双边权重；相机刚移动或历史无效时自动跳过空间融合。

鼠标左键或右键按下会主动清空时空历史，避免窗口焦点切换后出现横向拖影；窗口标题现显示阶段 6。

## 检查项目

```powershell
cargo fmt -- --check
cargo check
cargo test
```
