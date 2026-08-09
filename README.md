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

构建脚本会从 Windows SDK 中自动查找 `dxc.exe`。当前 DXR、Temporal、À-Trous 和 Tone Map Shader 均使用 Shader Model 6.6；运行 Debug 窗口时修改并保存任一当前 HLSL 文件，程序会重新编译完整兼容 Shader 集并在 GPU 安全点原子替换全部管线。编译或管线创建失败时保留上一版本，并在窗口标题和标准错误中报告原因。

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

## 阶段 6：GPU 时空降噪

阶段 6 已将渲染拆为独立的 DXR Path Trace、时域重投影、四轮 5×5 À-Trous 和 Tone Map Compute Pass。DXR Pass 只输出独立的 1 SPP 漫反射/镜面信号及第一交点 G-buffer，后续 Pass 使用各自的 SRV/UAV 表和显式资源屏障，不在同一次 Dispatch 中原地读取相邻 UAV。

当前时域管线使用世界位置投影生成像素运动矢量，并通过屏幕边界、实例/物体/材质 ID、世界法线和世界位置共同判断历史有效性。历史颜色在融合前裁剪到当前 3×3 邻域范围；漫反射先进行反照率解调，镜面信号使用独立历史长度、粗糙度和反射命中距离。

空间阶段使用步长 1、2、4、8 的四轮 5×5 B3 样条核，并通过 Ping-Pong 纹理隔离每轮读写。最终在线性 HDR 中重调制漫反射、合成镜面信号，再使用 ACES 近似 Tone Map 和显示 Gamma 输出到交换链。

按 `F1` 可依次查看最终结果、原始 1 SPP、反照率、法线/粗糙度、深度、运动矢量、方差、历史拒绝原因、历史长度、物体/材质 ID 和镜面命中距离。窗口标题分别显示 Path Trace、Temporal 和 À-Trous GPU 时间。

## 阶段 7：动态场景与 glTF

阶段 7 将默认 Cornell Box 和导入场景统一为 `SceneAsset` 路径，支持静态 glTF 2.0 网格、节点层级、多实例、PBR metallic-roughness 材质、RGBA8 图片以及刚体实例动画：

```powershell
cargo run --release
cargo run --release -- --model assets/gltf/Triangle/Triangle.gltf
cargo run --release -- --model assets/gltf/NonIndexedMultiNode/NonIndexedMultiNode.gltf --animate-model
```

`--model` 只接受本地 `.gltf`/`.glb` 文件；`--animate-model` 让导入实例按绝对时间绕 Y 轴旋转。每个唯一 mesh primitive 共享一个 BLAS，每个 node-primitive 生成一个 TLAS instance，动画帧只更新三帧 Frame Context 分片中的实例描述和 TLAS，不在每帧等待 GPU。

材质使用 glTF 的 base color、metallic-roughness、normal 和 emissive factor/texture。base color/emissive 使用 sRGB SRV，metallic-roughness/normal 使用线性 SRV，metallic-roughness 严格读取 G=roughness、B=metallic。缺失纹理使用预初始化 fallback；normal texture 但 primitive 缺少 tangent 时禁用 normal map并输出警告。

纹理 binding 保留 image、sampler 和 texCoord 语义。本阶段只使用 `TEXCOORD_0`：非零 `texCoord` 或有纹理但 primitive 缺少 UV0 会明确报错。sampler 支持 U/V 独立 Repeat、ClampToEdge、MirroredRepeat，以及 nearest/linear min/mag filter；sampler descriptor table 上限为 64，材质 texture/sampler index 在 CPU 侧做有界打包检查。`doubleSided=false` 的 glTF primitive 使用 DXR 背面剔除，`doubleSided=true` 和 legacy dielectric 允许双面命中，并使用 DXR HitKind 判断 front/back。

`RawDiffuse` 仅表示可按 base color 重调制的 diffuse 信号；`RawSpecular` 表示未调制的 specular + emissive 信号。first-bounce 的两个 lobe 使用同一个 mixture PDF 分别拆分，黑色 base color 不会抹掉自发光。

当前明确不支持并会报错：非 OPAQUE alpha、skin、morph target、animation channel、非 TRIANGLES primitive 和 `extensionsRequired`。压缩纹理、运行时网络下载、完整动画系统、阶段 8/9/10 优化也不在本阶段范围内。

仓库内的 `assets/gltf/TextureSampler` 是项目内生成的离线回归夹具；公开 Khronos `BoxTextured`/`DamagedHelmet` 资产不随仓库自动下载。

## 阶段 8：性能优化（8A–8E-2、8G）

8A 已建立可信 GPU 基线和显存遥测；8B 的 À-Trous shared tile 实验因 RTX 4060 Laptop 三档实测回退而未采纳；8C 的 barrier/bind 优化已设为默认并保留 baseline 回退；8D 已实现两阶段 AS 初始化、TLAS 策略、profitable BLAS compaction 和 allocation 遥测，但仓库小模型没有触发真实 compact copy，AS 默认仍为 baseline；8E-1 已完成输出/内部尺寸解耦和按 fence 退休的资源代际切换；8E-2 已实现由已完成 GPU Total timestamp 驱动的动态分辨率、旧 generation 样本隔离、双重冷却和 measurement/lifetime 遥测，并完成首轮 review 修复。8G 已补齐 typed DebugView、fence-safe PNG、image_diff、bounded 显存 measurement、正确 median、环境来源、五条 Debug raw 证据和有界 runner。自认证 commit paired 在同一 RTX 4060 Laptop/AC 环境下得到 candidate `0fdcb7f`/reference `2031abf` p95 median `7.91/7.84 ms`，差 `+0.893%`；同一 candidate 的 dynamic/fixed paired 为 `7.89/7.85 ms`，差 `+0.510%`，三次 dynamic 均保持原生 1920×1080、零切换。因此当前是绝对历史门槛 FAIL，但没有 commit regression 或 dynamic-mode regression，不应据此改 renderer。1800/600 秒长测、真实 F1/F2/resize/最小化/恢复/hot-reload、公开 PBR/大模型和 PIX UI 证据仍未执行，阶段 8 尚未完成。窗口标题显示阶段 8、命令记录模式、最近有效 GPU Total、滚动 p95、输出/内部尺寸、动态控制状态、generation、À-Trous 模式和 local VRAM usage/budget。

Release benchmark 在 120 个有效 GPU 帧预热后采样指定时长，stdout 最终输出恰好一行 JSON：

```powershell
target\release\ray_tracing_demo.exe --benchmark-seconds 30
target\release\ray_tracing_demo.exe --benchmark-seconds 30 --output-size 1600x900 --atrous-mode baseline
target\release\ray_tracing_demo.exe --benchmark-seconds 30 --output-size 1600x900 --atrous-mode shared
target\release\ray_tracing_demo.exe --benchmark-seconds 30 --output-size 1600x900 --command-recording-mode optimized
target\release\ray_tracing_demo.exe --benchmark-seconds 30 --output-size 1920x1080 --dynamic-resolution
```

JSON 包含实际 GPU 名称、输出/内部尺寸范围、分辨率模式、À-Trous 模式、完整测量区间的有效样本数、Total/AS/Path Trace/Temporal/À-Trous 聚合与四次迭代/ToneMap 的 p50/p95、历史/尺寸重置计数、PIX runtime 状态，以及显存 usage/budget。UI 的最近 240 帧滚动窗口不会截断 benchmark；正式测量开始时也会排除三帧 Frame Context 中尚未完成的预热 timestamp。`Present(1)` 的 FPS 不参与 GPU 统计。

项目随仓库固定包含 Microsoft 官方 `WinPixEventRuntime 1.0.240308001` 的 x64 DLL。`build.rs` 会将 DLL 和许可证复制到 Cargo profile 输出目录，程序只从可执行文件旁的绝对路径加载。它仅用于写入 PIX instrumentation；加载失败不影响渲染，但 JSON 中 `pix_events_available` 会为 `false`。来源、哈希和许可证见 [`third_party/winpix/README.md`](third_party/winpix/README.md)。8G 实测数据、raw JSON、capture/diff 和剩余债务见 [`docs/阶段8G总体验收记录.md`](docs/阶段8G总体验收记录.md)。8F 因 1080p Path Trace 占比约 43.7% 未达到 45% 入口，本轮不实施。

## 检查项目

```powershell
cargo fmt -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release
```
