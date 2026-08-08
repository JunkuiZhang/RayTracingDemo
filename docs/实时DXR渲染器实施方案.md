# RayTracingDemo 实时 DXR 渲染器实施方案

## 1. 文档目的

本文档描述如何将当前的 Rust CPU 单帧路径追踪 Demo，逐步升级为运行在 Windows 和 RTX 4060 Laptop GPU 上的实时交互式 DX12/DXR 渲染器。

最终产品不是“在窗口里重复运行现有 CPU 渲染”，而是完整的新 GPU 渲染后端：使用 RTX 光追单元执行求交，使用时空滤波稳定每帧低采样结果，并预留 NVIDIA NRD、DLSS Ray Reconstruction、DLSS Super Resolution 和 Frame Generation 的接入位置。

当前 CPU 渲染器继续保留，作为以下工作的参考实现：

- 验证相机、几何求交、材质和光照公式。
- 生成高采样参考图。
- 对比 GPU 路径追踪器的能量与颜色是否正确。
- 对比自研降噪、NRD 和 DLSS Ray Reconstruction 的质量。

---

## 2. 最终目标

### 2.1 用户可见效果

程序启动后打开一个可调整大小的 Windows 窗口，并具备以下能力：

- 使用鼠标控制视角，使用 WASD 移动相机。
- 实时显示 Cornell Box 和后续导入的三角网格场景。
- 支持窗口化、无边框全屏、分辨率切换和垂直同步。
- 支持漫反射、金属、玻璃和面积光源。
- 相机静止时逐帧收敛；相机运动时保持稳定，不出现大面积拖影。
- 可以实时切换原始 1 SPP、时域结果、空间滤波结果和最终输出。
- 可以显示帧率、GPU 各阶段耗时、累计样本数、光线数量和显存占用。
- 可以截图，并与 CPU 高采样参考图进行对比。
- 在支持且已安装相应 SDK 时，可以切换 DLSS、NRD 或 DLSS Ray Reconstruction。

### 2.2 首要性能目标

以 RTX 4060 Laptop GPU 为目标显卡，第一版采用以下保守指标：

| 项目 | 基线目标 | 进阶目标 |
| --- | --- | --- |
| 输出分辨率 | 1920×1080 | 2560×1440 |
| 内部光追分辨率 | 1280×720 或 1600×900 | 1920×1080 |
| 路径样本 | 每像素每帧 1 条 | 自适应 1–2 条 |
| 最大反弹 | 2–4 次 | 4–6 次 |
| 基线帧率 | 60 FPS | 视场景达到 90 FPS |
| 静止收敛 | 0.5–2 秒内明显稳定 | 小于 0.5 秒 |
| 动态画面 | 无大面积鬼影和闪烁 | 细小高光稳定 |

Frame Generation 不能替代基础帧率。接入插帧前，应保证不插帧时至少达到稳定的 45–60 FPS，并保持合理输入延迟。

### 2.3 质量目标

- 输出保持线性 HDR，最终阶段统一曝光和色调映射。
- 漫反射直接光和间接光符合能量守恒。
- 光源采样与 BSDF 采样继续使用 MIS，不能恢复为经验系数调亮。
- 法线、深度、反照率和运动矢量边界清晰。
- 静止画面应逐步接近 CPU 参考图，而不是收敛到被滤糊的结果。
- 动态画面允许少量噪声，但不能用长历史制造明显拖影。
- 玻璃和高光反射应与漫反射信号分开处理，避免共用同一组滤波参数。

---

## 3. 核心技术决策

### 3.1 图形 API

采用 DirectX 12 和 DXR 1.1。

选择理由：

- 项目目标平台已经确定为 Windows。
- RTX 4060 Laptop GPU 可以通过 DXR 使用硬件光追单元。
- PIX、Nsight Graphics、DRED 和 DirectX Debug Layer 适合定位 GPU 问题。
- NVIDIA Streamline、DLSS、NRD 和 Ray Reconstruction 在 DX12 上有成熟接入路径。
- HLSL、DXC 和 Shader Model 6.6 适合统一所有光追与计算着色器。

### 3.2 开发语言

应用主体继续使用 Rust。通过 `windows` crate 调用 DXGI、D3D12、DXR 和 Win32 API。

对于缺少稳定 Rust 绑定的 NVIDIA SDK，使用一个很薄的 C++ 桥接层，对 Rust 暴露稳定的 C ABI。桥接层只负责：

- Streamline 初始化、资源标记和功能调用。
- NRD 创建、调度描述获取和销毁。
- 必要时封装 NVIDIA SDK 的版本差异。

渲染器资源管理、帧图、场景、相机和 UI 仍由 Rust 控制，不能把主体逻辑迁入桥接层。

### 3.3 窗口和输入

采用 `winit` 创建窗口和处理输入。原生窗口句柄用于创建 DXGI Swap Chain。

### 3.4 Shader 工具链

- Shader 使用 HLSL。
- 使用 DXC 编译为 DXIL。
- 开发模式保留 Shader 调试信息并启用热重载。
- 发布模式开启优化并保存编译缓存。
- 所有 CPU/GPU 共享结构必须显式控制对齐，禁止直接假设 Rust 结构布局等于 HLSL 布局。

### 3.5 降噪策略

按以下优先级实施：

1. 自研时域积累和几何历史拒绝。
2. 自研方差估计与 GPU À-Trous，形成无专有 SDK 的完整基线。
3. 可选接入 NVIDIA NRD，替换自研漫反射/镜面降噪。
4. 可选接入 DLSS Ray Reconstruction，替换传统光追降噪和部分重建流程。

OIDN 不作为实时逐帧降噪器。它只用于离线截图、参考图或质量对照。

---

## 4. 总体架构

```text
┌─────────────────────────────────────────────────────────────┐
│                         应用层                              │
│  窗口 / 输入 / 相机 / UI / 配置 / 截图 / 性能统计          │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                       场景与资源层                          │
│  网格 / 材质 / 纹理 / 灯光 / 实例 / 前后帧变换 / 资源句柄   │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                       DX12 渲染后端                         │
│  Device / Queue / Fence / Swap Chain / Descriptor Heap     │
│  Upload Ring / Frame Context / Resource State / Profiler    │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                        DXR 子系统                           │
│  BLAS / TLAS / Root Signature / State Object / Shader Table│
│  RayGen / Miss / ClosestHit / AnyHit / TraceRays            │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                       帧图与后处理                          │
│  时域重投影 → 方差估计 → À-Trous/NRD/RR → 曝光 → DLSS SR   │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                    Tone Map / UI / Present                  │
└─────────────────────────────────────────────────────────────┘
```

---

## 5. 建议的代码结构

在项目稳定前不急于拆成大型 Cargo Workspace，先保持单仓库和清晰模块：

```text
RayTracingDemo/
├─ assets/
│  ├─ scenes/
│  ├─ textures/
│  └─ reference/
├─ docs/
├─ native/
│  ├─ streamline_bridge/
│  └─ nrd_bridge/
├─ shaders/
│  ├─ shared/
│  ├─ raytracing/
│  ├─ denoise/
│  ├─ postprocess/
│  └─ ui/
├─ src/
│  ├─ app/
│  │  ├─ input.rs
│  │  ├─ camera_controller.rs
│  │  └─ ui.rs
│  ├─ cpu_reference/
│  │  └─ 现有 CPU 渲染器
│  ├─ renderer/
│  │  ├─ d3d12/
│  │  │  ├─ device.rs
│  │  │  ├─ swap_chain.rs
│  │  │  ├─ command.rs
│  │  │  ├─ descriptor.rs
│  │  │  ├─ resource.rs
│  │  │  ├─ upload.rs
│  │  │  └─ profiler.rs
│  │  ├─ dxr/
│  │  │  ├─ acceleration_structure.rs
│  │  │  ├─ pipeline.rs
│  │  │  ├─ shader_table.rs
│  │  │  └─ dispatch.rs
│  │  ├─ passes/
│  │  │  ├─ path_trace.rs
│  │  │  ├─ temporal.rs
│  │  │  ├─ variance.rs
│  │  │  ├─ atrous.rs
│  │  │  ├─ tone_map.rs
│  │  │  └─ present.rs
│  │  ├─ integrations/
│  │  │  ├─ nrd.rs
│  │  │  └─ streamline.rs
│  │  └─ frame_graph.rs
│  ├─ scene/
│  │  ├─ mesh.rs
│  │  ├─ material.rs
│  │  ├─ light.rs
│  │  └─ loader.rs
│  └─ main.rs
├─ tools/
│  └─ shader_build/
├─ Cargo.toml
└─ build.rs
```

当原生桥接层和 Shader 构建流程稳定后，再考虑拆为 `renderer_core`、`app` 和 `cpu_reference` 三个 crate。

---

## 6. 每帧渲染流程

### 6.1 CPU 阶段

1. 处理窗口消息和输入。
2. 更新相机、实例变换和灯光。
3. 保存当前帧与上一帧的视图投影矩阵。
4. 更新动态实例缓冲。
5. 必要时更新动态 BLAS，并重建或更新 TLAS。
6. 构建本帧常量缓冲和帧图。
7. 提交 GPU 命令并推进 Fence。

### 6.2 GPU 阶段

```text
更新/构建 TLAS
    ↓
DXR Path Trace，输出低采样 HDR 信号和 G-buffer
    ↓
反照率解调，拆分漫反射与镜面信号
    ↓
运动矢量重投影
    ↓
深度/法线/物体 ID 历史有效性检查
    ↓
邻域裁剪与时域积累
    ↓
一阶/二阶矩更新和方差估计
    ↓
3–5 轮 À-Trous，或调用 NRD/Ray Reconstruction
    ↓
反照率重调制与信号合成
    ↓
自动曝光和 Tone Mapping
    ↓
DLSS Super Resolution，可选 Frame Generation
    ↓
UI 合成并 Present
```

### 6.3 推荐输出缓冲

| 缓冲 | 推荐格式 | 用途 |
| --- | --- | --- |
| 漫反射辐射亮度 | `R16G16B16A16_FLOAT` | 漫反射直接光与间接光 |
| 镜面辐射亮度 | `R16G16B16A16_FLOAT` | 金属、玻璃和高光反射 |
| 反照率 | `R8G8B8A8_UNORM` 或 `R16G16B16A16_FLOAT` | 解调和材质边界 |
| 法线/粗糙度 | `R16G16B16A16_FLOAT` | 历史拒绝和滤波 |
| 线性深度 | `R32_FLOAT` | 重投影和遮挡检测 |
| 运动矢量 | `R16G16_FLOAT` | 当前像素到上一帧的位置 |
| 物体/材质 ID | `R32_UINT` | 严格边界和调试 |
| 历史颜色 | `R16G16B16A16_FLOAT` | Ping-Pong 时域缓存 |
| 一阶/二阶矩 | `R16G16_FLOAT` | 亮度均值与方差 |
| 历史长度 | `R8_UINT` 或 `R16_UINT` | 控制积累权重 |
| 最终 HDR | `R16G16B16A16_FLOAT` | Tone Mapping 和 DLSS 输入 |

所有历史缓冲采用 Ping-Pong 资源，窗口尺寸或内部渲染分辨率变化时统一重建并清空历史。

---

## 7. DX12 基础层设计

### 7.1 初始化

- Debug 构建启用 D3D12 Debug Layer 和 GPU-Based Validation。
- 创建 DXGI Factory，按高性能优先级枚举适配器，并选择支持 DXR Tier 1.1 的硬件设备；目标验证设备为 RTX 4060 Laptop GPU。
- 检查 `D3D12_FEATURE_D3D12_OPTIONS5`，要求支持 Raytracing Tier 1.1。
- 创建 Direct Command Queue、Copy Command Queue 和对应 Fence。
- 创建三缓冲交换链，格式优先使用 `R8G8B8A8_UNORM`；HDR 显示作为后续功能。
- 创建 RTV、DSV、CBV/SRV/UAV 和 Sampler Descriptor Heap。
- 为每个 Frame Context 分配独立的 Command Allocator 和 Fence 值。

### 7.2 资源管理

必须实现统一资源包装，至少记录：

- `ID3D12Resource`。
- 当前资源状态。
- 格式、尺寸和用途。
- 对应的 SRV/UAV/RTV/DSV 描述符。
- 可选 GPU 虚拟地址。
- 调试名称。

实现一个持续映射的 Upload Ring Buffer，每帧按 Fence 回收空间。常量缓冲分配必须满足 256 字节对齐。

### 7.3 同步

- 禁止每帧调用 `WaitForGpu`。
- 只在复用 Frame Context、调整窗口大小和退出时等待对应 Fence。
- 资源状态转换统一由帧图或 Barrier 辅助层生成。
- 第一版使用传统 Resource Barrier；渲染稳定后再评估 Enhanced Barriers。
- Debug 模式开启 DRED，记录 Device Removed 时的 Breadcrumb 和 Page Fault 信息。

### 7.4 Shader 和管线缓存

- `build.rs` 或独立工具扫描 `shaders/` 并调用 DXC。
- 编译失败时输出完整文件、入口点、宏和 DXC 错误。
- 开发模式根据文件时间戳热重载 Compute/Graphics Shader。
- DXR State Object 的热重载先允许 GPU 空闲，后续再做无停顿替换。

---

## 8. DXR 光追管线

### 8.1 几何表示

当前 Panel、Rectangle 和 Sphere 最终统一转换为 GPU 三角形网格。第一版不实现程序化求交，以减少 Shader Binding Table 和 Intersection Shader 的复杂度。

- Cornell Box 墙体和灯光转换为三角形。
- 两个盒子转换为带索引三角形网格。
- 球体先使用细分网格；后续如有必要再实现程序化球体求交。
- 顶点至少包含位置、法线和 UV。
- 材质通过实例或几何索引查表，不能为每个材质复制完整 Shader。

### 8.2 加速结构

- 静态网格构建一次 BLAS，并在构建完成后进行 Compaction。
- 可变形网格使用允许更新的 BLAS，但第一阶段不实现蒙皮。
- 每个场景实例在 TLAS 中保存实例 ID、Mask 和 Hit Group 索引。
- 动态物体只更新受影响的变换和 TLAS。
- 加速结构 Scratch Buffer 使用可复用的临时内存池。

### 8.3 Shader Binding Table

第一版只使用两种 Ray Type：

1. Radiance Ray：计算最近交点、材质和下一次反弹。
2. Shadow Ray：只判断遮挡，可在命中后立即结束。

Shader Table 至少包含：

- 一个 RayGen Record。
- 每种 Ray Type 的 Miss Record。
- 每个几何实例对应的 Hit Group Record。

所有 Record 和 Table 必须遵守 DXR 对齐要求，并由专用构建器生成，禁止散落手算偏移。

### 8.4 路径追踪策略

第一版保留简单递归 DXR 管线，最大递归深度限制为 4。质量和正确性稳定后，再评估改成 Compute Shader + Inline Ray Query 的 Wavefront 路径追踪器。

每个像素每帧：

1. 使用帧索引和像素坐标生成低差异随机数。
2. 对像素位置加入亚像素抖动。
3. 发射主射线并写入第一交点 G-buffer。
4. 对非 Delta 材质执行面积光源采样。
5. 按 BSDF 生成下一条路径。
6. 使用 MIS 合并光源采样与 BSDF 采样。
7. 达到最大反弹或没有命中时终止。

当最大反弹提高到 4 次以上时，引入俄罗斯轮盘赌，生存概率由路径吞吐量决定，并对存活路径除以生存概率以保持无偏。

### 8.5 随机数

- 不在 Shader 中使用简单帧号哈希作为全部随机源。
- 第一版使用 PCG Hash 或 TEA 生成初始状态，并结合蓝噪声纹理扰动采样维度。
- 后续可加入 Sobol/Owen Scrambling，降低低 SPP 的结构化噪声。
- 所有随机采样必须能够通过固定种子复现，便于回归测试。

---

## 9. 实时降噪方案

### 9.1 信号拆分

至少拆成两路：

- 漫反射信号：可使用反照率解调，允许更强空间滤波。
- 镜面信号：依赖粗糙度和反射命中距离，历史通常更短，滤波更谨慎。

不能把玻璃、高光和漫反射放入同一颜色缓冲后使用同一组参数。

### 9.2 时域重投影

每个像素保存上一帧世界位置或通过深度重建世界位置，再使用上一帧视图投影矩阵获得历史 UV。

历史有效需要同时满足：

- 历史 UV 位于屏幕范围内。
- 物体 ID 或材质类别一致。
- 当前与历史法线点积超过阈值。
- 线性深度差在相对阈值内。
- 没有发生窗口重建、相机瞬移、场景重载或曝光突变。

动态实例必须提供当前和上一帧世界矩阵，运动矢量不能只考虑相机运动。

### 9.3 历史裁剪

在合并历史前，计算当前像素 3×3 邻域的亮度或 YCoCg 包围盒，将历史颜色裁剪到合理范围。这样可以减少旧高亮拖入新区域。

推荐策略：

- 历史短时使用较高当前帧权重。
- 历史稳定时逐渐增加历史权重。
- 高方差区域减少历史信任。
- 遮挡显露区域直接清空历史。
- 镜面信号按粗糙度和反射命中距离动态限制历史长度。

### 9.4 方差估计

维护亮度的一阶矩和二阶矩：

```text
均值     E[x]
二阶矩   E[x²]
方差     max(E[x²] - E[x]², 0)
```

方差既用于控制 À-Trous 颜色权重，也用于调试自适应采样。

### 9.5 GPU À-Trous

把当前 CPU 版二维 B3 样条核迁移为 Compute Shader：

- 每轮使用 5×5 核。
- 步长依次为 1、2、4、8，必要时增加 16。
- 使用 Group Shared Memory 缓存局部像素。
- 漫反射权重由亮度、法线、相对深度和物体 ID 决定。
- 镜面权重额外使用粗糙度、反射命中距离和视角变化。
- 每轮使用 Ping-Pong UAV，不能在同一资源内读写相邻像素。

### 9.6 NRD 和 Ray Reconstruction

自研降噪达到稳定基线后，增加运行时后端选项：

```text
降噪模式：关闭 / 自研 SVGF / NRD / DLSS Ray Reconstruction
```

接入第三方方案时，保持输入缓冲的语义和单位明确：

- 深度是线性视空间距离还是设备深度。
- 法线位于世界空间还是视空间。
- 运动矢量使用像素、UV 还是 NDC 单位。
- 信号是已解调还是未解调。
- 曝光是否提前应用。

这类不一致通常比 API 调用本身更容易造成鬼影和亮度错误。

---

## 10. DLSS 接入方案

### 10.1 接入顺序

DLSS 必须在基础渲染正确后接入：

1. 先完成稳定的原生分辨率输出。
2. 实现正确的亚像素抖动和运动矢量。
3. 接入 DLSS Super Resolution。
4. 接入 Reflex，记录模拟和渲染延迟标记。
5. 评估 DLSS Ray Reconstruction 是否替换当前降噪链。
6. 基础帧率和 UI 正确后再接入 Frame Generation。

### 10.2 Streamline 桥接层

建议在 `native/streamline_bridge` 创建小型 C++ 静态库或 DLL，对 Rust 提供以下接口：

```text
创建与销毁 Streamline 上下文
设置渲染分辨率和输出分辨率
标记颜色、深度、运动矢量、曝光等资源
设置相机矩阵、抖动和重置历史标记
查询功能支持和推荐设置
调度 DLSS SR / RR / Frame Generation
提交 Reflex 标记
```

Rust 只传递稳定的句柄、枚举和 POD 结构，禁止跨 FFI 传递 Rust 容器或所有权复杂的对象。

### 10.3 DLSS 输入要求

在接入前必须确保：

- 运动矢量包含相机和物体运动。
- 抖动序列和投影矩阵一致。
- 深度方向、Near/Far 和是否反向 Z 配置正确。
- 调整窗口大小后向 DLSS 发送历史重置。
- 相机瞬移、场景切换、内部比例变化时重置历史。
- UI 在超分和插帧之后合成，避免 UI 被错误重建。

### 10.4 Frame Generation 限制

- 插帧只提高显示帧率，不提高模拟频率或输入响应速度。
- 必须先保证基础渲染延迟可接受。
- UI、光标和调试覆盖层需要按 SDK 推荐方式处理。
- 性能统计同时显示基础 FPS 和生成后 FPS，不能只显示更大的数字。

---

## 11. 相机、运动矢量与交互

### 11.1 相机

- 使用右手或左手坐标系必须全项目统一，并在 Shader 共享头中注明。
- 建议使用反向 Z，提高远距离深度精度。
- 保存当前帧和上一帧无抖动/带抖动视图投影矩阵。
- 相机瞬移、FOV 突变和分辨率变化时设置 `reset_history`。

### 11.2 输入

- 鼠标右键控制视角。
- WASD 平移，Q/E 垂直移动。
- Shift 加速，Ctrl 减速。
- F1 切换调试 UI。
- F2 切换原始/降噪输出。
- F3 冻结相机但继续累计。
- F4 清空历史。
- F12 截图。

### 11.3 动态物体

动态实例保存：

- 当前世界矩阵。
- 上一帧世界矩阵。
- 当前和上一帧法线矩阵。
- 实例 ID 和材质 ID。

物体 ID 只在场景重载时重新分配，不能每帧变化，否则历史会全部失效。

---

## 12. 场景与资产

### 12.1 第一阶段

仅支持代码生成的 Cornell Box，确保和 CPU 参考场景一致。

### 12.2 第二阶段

加入 glTF 2.0：

- 静态三角网格。
- 基础颜色纹理。
- 金属度和粗糙度。
- 法线纹理。
- 发光纹理。
- 节点层级和实例变换。

纹理进入 Shader 前统一颜色空间：基础颜色和发光纹理按 sRGB 解码，法线、粗糙度和金属度保持线性。

### 12.3 材质模型

第一版 GPU 材质保持与 CPU 版一致，完成验证后再升级为金属度/粗糙度 PBR：

- Lambert 或 Burley Diffuse。
- GGX/Trowbridge-Reitz 镜面分布。
- Smith Masking-Shadowing。
- Schlick Fresnel。
- 介质透射和全反射。

升级 PBR 时，CPU 参考实现也应同步，避免失去可比性。

---

## 13. 性能预算

以 60 FPS 的 16.67 毫秒为总预算，初始分配如下：

| 阶段 | 目标耗时 |
| --- | ---: |
| CPU 更新和命令录制 | 0.5–1.0 ms |
| BLAS/TLAS 更新 | 0.5–1.5 ms |
| 1 SPP 路径追踪 | 5.0–8.0 ms |
| 时域重投影和矩更新 | 0.5–1.0 ms |
| À-Trous 或 NRD | 2.0–4.0 ms |
| Tone Mapping / DLSS / UI | 1.0–2.5 ms |
| 同步与余量 | 1.5–3.0 ms |

预算是验收目标，不是硬编码。必须通过 GPU Timestamp Query 实测，不凭 CPU 墙钟时间判断 GPU 性能。

显存使用遵守以下原则：

- 启动时通过 DXGI 查询专用显存预算。
- 正常运行控制在当前预算的 70% 以内。
- 历史缓冲随内部渲染分辨率而不是输出分辨率分配。
- BLAS Compaction 后释放原始大缓冲。
- 调整分辨率时延迟释放旧资源，等待对应 Fence 后回收。

---

## 14. 调试与可观测性

### 14.1 UI 面板

至少显示：

- CPU FPS、基础 GPU FPS 和生成后 FPS。
- 每个 GPU Pass 的耗时。
- 当前输出/内部渲染分辨率。
- 每帧 SPP、最大反弹和累计历史长度。
- BLAS/TLAS 数量和更新时间。
- 显存预算、当前使用量和资源数量。
- 当前降噪器和 DLSS 模式。

### 14.2 缓冲可视化

支持查看：

- 原始漫反射和镜面信号。
- 反照率。
- 世界法线。
- 线性深度。
- 运动矢量。
- 物体 ID。
- 历史长度。
- 方差。
- 每轮 À-Trous 输出。
- 历史拒绝原因。

### 14.3 GPU 调试

- 每个资源和命令列表设置可读调试名。
- Debug 构建将 D3D12 错误和警告提升为日志。
- 每个 Pass 放置 PIX Event。
- Device Removed 时输出 DRED 信息。
- 对复杂光追帧使用 Nsight Graphics 检查 Shader、SBT 和加速结构性能。

---

## 15. 测试策略

### 15.1 CPU 单元测试

- 矩阵、投影和运动矢量变换。
- AABB 和场景包围盒。
- 材质参数转换。
- Shader Table 对齐和偏移。
- Descriptor 分配与回收。
- FFI POD 结构的大小和对齐。

### 15.2 Shader 测试

- 所有 Shader 在 CI 中完成 DXC 编译。
- 对共享结构生成 CPU/HLSL 偏移检查。
- 固定随机种子渲染小分辨率 Cornell Box，比较统计误差而不是逐像素完全相同。

### 15.3 GPU 冒烟测试

本地 RTX 4060 Laptop GPU 每次重要阶段至少验证：

- 创建和销毁窗口无资源泄漏。
- 连续调整窗口大小不崩溃。
- 最小化和恢复窗口正确。
- 运行 30 分钟无 Device Removed。
- 相机快速移动后历史能够恢复。
- 切换降噪器和 DLSS 模式不会残留旧历史。

### 15.4 图像回归

维护以下参考场景：

- Cornell Box 漫反射。
- 单镜面球。
- 单玻璃球。
- 强小光源和高动态范围。
- 动态遮挡物。
- 细线与薄几何。

每个场景保存 CPU 高采样参考图、GPU 高积累图和实时动态录屏。

---

## 16. 分阶段实施计划

每个阶段必须保持可运行，并独立提交。提交信息继续使用中文。

### 阶段 0：冻结 CPU 参考实现

工作内容：

- 将现有 CPU 渲染器移动到清晰的 `cpu_reference` 模块。
- 固定 Cornell Box 参数、相机和随机种子。
- 保存 1、16、500 SPP 参考图。
- 为 CPU 渲染增加命令行模式，避免以后默认启动时阻塞窗口。

验收条件：

- `cargo run -- --cpu-reference` 能生成确定性的参考图。
- 默认入口仍可临时运行，且所有现有测试通过。

### 阶段 1：DX12 窗口和交换链

工作内容：

- 引入 `winit` 和 `windows` crate。
- 创建设备、队列、交换链、RTV、Fence 和三帧上下文。
- 实现清屏、Present、窗口调整大小和 Debug Layer。

验收条件：

- 窗口稳定显示指定颜色。
- 连续调整大小、最小化和恢复无错误。
- PIX 能捕获一帧。

### 阶段 2：DX12 资源与 Shader 基础设施

工作内容：

- 实现资源包装、Descriptor 分配、Upload Ring 和 Shader 编译。
- 加入 GPU Timestamp Query 和基础 UI。
- 建立资源状态转换辅助层。

验收条件：

- Compute Shader 能写入纹理并显示。
- Shader 修改后可以重新编译。
- UI 显示各 Pass GPU 耗时。

### 阶段 3：第一个 DXR 三角形

工作内容：

- 创建 BLAS、TLAS、DXR State Object 和 Shader Table。
- 实现 RayGen、Miss 和 ClosestHit。
- 将命中法线显示为颜色。

验收条件：

- RTX 4060 Laptop GPU 上确认使用 DXR Tier 1.1 或更高版本。
- 窗口实时显示可旋转观察的三角形或简单盒子。
- Nsight/PIX 中可看到 `DispatchRays`。

### 阶段 4：Cornell Box GPU 路径追踪

工作内容：

- 上传 Cornell Box 三角网格、材质和面积光源。
- 实现漫反射、金属、玻璃、阴影射线、NEE 和 MIS。
- 输出第一交点 G-buffer。
- 加入相机控制和每帧随机采样。

验收条件：

- GPU 高积累结果与 CPU 参考图主要亮度和颜色一致。
- 原始 1 SPP 可以实时显示。
- 相机移动时没有资源或同步错误。

### 阶段 5：渐进积累

工作内容：

- 静止相机时累计独立样本。
- 相机、场景或分辨率变化时清空历史。
- 显示累计样本数和收敛过程。

验收条件：

- 静止画面随帧数增加持续接近参考图。
- 相机移动后不会混入旧视角图像。

### 阶段 6：GPU 时空降噪

**当前状态（2026-08-08）：阶段 6 工程实现已完成；阶段 7 代码工作包 7A–7F 已完成，7G 真机和性能验收待执行。**

工作内容：

- 实现运动矢量和历史重投影。
- 实现历史有效性检查、邻域裁剪、矩和方差。
- 将 CPU À-Trous 迁移为 Compute Shader。
- 分离漫反射和镜面信号。

验收条件：

- 1 SPP 静止画面在短时间内稳定。
- 相机移动和动态遮挡时无大面积鬼影。
- 可以实时查看拒绝掩码、方差和历史长度。

本阶段已在 NVIDIA GeForce RTX 4060 Laptop GPU（DXR Tier 1.2）上通过 Release 和启用 GPU-Based Validation 的 Debug 冒烟测试。一次 Release 稳态采样的结果为：1280×720 约 113 FPS / 8.58 ms，1600×900 约 72 FPS / 13.81 ms，1920×1080 约 51 FPS / 19.43 ms。因此 720p–900p 内部光追分辨率已达到 60 FPS 基线；当前不降分辨率的原生 1080p 落在 45–60 FPS 最低可用区间，但未达到 60 FPS。这些数据是单机单次测量，不代替 PIX/Nsight 长时帧分析与动态鬼影的图像对比验收。

### 阶段 7：动态场景和 glTF

详细实施、分提交顺序和数据契约见 [`阶段7动态场景与glTF执行计划.md`](./阶段7动态场景与glTF执行计划.md)。阶段 7 代码评审发现的问题、修复工作包和新版 Luna 提示词见 [`阶段7评审问题修复执行方案.md`](./阶段7评审问题修复执行方案.md)。

**实现状态（2026-08-08）：7A–7F 已提交；后续代码评审确认仍有 TLAS update scratch、动画 pivot、glTF sampler/双面语义和 PBR 信号分离等 R1–R8 问题，必须先按修复方案关闭。7G 已完成基础自动化冒烟和 Release 计时，但公开 PBR 资产、30 秒动画和逐调试视图图像核验仍未完成，不能据此宣称阶段 7 完成。**

工作内容：

- 加入 glTF 网格、纹理和 PBR 材质。
- 支持实例变换和 TLAS 更新。
- 为动态物体生成正确运动矢量。
- 保留独立 DXR、Temporal、À-Trous、ToneMap pass 及其资源状态和调试视图。
- 明确拒绝 alpha/skin/morph/animation channel/required extension 等未支持 glTF 特性。

验收条件：

- 能加载至少一个公开 glTF 测试场景。
- 物体运动时阴影、反射和降噪历史正确更新。

本工作区的 7G 实测记录：

| 构建/场景 | 结果 |
| --- | --- |
| Debug GPU-Based Validation + RTX 4060 Laptop GPU + Cornell | 启动运行约 12 秒，无 D3D12 验证错误 |
| Debug GPU-Based Validation + 静态 Triangle | 启动运行约 12 秒，无验证错误；仅输出缺少 tangent 的预期警告 |
| Debug GPU-Based Validation + 动画 Triangle | 启动运行约 15 秒，无验证错误；仅输出缺少 tangent 的预期警告 |
| Debug UI 序列 | resize、3 次 F1、最小化/恢复运行，无验证错误 |
| Release 1280×720 | Total 约 10.6–11.1 ms；AS 0.01；Path Trace 约 5.2–5.9；Temporal 约 1.5–1.6；À-Trous 约 3.4–4.0 ms |
| Release 1600×900 | Total 约 11.1–11.8 ms；AS 0.01；Path Trace 约 5.6–5.9；Temporal 约 1.6–1.7；À-Trous 约 3.8–4.1 ms |
| Release 1907×1044（屏幕工作区限制） | Total 约 23.2–23.8 ms；AS 0.01；Path Trace 约 11.4–11.7；Temporal 约 3.4；À-Trous 约 8.0–8.2 ms |

尚未达到的条件：仓库没有可离线验收的 Khronos `BoxTextured` 或 `DamagedHelmet.glb`，因此未完成公开 PBR 资产的纹理方向/法线图人工截图；未执行 30 秒连续动画、逐个 F1 视图的图像对比和 720p/900p/1080p 全套 Debug Validation 长时序列。上述限制必须在补齐资产和测试时间后再关闭。

### 阶段 8：性能优化

工作内容：

- 优化 BLAS/TLAS 构建和更新。
- 降低 Descriptor 和资源切换开销。
- À-Trous 使用 Group Shared Memory。
- 根据 GPU 计时建立动态内部渲染分辨率。
- 评估 Inline Ray Query/Wavefront 路径追踪。

验收条件：

- Cornell Box 在目标设置下达到稳定 60 FPS。
- 无每帧 CPU/GPU 强制同步。
- 显存占用低于预算的 70%。

### 阶段 9：NRD 可选后端

工作内容：

- 建立 C++ C ABI 桥接层。
- 接入 NRD 漫反射和镜面降噪。
- UI 支持自研降噪与 NRD 对比。

验收条件：

- 两种降噪器可运行时切换。
- 输入单位、运动矢量和历史重置全部正确。

### 阶段 10：DLSS Super Resolution 和 Reflex

工作内容：

- 接入 Streamline。
- 标记颜色、深度、运动矢量和曝光资源。
- 支持 Quality、Balanced、Performance 和 DLAA。
- 接入 Reflex 标记。

验收条件：

- 各 DLSS 模式可安全切换并正确重置历史。
- UI 不被超分处理破坏。
- 输出分辨率变化不产生崩溃或资源泄漏。

### 阶段 11：Ray Reconstruction 与 Frame Generation

工作内容：

- 评估并接入 DLSS Ray Reconstruction。
- 对比自研 SVGF、NRD 和 Ray Reconstruction。
- 基础帧率达标后接入 Frame Generation。
- 正确处理 UI、光标、Reflex 和性能统计。

验收条件：

- Ray Reconstruction 在运动中不出现明显错误历史。
- 同时显示基础 FPS、生成后 FPS 和延迟。
- 关闭所有 NVIDIA 可选功能时，程序仍能使用自研路径运行。

### 阶段 12：产品化收尾

工作内容：

- 设置文件和命令行参数。
- 启动时功能检测和友好错误提示。
- Shader 与资产打包。
- 截图、性能日志和崩溃诊断。
- Release 构建和安装说明。

验收条件：

- 新机器上按照文档能够完成构建和运行。
- 缺少 DLSS/NRD SDK 时仍能构建基础版本。
- 连续运行 30 分钟无明显泄漏或 Device Removed。

---

## 17. 风险与应对

| 风险 | 表现 | 应对措施 |
| --- | --- | --- |
| DX12 Rust 示例较少 | 初始化和 COM 调用容易出错 | 封装小模块，严格启用 Debug Layer，阶段式验收 |
| GPU 同步错误 | 随机闪烁、崩溃、Device Removed | Frame Context + Fence，统一资源状态追踪，使用 DRED |
| Shader 结构布局不一致 | 材质、矩阵数据异常 | 共享定义、静态大小检查、显式对齐 |
| 时域拖影 | 相机或物体移动留下残影 | 正确运动矢量、历史拒绝、邻域裁剪、历史长度限制 |
| 光追性能不足 | 1 SPP 仍无法达到目标帧率 | 降低内部分辨率/反弹数，优化 AS，使用 DLSS SR |
| 镜面降噪困难 | 高光闪烁或被滤糊 | 漫反射/镜面拆分，引入粗糙度和命中距离，评估 NRD/RR |
| NVIDIA SDK 接入阻塞 | Rust 缺少官方绑定或版本变化 | 独立 C++ C ABI 桥接，基础路径不依赖专有 SDK |
| 过早优化 | 复杂架构无法验证正确性 | 先固定 Cornell Box 和图像基准，再逐步优化 |
| Frame Generation 掩盖问题 | 显示 FPS 高但输入延迟和基础帧率差 | 同时展示基础 FPS、生成 FPS 和延迟，最后接入 FG |

---

## 18. 明确不采用的方案

- 不把现有 CPU 多线程渲染循环直接放进窗口作为最终实现。
- 不在第一阶段接入 DLSS 或 NRD。
- 不以 OIDN 作为实时逐帧降噪器。
- 不保留 20 次实时反弹作为默认配置。
- 不在没有正确运动矢量前实现时域滤波或 DLSS。
- 不在基础帧率不达标时使用 Frame Generation 掩盖性能问题。
- 不把所有资源每帧创建和销毁。
- 不使用 `WaitForGpu` 作为常规帧同步方案。
- 不删除 CPU 参考实现，直到 GPU 结果完成系统性验证。

---

## 19. 第一阶段立即执行清单

下一次开始编码时，只执行以下内容：

1. 为 CPU 版增加明确的参考渲染入口。
2. 引入 `winit` 和 `windows` crate。
3. 创建设备、命令队列、交换链和三帧上下文。
4. 启用 Debug Layer、Info Queue 和 DRED。
5. 实现窗口清屏、Present、Resize、最小化和恢复。
6. 使用 PIX 捕获并验证第一帧。

在以上六项全部稳定前，不创建 BLAS/TLAS，也不接入任何 NVIDIA SDK。

---

## 20. 最终完成定义

只有同时满足以下条件，项目才达到本文档定义的最终效果：

- 默认启动进入实时窗口，而不是生成单张 PNG 后退出。
- RTX 4060 Laptop GPU 通过 DXR 执行场景求交和路径追踪。
- 用户可以实时移动相机并观察动态收敛。
- 1 SPP 画面经过时域与空间降噪后可稳定交互。
- 漫反射、镜面和玻璃信号没有明显串色或大面积拖影。
- Cornell Box 在目标设置下达到稳定 60 FPS。
- 可以显示关键 G-buffer、历史、方差和 GPU 时间。
- CPU 参考图和 GPU 高积累结果通过质量对比。
- 自研降噪始终可用，不依赖专有 SDK。
- 可选启用 NRD、DLSS Super Resolution、Ray Reconstruction 和 Frame Generation。
- 窗口调整大小、最小化、恢复、场景切换和退出均无资源错误。
- Debug Layer 无未处理错误，30 分钟运行无 Device Removed 和明显显存增长。

---

## 21. 官方资料入口

- Microsoft DirectX Raytracing：<https://learn.microsoft.com/windows/win32/direct3d12/direct3d-12-raytracing>
- DirectX 12 编程指南：<https://learn.microsoft.com/windows/win32/direct3d12/directx-12-programming-guide>
- DirectX Shader Compiler：<https://github.com/microsoft/DirectXShaderCompiler>
- DirectX Graphics Samples：<https://github.com/microsoft/DirectX-Graphics-Samples>
- PIX on Windows：<https://devblogs.microsoft.com/pix/>
- NVIDIA Streamline：<https://github.com/NVIDIAGameWorks/Streamline>
- NVIDIA NRD：<https://github.com/NVIDIAGameWorks/RayTracingDenoiser>
- NVIDIA Nsight Graphics：<https://developer.nvidia.com/nsight-graphics>
- Intel Open Image Denoise：<https://www.openimagedenoise.org/>
