# 阶段 9：重建输入契约与 NRD 最小后端执行方案

## 1. 文档状态与结论

本文是 [`实时DXR渲染器实施方案.md`](实时DXR渲染器实施方案.md) 中阶段 9 的可执行细则，供 Luna 分包实现、Codex 逐包 review。

- 目标硬件：NVIDIA GeForce RTX 4060 Laptop GPU。
- 目标平台：Windows 10/11、D3D12、MSVC x64。
- 当前默认路径：自研 SVGF（Temporal + 四轮 À-Trous）。
- 阶段 9 新路径：可选的 `NRD REBLUR_DIFFUSE_SPECULAR`。
- 后续目标：阶段 10 接入 DLSS Super Resolution/Reflex，阶段 11 接入 DLSS Ray Reconstruction（RR）。
- 文档日期：2026-08-09。
- 实现状态：尚未开始；本文提交只定义范围、接口、提交顺序和验收标准，不代表任何 NRD 代码已经完成。

本阶段采用“缩减方案”：优先建立 NRD 与 DLSS RR 都能复用的重建输入契约，只实现一个最小、正确、可切换的 NRD REBLUR 对照后端。不得在本阶段加入 RELAX、SH、SIGMA、Streamline、DLSS、复杂 NRD 调参或新的采样架构。

### 1.1 为什么仍保留最小 NRD

DLSS RR 会替代传统降噪与超分辨率链，因此 NRD 不是 RR 的前置依赖，也不能与 RR 串联使用。保留最小 NRD 的目的只有三个：

1. 验证法线、粗糙度、viewZ、运动矢量、材质反照率、矩阵、hit distance 和历史重置是否满足专业重建器要求。
2. 提供比自研 SVGF 更成熟的传统降噪对照，便于阶段 11 比较 SVGF、NRD 与 RR。
3. 在 RR 插件、驱动或运行时不可用，以及当前动态内部渲染分辨率模式下，保留一个可选传统后端。

本阶段不以“把 NRD 调到最终产品画质”为目标。NRD 最小后端通过正确性和短矩阵后应停止扩张，把后续主要投入留给 Streamline 和 DLSS RR。

### 1.2 阶段 8 的关系

阶段 8 的代码基础足以开始阶段 9，但阶段 8 的历史绝对性能门槛、长时稳定性和人工交互债务仍然存在。阶段 9：

- 不得把阶段 8 改成“已完成”；
- 不得删除或放宽阶段 8 的验证脚本和门槛；
- 不得用 NRD 的性能或画质替代 SVGF 默认路径的阶段 8 证据；
- 必须保持默认、不带 SDK 的基础构建和 SVGF 输出不回归。

### 1.3 官方依据和版本决策

实现必须以 NVIDIA 官方资料为准：

- NRD 仓库：<https://github.com/NVIDIA-RTX/NRD>
- NRD v4.17.3 发布页：<https://github.com/NVIDIA-RTX/NRD/releases/tag/v4.17.3>
- NRD v4.17.3 README：<https://github.com/NVIDIA-RTX/NRD/blob/v4.17.3/README.md>
- NRD Sample：<https://github.com/NVIDIA-RTX/NRD-Sample>
- Streamline DLSS RR 指南：<https://github.com/NVIDIA-RTX/Streamline/blob/main/docs/ProgrammingGuideDLSS_RR.md>

截至本文日期，NRD `master` README 已显示 v4.17.4，但最新正式 Release 页面仍是 v4.17.3，发布提交前缀为 `792eff1`。阶段 9 必须锁定正式发布的 `v4.17.3`，不能跟随 `master`、浮动分支或只写“latest”。首次获取依赖后应记录：

- 仓库 URL；
- tag `v4.17.3`；
- `git rev-parse HEAD` 得到的完整提交；
- 提交必须以前缀 `792eff1` 开头；
- NRI `v179`、MathLib `v11` 和 ShaderMake
  `18f5a344e7ca8fa65daaf079d07bc8ce38453e05` 的解析提交或归档 SHA-256；
- SDK/源码与三个传递依赖的获取方式；
- NRD/NRI/MathLib/ShaderMake 许可文件 SHA-256。

禁止猜测或手写缺失的完整提交。若 tag 解析结果与官方发布页前缀不一致，停止实现并报告，不得继续构建。

NRD 使用 NVIDIA RTX SDK License。实现和分发至少要满足：

- 不把 NRD SDK 作为独立产品分发；
- 应用必须包含实质性的额外功能；
- 分发的源码修改或派生作品包含 NVIDIA 要求的源码声明；
- 最终用户文档显著注明使用 NVIDIA NRD；
- 保留 SDK 及第三方组件许可；
- 不暗示 NVIDIA 对本项目的赞助或背书。

这不是法律意见；若项目公开或商业分发，应再次审阅锁定版本的完整许可。

## 2. 完成定义

只有以下条件全部满足，才能把阶段 9 标记为完成：

1. 不带 NRD feature、没有 NRD SDK 的机器仍能执行 `cargo build --locked`、测试和运行 SVGF 默认路径。
2. NRD 及 NRI/MathLib/ShaderMake 传递依赖固定到可复核的 tag、完整提交或归档 SHA-256；构建过程不访问浮动分支，`build.rs` 不执行网络下载。
3. 共用重建输入契约已落地，至少覆盖 noisy HDR color、diffuse/specular albedo、世界法线、线性粗糙度、linear viewZ、稠密运动矢量、specular hit distance、当前/上一帧非抖动矩阵和 reset 标志。
4. `--denoiser svgf|nrd-reblur` 有确定行为；默认始终是 `svgf`。
5. 支持 NRD 的构建可以用 F3 在 SVGF 与 NRD REBLUR 间切换；切换创建新资源代、重置历史，旧代按 fence 退休，不执行 GPU idle wait。
6. NRD 使用 `REBLUR_DIFFUSE_SPECULAR`、默认设置起步、官方 HLSL 打包/解包 helper；输入中无非预期 NaN/Inf、负 radiance、错误 viewZ、错误运动符号或错误 hit distance。
7. NRD 调度后显式恢复应用 descriptor heap、root signature/PSO 所需状态，并与 `TrackedResource` 状态一致；Debug Layer 和 GPU-Based Validation 无错误。
8. resize、固定 render scale、动态 render scale、静态 Cornell 和动画 glTF 不崩溃、不泄漏、不复用错误历史。
9. RTX 4060 Laptop 上的短性能矩阵有原始 JSON，NRD 后端 GPU Total p95 不高于 16.67 ms；显存峰值低于 DXGI budget 的 70%。该门槛不要求 NRD 比 SVGF 更快。
10. NRD 的静态收敛、相机运动、动画物体和灯附近高方差区域没有明显错误历史、整片黑块、NaN 闪烁、边缘漏光或比 raw 1 SPP 更严重的持续跳噪。

本阶段不运行 600 秒或 1800 秒测试。每个 benchmark 子进程最长 30 秒；长期稳定性只记录为后续人工债务，不能伪装成已通过。

## 3. 范围与非目标

### 3.1 本阶段包含

- 重建后端枚举、CLI、F3 切换、标题和 benchmark 元数据。
- 可选 Cargo feature 和无 SDK 基础构建。
- NRD/NRI 固定版本获取、许可、构建与 C++ C ABI 桥。
- 为 NRD 和 RR 复用的 G-buffer/重建输入。
- NRD 专属输入准备、REBLUR 调度和输出合成。
- 资源代际、barrier、descriptor heap 恢复、history reset。
- 新增 NRD GPU 时间、显存和短验证证据。

### 3.2 明确不包含

- DLSS SR、DLSS RR、Frame Generation、Reflex 或 Streamline 代码。
- NRD RELAX、SIGMA、REFERENCE、SH/SG 模式和多层 path-space decomposition。
- NRD 与 RR 串联；任何一帧只能选择一个重建/降噪后端。
- ReSTIR、RTXDI、SHARC、Wavefront、Inline Ray Query 或新的光照采样系统。
- 为适配 NRD 重写全部 PBR、加入第二条主路径或提高每像素光线数。
- 把 NRD 设为默认，或删除 SVGF。
- 大规模 UI framework；阶段 9 只用 CLI、F3、窗口标题和现有 debug view 系统。
- 以调参数掩盖错误的 motion、viewZ、矩阵、material factor 或 hit distance。

遇到必须扩大到以上内容才能继续的情况，应停止对应工作包，提交最小可构建状态并在 review 中说明原因。

## 4. 当前实现审计与必须修正的差异

| 项目 | 当前实现 | NRD / RR 要求 | 阶段 9 决策 |
| --- | --- | --- | --- |
| 漫反射信号 | `RawDiffuse` 为材质调制后的贡献，SVGF 用 `raw / albedo` 解调 | NRD 要求用官方 material factors 解调；RR 要 noisy full color 与 diffuse albedo | 保留 SVGF 语义，新增显式 reconstruction/material contract；NRD prep 使用官方 helper |
| 镜面信号 | `RawSpecular` 含镜面贡献和主表面 emissive | NRD 需要可正确解调的 specular radiance；RR 需要 noisy color 和 specular albedo | 不再把“含 emissive”当作 NRD 纯镜面；必要时单独输出 primary emissive，合成时加回 |
| 法线/粗糙度 | `R16G16B16A16_FLOAT`，RGB 为 `N * 0.5 + 0.5`，A 为线性 roughness | NRD 要世界法线、线性 roughness 并按库编译编码打包；RR 接受世界/视图法线和线性 roughness | 保留现有资源供 SVGF；共用 contract 明确世界法线和线性 roughness；NRD 用 `NRD_FrontEnd_PackNormalAndRoughness` |
| 深度 | `GBufferDepth = RayTCurrent()`，是主射线距离 | NRD `IN_VIEWZ` 是主表面 view-space Z；RR 可用 linear depth，但必须与 motion/matrix 一致 | 新增真正的 signed/positive convention 明确的 linear viewZ；不得把 RayT 当 viewZ |
| 运动矢量 | 当前为 `currentUv - previousUv` 的像素值；SVGF 用 `previous = current - motion` | NRD 要 `old = new + MV`，推荐 2.5D，`.z = viewZprev - viewZ`；RR 要稠密 camera + object motion 和明确 scale | 不直接复用现有纹理；生成适配器/专用 reconstruction motion，并用解析测试验证符号和单位 |
| 相机矩阵 | shader 只传 camera position/yaw/pitch 并在 HLSL 构造射线 | NRD 要当前/上一帧非抖动 `worldToView`、`viewToClip`，列向量、列主序；Streamline 后续要一组完整相机常量 | Rust 侧建立统一矩阵快照和明确转置边界；不能在 C++ 桥内猜矩阵布局 |
| 抖动 | 每像素随机 ray jitter，不是整帧 projection jitter | NRD 矩阵必须非抖动；Streamline projection matrix 也不含 jitter，jitter 单独传 | 阶段 9 的 `camera_jitter_px` 为 `[0,0]`；不把每像素随机数伪装成 camera jitter，阶段 10 再设计全帧 jitter |
| hit distance | 只有一个 `GBufferHitDistance`，且目前只在 `sampledSpecular` 时写 child hitT | NRD 要 lobe 内第一跳 hitT；不能含 primary hitT，不能除 PDF/BRDF，跳过的 lobe 才能为 0；RR 要 primary surface 到 specular hit 的世界距离 | 增加明确命名的 first-bounce/specular hit distance；不要继续沿用含糊资源名或错误的零值语义 |
| sky / miss | depth、normal、motion 多为 0 | NRD 建议 sky 的 viewZ 超过 `denoisingRange`，guide 不得有 NaN/Inf | input prep 对 miss 写确定的无效 viewZ、零 motion、零 radiance；不得让 sky 污染历史 |
| 资源尺寸 | 每次内部尺寸变化创建完整 `RenderResourceGeneration` | NRDIntegration 预分配固定尺寸；普通 resize 要重建，动态 viewport 可用 `rectSize` | 最小实现把 NRD instance 归属于资源代，尺寸等于该代 render extent；切换尺寸创建新代并 fence 退休旧代 |
| 帧并发 | 三个 Frame Context | NRD `queuedFrameNum` 应等于 frames in flight | 固定为 3，并在桥的静态断言/日志中记录 |
| descriptor heap | 应用每帧绑定自己的 CBV/SRV/UAV 与 sampler heap | NRDIntegration 会绑定内部 heap 和 root signature | NRD 调度后必须重新绑定应用两个 heap；后续 ToneMap 必须重新绑定自己的 root signature/PSO/table |

任何一项没有可运行测试或可观察 debug 证据，都不能只凭截图认定正确。

## 5. 目标架构

### 5.1 后端选择

新增稳定枚举，命名可按 Rust 风格微调，但语义不得变化：

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DenoiserBackend {
    #[default]
    Svgf,
    NrdReblur,
}
```

CLI：

```text
--denoiser svgf|nrd-reblur
```

规则：

- 未传参数时为 `svgf`。
- `--denoiser nrd-reblur` 在不含 `nrd` feature 的构建中必须启动失败，并给出如何重新构建的明确错误；不得静默回退。
- F3 只在运行时能力确实可用时切换。不可用时保持当前后端并输出稳定诊断，例如 `denoiser_switch blocked=nrd_not_built`。
- benchmark/capture JSON、窗口标题和 stderr 切换诊断都记录实际后端、编译能力、NRD 版本和资源代 ID。
- `--atrous-mode` 只影响 SVGF；NRD 后端传该参数可以接受但必须记录 `inactive_for_backend=true`，或者在参数层明确拒绝。选择一种行为并加测试，禁止悄悄影响 NRD。

运行管线：

```text
DXR Path Trace
  ├─ 原有 Raw/G-buffer ────────────────┐
  └─ 共用 Reconstruction Guides ───────┤
                                      ├─ SVGF: Temporal → À-Trous ──┐
                                      └─ NRD: Prep → REBLUR → Compose ├─ ToneMap → Present
```

SVGF 与 NRD 是互斥分支，不能同帧同时执行并把两个时间都算入 Total。

### 5.2 编译与依赖边界

Cargo 增加 opt-in feature：

```toml
[features]
default = []
nrd = []
```

要求：

- 未启用 feature 时，`build.rs` 不寻找 CMake、不读取 NRD 环境变量、不链接 C++ 库、不编译 NRD shader。
- 启用 feature 时使用显式 `NRD_SOURCE_DIR` 或等价环境变量；NRD 与所有传递依赖都必须已经由显式获取脚本准备好。
- `build.rs` 只构建本地依赖，不得执行 `git clone`、下载压缩包或访问网络。
- 获取脚本可以是 `scripts/fetch_nrd.ps1`，但必须由开发者显式运行；下载/clone 到 `.gitignore` 的 `external/nrd-v4.17.3/`。
- v4.17.3 的官方 CMake 会通过 `FetchContent` 获取 NRI `v179`、MathLib `v11` 和固定 ShaderMake commit。获取脚本必须预取并校验这三个依赖；CMake 调用通过 `FETCHCONTENT_SOURCE_DIR_NRI`、`FETCHCONTENT_SOURCE_DIR_MATHLIB`、`FETCHCONTENT_SOURCE_DIR_SHADERMAKE` 和 `FETCHCONTENT_FULLY_DISCONNECTED=ON` 指向本地缓存，确保 Cargo 构建离线。
- 获取脚本必须校验 tag/commit/归档 SHA-256 和许可文件，重复运行幂等；已存在但版本或 hash 不符时拒绝覆盖。
- 不提交生成的 `_NRD_SDK`、NRI 构建树、PDB、DLL 或整个上游源码。仓库只提交 lock manifest、获取说明、许可副本/通知和本项目桥代码。

NRD 构建配置：

- `NRD_NRI=ON`，使用官方 `NRDIntegration` 的 D3D12 wrapper；本阶段不手写完整 native NRD RHI。
- `NRD_STATIC_LIBRARY=ON`、`NRD_EMBEDS_DXIL_SHADERS=ON`、`NRD_EMBEDS_DXBC_SHADERS=OFF`、`NRD_EMBEDS_SPIRV_SHADERS=OFF`，NRI 只启用 D3D12。实例只创建 `REBLUR_DIFFUSE_SPECULAR`；NRD CMake 没有公开的“只编译一个 denoiser”选项，不得为删源码而修改上游。
- 显式设置 `NRD_NORMAL_ENCODING=2`（`R10_G10_B10_A2_UNORM`）和 `NRD_ROUGHNESS_ENCODING=1`（`LINEAR`），prep 纹理使用匹配格式，并通过 `LibraryDesc` 在运行时再次校验。
- 优先静态链接 NRD/NRI 到本项目的 C++ bridge，避免额外 NRD DLL 部署；若官方配置阻止可靠静态链接，必须在 review 前说明并完整部署相应 DLL 和许可。
- MSVC CRT 与 Rust MSVC 目标保持一致，Debug/Release 都使用动态 CRT `/MDd`、`/MD`，不允许同一进程混用不兼容 CRT。
- CMake 至少 3.22；构建输出放在 Cargo `OUT_DIR` 或 `target` 下，不污染源码目录。
- C++ exception、RTTI 和 allocator 所有权不得穿过 C ABI。

建议新增：

```text
native/nrd_bridge/
  CMakeLists.txt
  include/nrd_bridge.h
  src/nrd_bridge.cpp
third_party/nrd/
  README.md
  LICENSE.txt
  version.lock.json
scripts/fetch_nrd.ps1
```

### 5.3 C ABI 边界

C++ 桥只暴露 `extern "C"`、固定宽度整数、POD 数组和 opaque handle。建议最小接口：

```c
typedef struct NrdBridge NrdBridge;

NrdBridgeStatus nrd_bridge_query_version(NrdBridgeVersion* out_version) noexcept;
NrdBridgeStatus nrd_bridge_create(
    const NrdBridgeCreateDesc* desc,
    NrdBridge** out_bridge) noexcept;
NrdBridgeStatus nrd_bridge_denoise(
    NrdBridge* bridge,
    const NrdBridgeFrameDesc* frame,
    const NrdBridgeResources* resources,
    ID3D12GraphicsCommandList* command_list) noexcept;
void nrd_bridge_destroy(NrdBridge* bridge) noexcept;
size_t nrd_bridge_copy_last_error(
    const NrdBridge* bridge,
    char* destination,
    size_t capacity) noexcept;
```

具体约束：

- `NrdBridgeCreateDesc` 包含 device、resource width/height、queued frames 和 ABI version；创建时验证 `queued_frames == 3`。
- Rust 传出的 COM 指针不转移所有权。桥若跨帧持有 device，必须自行 `AddRef/Release` 或由官方 wrapper 明确持有；每帧资源指针不得保存到调用之后。
- 所有函数捕获 C++ 异常并返回错误码；任何异常/`std::string`/`std::vector` 不得跨边界。
- Rust 与 C++ 都对每个 POD 结构做 `sizeof`、`alignof`、offset 和 ABI version 测试。
- 错误字符串复制到调用者缓冲区；不得返回临时 `c_str()`。
- create 失败时 `out_bridge == NULL`，destroy(NULL) 安全。
- query_version 返回 NRD major/minor/build、锁定提交标识、normal/roughness encoding；运行时结果与 lock manifest 不一致时拒绝启用。

### 5.4 生命周期与资源代

NRD instance 必须归属于 `RenderResourceGeneration`，不能作为与尺寸无关的全局裸指针：

```rust
struct NrdGenerationResources {
    bridge: NrdBridgeHandle,
    inputs: NrdInputTextures,
    outputs: NrdOutputTextures,
}

struct RenderResourceGeneration {
    // 现有字段……
    denoiser_backend: DenoiserBackend,
    nrd: Option<NrdGenerationResources>,
}
```

- 创建 SVGF generation 时 `nrd == None`，不得为未选择的 NRD 预分配全部内部历史。
- 切到 NRD 时先完整创建同 extent 的新 generation 和 bridge，成功后才替换 active generation。
- 创建失败时保持旧 generation 和旧后端，不销毁可工作的路径；CLI 首次启动明确请求 NRD 时则返回错误。
- 旧 generation 的 NRD bridge 与纹理一起放入退休队列；只有 `completed_fence >= last_used_fence` 才调用 destroy/drop。
- `autoWaitForIdle=false`。NRD create、denoise、switch 和 generation drop 都不能隐藏 queue idle wait。
- 真正的 swapchain resize 可以沿用现有 resize 全 GPU wait；F3 和动态内部尺寸切换不允许 wait idle。
- 动态分辨率决策只有在新 NRD generation 创建成功后才 `commit_switch`。失败时不更新 controller 当前档位，错误日志限频，避免每帧重试。
- 最小实现不缓存多个 NRD extent；若 creation hitch 或显存峰值无法接受，先记录数据，不在本阶段扩成复杂 cache。

## 6. 共用重建输入契约

### 6.1 统一帧快照

新增 Rust 侧 `ReconstructionFrameState` 或等价类型，作为 NRD 适配器和未来 Streamline 适配器的唯一来源：

```rust
#[repr(C)]
struct ReconstructionFrameState {
    world_to_view: [f32; 16],
    world_to_view_prev: [f32; 16],
    view_to_clip: [f32; 16],
    view_to_clip_prev: [f32; 16],
    camera_position: [f32; 3],
    frame_index: u32,
    render_width: u32,
    render_height: u32,
    previous_render_width: u32,
    previous_render_height: u32,
    camera_jitter_px: [f32; 2],
    camera_jitter_prev_px: [f32; 2],
    reset: u32,
    delta_time_ms: f32,
}
```

矩阵必须由一个有单元测试的相机模块生成。NRD 的 C++ adapter 负责把 Rust 的明确布局转换为 NRD 所需的列向量、列主序；未来 Streamline adapter 负责转换为其 row-major contract。不得让两个 SDK 各自在桥里从 yaw/pitch 重建不同矩阵。

测试至少覆盖：

- identity；
- 纯平移；
- yaw/pitch 旋转；
- 已知世界点到 view/clip 的解析结果；
- current/previous 矩阵方向；
- Rust 数组、HLSL `mul` 顺序和 C++ NRD 数组的一致性；
- resize/history reset 时 previous 等于 current 且 reset 为真。

### 6.2 重建 guide 资源

在不破坏现有 SVGF descriptor 语义的前提下，增加或明确以下资源：

| 资源 | 建议格式 | 语义 |
| --- | --- | --- |
| `reconstruction_noisy_hdr` | `R16G16B16A16_FLOAT` | 线性 HDR、pre-exposure 明确的最终 noisy opaque color，不含 UI/ToneMap |
| `reconstruction_diffuse_albedo` | `R16G16B16A16_FLOAT` | 线性 diffuse reflectance，RGB 有效 |
| `reconstruction_specular_albedo` | `R16G16B16A16_FLOAT` | 按 NVIDIA RR 参考 EnvBRDF 计算的 specular reflectance，不能只写 metallic 或裸 F0 |
| `reconstruction_normal_roughness` | `R16G16B16A16_FLOAT` | RGB 世界空间归一化 shading normal，A 为 linear roughness |
| `reconstruction_view_z` | `R32_FLOAT` | 与 worldToView 一致的主表面 linear viewZ；miss 写无效范围外值 |
| `reconstruction_motion` | `R16G16_FLOAT` | 稠密 pixel-space surface motion，包含 camera 与 object motion；方向由类型/注释固定 |
| `reconstruction_specular_hit_distance` | `R32_FLOAT` | primary surface 到第一 specular bounce hit 的世界距离，不含 primary RayT |
| `reconstruction_primary_emissive` | `R16G16B16A16_FLOAT`（若需要） | 只存主表面 emissive，供 NRD 前后正确分离/加回 |

可以复用现有底层纹理，但前提是格式、语义和所有消费者都明确一致。不要为了减少一张纹理而让一个 alpha 同时代表 `material kind`、metalness 和 roughness。

资源必须：

- 跟随 render extent，而不是 output extent；
- 归属于 generation；
- 在 DXR 后处于可供 prep/NRD 读取的状态；
- miss、透明/不支持材质和无效像素有确定值；
- Debug 名称包含 generation id 与精确语义。

### 6.3 viewZ 与运动矢量

现有 `GBufferDepth = RayTCurrent()` 不可作为 NRD `IN_VIEWZ`。正确流程：

1. 主命中点使用当前 `world_to_view` 得到 `viewZ`。
2. 同一局部表面点通过上一帧 instance transform 得到 `previousWorldPosition`。
3. 使用上一帧 `world_to_view_prev` 和 `view_to_clip_prev` 投影到 previous UV。
4. 当前 surface motion 由 previous/current pixel position 计算，包含相机和物体运动。
5. NRD adapter 生成 `old = new + MV` 的 2.5D motion：XY 使用 normalized UV 或配合 `motionVectorScale` 的像素值，Z 为 `viewZprev - viewZ`。

现有 SVGF motion 的方向是 `current - previous`，NRD 的 XY 方向相反。最安全做法是保留现有 SVGF 纹理，使用显式函数/compute prep 生成 NRD motion。若选择统一现有纹理，必须同时修改所有消费者并用 deterministic capture 证明 SVGF 结果不变。

必须新增 CPU 解析测试：

- 静态相机、静态物体：motion 为 0。
- 相机向右平移：选定世界点的 current/previous UV 与 motion 符号吻合。
- 相机旋转：`previous_uv == current_uv + nrd_mv.xy`，误差不超过 `1e-5`。
- 物体平移、相机静止：motion 不为 0，stable surface id 不变。
- `viewZprev == viewZ + nrd_mv.z`。
- reset 帧 motion 和 scale 按 NRD reset contract 归零。

### 6.4 材质反照率与信号

- `diffuse_albedo` 和 `specular_albedo` 必须与同一主交点的 base color、metalness、roughness、NdotV 一致。
- specular albedo 使用锁定 Streamline RR 指南提供的参考 EnvBRDF 近似或等价的官方实现，并在源码旁保留来源和 NVIDIA 声明；不得自行简化为 `lerp(0.04, baseColor, metallic)` 后声称满足 RR。
- noisy HDR 仍是当前路径追踪的总线性颜色，不把 ToneMap、曝光、UI 或 debug overlay 混进去。
- primary emissive 不得被错误地除以 specular material factor。若当前 `RawSpecular` 的复合语义妨碍正确 NRD 解调，应增加独立 primary emissive guide，并在 NRD composition 后加回。
- NRD 输入使用 `NRD_MaterialFactors`、REBLUR pack/unpack helper；禁止手写看似等价但未与库编码设置联动的公式。
- 所有 noisy RGB 在进入 NRD 前保证 finite、非负。不能用全局低阈值 clamp 掩盖 firefly；发现极端值先检查 PDF、MIS 和材质解调。

### 6.5 hit distance

NRD hitT 必须遵守：

- 是 primary hit 之后第一跳的世界距离；
- 不含 camera 到 primary 的 `RayTCurrent()`；
- 不累计多段路径；
- 不除以 PDF、BRDF 或 lobe 概率；
- miss 为 0；
- 只有真正跳过的 lobe 才以 0 表示无样本；
- delta transmission 不得伪装为普通 rough specular hit。

当前路径在混合 proposal 下会同时评估 diffuse/specular BRDF 贡献，但只在 `sampledSpecular` 时写一个 hit distance。最小阶段 9 不应顺手改成另一套采样器。建议新增“第一 bounce hit distance”并在两个信号确实共享同一 continuation ray 时显式复用；同时保留现有 SVGF hit-distance 语义，避免无意改变默认输出。

如果实现者把信号改成真正的 probabilistic lobe skip：

- 跳过信号的 radiance 和 hitT 都必须为 0；
- 选择概率和 estimator 权重必须保持无偏，禁止重复除概率；
- 3x3 reconstruction 要把概率钳制到官方要求范围并使用官方 sample 的 Bayer-like 选择，不能继续用白噪声保证邻域样本；
- `HitDistanceReconstructionMode::AREA_3X3` 和非零 prepass 必须一起启用。

这条改动超出“最小后端”的推荐路线。除非现有共享 hitT 在 validation overlay 或画质门槛中失败，否则本阶段不实现 probabilistic skip 重构。

## 7. NRD 专属路径

### 7.1 输入准备

增加 `stage9_nrd_prep.hlsl` 或等价 compute pass，使用锁定版本 `NRD.hlsli`：

- `NRD_FrontEnd_PackNormalAndRoughness`；
- `NRD_MaterialFactors`；
- `REBLUR_FrontEnd_GetNormHitDist`；
- `REBLUR_FrontEnd_PackRadianceAndNormHitDist`。

推荐 NRD 外部资源：

| NRD ResourceType | 来源/目标 |
| --- | --- |
| `IN_MV` | prep 生成的 2.5D motion |
| `IN_NORMAL_ROUGHNESS` | 官方 helper 打包后的世界法线/线性 roughness |
| `IN_VIEWZ` | linear viewZ |
| `IN_DIFF_RADIANCE_HITDIST` | 解调后的 diffuse radiance + normalized hit distance |
| `IN_SPEC_RADIANCE_HITDIST` | 去除 primary emissive、解调后的 specular radiance + normalized hit distance |
| `OUT_DIFF_RADIANCE_HITDIST` | NRD generation-owned output |
| `OUT_SPEC_RADIANCE_HITDIST` | NRD generation-owned output |
| `OUT_VALIDATION` | Debug/validation 构建可选输出 |

所有格式必须从锁定版本 `NRDDescs.h`/`LibraryDesc` 验证，不能凭旧版博客或枚举顺序写死。升级 SDK 时 `ResourceType` 顺序可能变化，bridge 必须按枚举名映射，并有版本测试。

### 7.2 CommonSettings

每帧调用顺序：

```text
NewFrame
SetCommonSettings
SetDenoiserSettings(REBLUR identifier)
DenoiseD3D12
```

`CommonSettings`：

- 当前/上一帧 `viewToClip`、`worldToView` 使用非抖动矩阵和明确布局转换。
- `resourceSize == rectSize == 当前 generation render extent`；previous size 在正常连续帧为上一帧 extent，新 generation/reset 时等于 current。
- `frameIndex` 每渲染帧加 1；reset 后允许重新起序列，但不能同一帧调用多次而加多次。
- `accumulationMode` 正常为 `CONTINUE`；相机 teleport、F3、resize、render generation、shader reload、scene/model 改变时恰好一帧 `RESTART`。初始化可用 `CLEAR_AND_RESTART`，不能每帧 clear。
- `isMotionVectorInWorldSpace=false`，采用 2.5D screen motion。
- `motionVectorScale` 与实际纹理单位严格对应。若 texture 存像素值，则 XY 为 `1/width, 1/height`；Z scale 与 viewZ 单位一致。
- `cameraJitter`/Prev 为 0；当前每像素随机采样不写入该字段。
- `timeDeltaBetweenFrames` 使用渲染帧时间并钳制异常 pause；benchmark 中记录实际值。
- `denoisingRange` 小于 ray `TMax=1000` 的安全上限或显式设为 1000，sky viewZ 写到 range 外；不要沿用默认 500000 而让无效 0 深度成为有效表面。
- `enableValidation` 只在 `--debug-view nrd-validation` 或专用验证模式打开。

### 7.3 ReblurSettings

初次集成以 v4.17.3 默认 `ReblurSettings` 为基线：

- `enableAntiFirefly=true`；
- 默认 prepass blur radius；
- 当前两个信号每像素都有共享 continuation hitT 时，`hitDistanceReconstructionMode=OFF`；
- 只有实现了第 6.5 节的真正 probabilistic skip contract 才能改为 `AREA_3X3`；
- hit distance normalization parameters 必须与 HLSL prep 使用的值完全一致；
- 不做“看起来更干净”的隐藏调参。

任何非默认参数必须进入：

- CLI/常量定义；
- benchmark JSON；
- 文档表格；
- 一次独立 A/B capture。

### 7.4 调度、状态与 descriptor 恢复

调用 NRD 前，应用将 guide/input 转成 bridge 声明的 shader-resource 状态，output 转成 UAV 状态。桥必须选择以下两种可验证策略之一：

1. `restoreInitialState=false`，返回每个外部资源的准确最终状态，Rust 调用 `TrackedResource::set_known_state_after_external_recording` 或等价 API 更新追踪；或
2. bridge 在 NRD 调度尾部只把外部资源规范化到固定 after-state：所有 input/output 都为 `NON_PIXEL_SHADER_RESOURCE`，Rust 同步记录该状态。

推荐第 2 种，接口更小且 ToneMap 下一步正好读取 output。禁止：

- 让 NRD 改状态后 Rust 仍认为 output 是 UAV；
- 对状态不确定的资源硬写 transition before-state；
- 每帧用 `restoreInitialState=true` 回滚所有资源却不测额外 barrier；
- 在桥内提交 command list、Signal fence 或等待 queue。

`DenoiseD3D12` 返回后立即：

1. 重新绑定 active generation 的 CBV/SRV/UAV heap；
2. 重新绑定 sampler heap；
3. ToneMap/Compose 显式绑定自己的 root signature、PSO 和 descriptor table；
4. profiler/PIX 正确结束 NRD 区间。

NRD bridge 不拥有 command list 的 Close/Execute 生命周期。

### 7.5 输出合成

增加 `stage9_nrd_compose.hlsl` 或将明确分支加入 ToneMap 前的 compute：

- 使用相同版本 `NRD.hlsli` 的 REBLUR backend unpack；
- 用与 prep 相同的 material factors 把 diffuse/specular radiance重新调制；
- 加回独立 primary emissive；
- 输出线性 HDR，最后才进入现有曝光/ToneMap；
- 不对 NRD 输出重复运行 Temporal 或 À-Trous；
- Raw、albedo、normal、depth、motion、ID 等既有 debug view 仍查看同一语义资源；Final 才切换后端输出。

新增 `nrd-validation` debug view 时必须使用固定枚举/CLI 映射和 point/bilinear 规则，保持已有 11 个 debug view 的编号、名称和 capture metadata 不变；新项追加到末尾，不能插入中间破坏阶段 8 脚本。

## 8. GPU 计时、JSON 和诊断

Profiler 新增 NRD 或统一 Denoiser 区间时必须保持旧字段兼容：

- SVGF：`Temporal`、`Atrous` 和四轮子区间继续有效，NRD 字段为 `null`/inactive。
- NRD：新增 `NrdPrep`、`Nrd`、`NrdCompose`（可以只对外聚合 `Nrd`，但 PIX 中应可分辨）；旧 SVGF 子区间为 `null`/inactive。
- `Total` 始终覆盖 AS、Path Trace、当前选择的完整 denoiser、ToneMap。
- profiler 用 active-pass mask，不为未执行的 pass 伪造 0 ms 样本，也不能因为 inactive query 未写入而让整个 Total 样本失效。
- 动态分辨率仍只消费 fence 完成、generation 匹配的 Total。

benchmark JSON 至少新增：

```json
{
  "denoiser": {
    "requested": "nrd-reblur",
    "active": "nrd-reblur",
    "nrd_compiled": true,
    "nrd_version": "4.17.3",
    "nrd_commit": "<full commit>",
    "history_reset_count": 0,
    "switch_count": 0
  },
  "passes": {
    "nrd": { "p50_ms": 0.0, "p95_ms": 0.0, "valid_samples": 0 }
  }
}
```

现有顶层字段不得改名。若升级 schema，写 `schema_version` 并让旧 runner 明确拒绝不兼容 schema，而不是错读。

稳定诊断至少包括：

- `denoiser_switch from=svgf to=nrd-reblur generation=... history_reset=1 idle_waits=0`；
- `denoiser_switch blocked=...`；
- `nrd_generation_create status=... extent=... version=...`；
- `nrd_dispatch_failed status=... frame=...`，错误限频；
- 窗口标题显示后端和 NRD GPU 时间，不把 NRD 时间写到 À-Trous 标签下。

## 9. 工作包与推荐提交

每个工作包必须单独 review。可以拆得更细，但不能把全部阶段塞进一个提交。

### 9A：后端模型、可选构建和依赖锁

内容：

- `DenoiserBackend`、CLI、默认值、help、测试和 benchmark 元数据占位。
- `nrd` Cargo feature；feature off 完全不触碰 NRD 工具链。
- `third_party/nrd` 许可/版本 manifest/README。
- 幂等 `scripts/fetch_nrd.ps1` 和 `.gitignore external/`。
- C ABI header、layout/version smoke，不创建实际 denoiser也可。

验收：

- 默认 `cargo test --all-targets`、Clippy、Debug/Release build 通过。
- `--denoiser nrd-reblur` 在 feature off 给出准确错误。
- 获取脚本的 PowerShell AST parse 通过；错误 commit 测试拒绝继续。

建议提交：

```text
feat(stage9): add optional reconstruction backend contract
build(stage9): pin NRD dependency and bridge feature
```

### 9B：共用 Reconstruction Guides

内容：

- 完整矩阵快照、viewZ、dense motion、diffuse/specular albedo、noisy HDR、spec hit distance 和必要的 primary emissive。
- 资源归属 generation，descriptor 与 barrier 完整。
- 解析单元测试和追加 debug views/validation capture。
- SVGF 默认 final deterministic capture 与 9A 基线一致；允许新增 debug 资源，但不能改变默认光照算法。

验收：

- motion/viewZ/matrix 测试全部通过。
- 静态相机静态场景 motion 为零；动画模型有稳定非零 motion。
- guide 无 NaN/Inf，sky 值正确。
- SVGF Final、Raw、现有 11 个 debug view 的固定 seed capture 不出现语义漂移；任何像素差必须解释。

建议提交：

```text
feat(stage9): add shared reconstruction guides
```

### 9C：NRD/NRI C++ 桥

内容：

- 用本地锁定源码构建 NRD v4.17.3 + NRI D3D12 integration。
- 实现 create/query/denoise/destroy/error C ABI。
- normal/roughness encoding 和 NRD 版本运行时校验。
- no exception/allocator crossing、COM lifetime 和 layout tests。
- 一个不接 renderer 的 bridge smoke：创建 device 上下文后可 create/destroy，或在 renderer init 中 feature-gated 验证。

验收：

- `cargo build --features nrd` 的 Debug/Release 通过。
- feature off 仍不需要 SDK。
- bridge create/destroy 不等待 GPU、不泄漏、不产生 Debug Layer 错误。

建议提交：

```text
feat(stage9-nrd): add versioned D3D12 C ABI bridge
```

### 9D：REBLUR 输入、调度和合成

内容：

- NRD prep/compose shader。
- `REBLUR_DIFFUSE_SPECULAR`、ResourceSnapshot、CommonSettings、ReblurSettings。
- resource states、descriptor heap 恢复和 profiler/PIX。
- CLI 启动 NRD 固定尺寸路径；本包可以暂不开放 F3 和动态分辨率。

验收：

- 1280×720 固定尺寸 3 秒 smoke，Final 非黑、finite、退出码 0。
- Debug + GPU Validation 短运行无 InfoQueue 错误。
- NRD validation overlay 可查看。
- Raw 信号不因选择 NRD 而变化；只有 denoised Final 分支不同。

建议提交：

```text
feat(stage9-nrd): dispatch REBLUR diffuse and specular
```

### 9E：运行时切换与 generation 生命周期

内容：

- F3 切换。
- 同 extent 新 generation、历史 reset、失败原子性、fence 退休。
- resize、F2 fixed scale、dynamic-resolution 下 NRD generation recreate。
- switch/create/retire/idle-wait 计数和 JSON。

验收：

- `svgf -> nrd -> svgf` 每次只 reset 一次，旧代最终退休，idle wait 增量为 0。
- NRD dynamic forced 3 秒 smoke 至少发生一次 generation switch，决策/代际/历史计数对账。
- resize 允许现有全 GPU wait，但恢复后 backend 不变、画面有效。
- NRD generation 创建失败保留旧可用画面，不提交错误 controller 状态。

建议提交：

```text
feat(stage9): switch denoisers with fence-retired generations
```

### 9F：短矩阵、文档和债务收口

内容：

- 更新 README、主方案阶段 9 状态、第三方通知。
- 新增 `scripts/stage9_acceptance.ps1`，每个子进程有 timeout，保存 args/stdout/stderr/exit/JSON/hash/environment。
- 运行本节的短矩阵，保存 raw 结果；不运行 600/1800 秒测试。
- 明确 PASS/FAIL/BLOCKED，不能把人工画质或未运行项目写成 PASS。

建议提交：

```text
test(stage9): add bounded NRD acceptance evidence
docs(stage9): record minimal NRD backend results
```

## 10. 验证矩阵

### 10.1 每个提交的静态检查

```powershell
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --locked
cargo build --release --locked
git diff --check
```

9C 之后在本机 NRD 源码可用时追加：

```powershell
cargo test --all-targets --features nrd --locked
cargo clippy --all-targets --features nrd --locked -- -D warnings
cargo build --features nrd --locked
cargo build --release --features nrd --locked
```

### 10.2 最小运行检查

每条最长 3 秒 benchmark：

```powershell
.\target\release\ray_tracing_demo.exe `
  --benchmark-seconds 3 `
  --output-size 1280x720 `
  --denoiser svgf

.\target\release\ray_tracing_demo.exe `
  --benchmark-seconds 3 `
  --output-size 1280x720 `
  --denoiser nrd-reblur

.\target\release\ray_tracing_demo.exe `
  --benchmark-seconds 3 `
  --output-size 1920x1080 `
  --dynamic-resolution `
  --target-gpu-ms 4.0 `
  --denoiser nrd-reblur
```

### 10.3 Release 性能短矩阵

每个配置三次、每次 10 秒；不允许 cherry-pick 最好的一次，只报告三次 raw 和 median：

| 后端 | output | render mode | 场景 |
| --- | --- | --- | --- |
| SVGF | 1280×720 | fixed 1.0 | Cornell |
| NRD | 1280×720 | fixed 1.0 | Cornell |
| SVGF | 1920×1080 | fixed 1.0 | Cornell |
| NRD | 1920×1080 | fixed 1.0 | Cornell |
| NRD | 1920×1080 | dynamic target 4 ms | Cornell |
| NRD | 1280×720 | fixed 1.0 | animated glTF fixture |

断言：

- Total、当前 active denoiser、Path Trace、ToneMap 的 p50/p95 finite 且样本数一致。
- inactive pass 为 null，不伪造 0。
- Total p95 ≤ 16.67 ms。
- `gpu_idle_wait_count == 0`，resize case 除外。
- generation create/switch/retire、history reset 和动态 decision 对账。
- 显存 query 有效、peak/budget < 70%，结束时没有无 fence 保护的 retired generation。

### 10.4 正确性和画质

自动 capture 使用短、可复现配置，不以单张截图证明时域正确：

- 固定 Cornell：Raw、Final、normal/roughness、viewZ/depth、motion、spec hit distance、NRD validation。
- 静态相机连续两张 NRD Final，计算 changed pixels、RMSE 和亮度统计；不要求完全相同，但不得出现全屏随机重置。
- 相机平移/旋转：motion 方向 debug 与解析测试一致。
- animated `NonIndexedMultiNode.gltf`：物体边缘没有明显旧轮廓。
- 灯附近和镜面方块：观察高方差是否被稳定处理，不能只检查均值。
- SVGF 9A 与 9B/9F deterministic capture 对比，证明默认路径未被 NRD feature 改写。

人工项目只标记 `PENDING MANUAL`：

- 实际按 F3 往返切换；
- F1 全部视图；
- F2 fixed scale；
- resize、最小化/恢复；
- shader hot reload；
- 连续相机移动下的拖影、闪烁、灯周围噪声和镜面稳定性。

### 10.5 Debug Validation

Debug + D3D12 GPU-Based Validation 只做有界运行，不做性能判断：

- SVGF static 1 次；
- NRD static 1 次；
- NRD animated glTF 1 次；
- NRD forced dynamic 1 次；
- NRD validation capture 1 次。

每个进程设置墙钟 timeout，保存完整 InfoQueue raw。`exit 0` 但存在 D3D12 Error/Corruption 仍为 FAIL。

## 11. Review 高风险清单

Codex review 必须逐项检查：

1. 是否真的锁定 v4.17.3 完整 commit，以及 NRI v179、MathLib v11、ShaderMake 固定 commit/hash，而不是 master/latest 或在线 FetchContent。
2. feature off 是否完全不需要 NRD、CMake 或网络。
3. `build.rs` 是否偷偷下载依赖或修改源码目录。
4. C ABI 是否有 Rust/C++ bool、enum、packing、exception、string 或 allocator 越界。
5. NRD bridge 是否保存了每帧纹理裸指针。
6. motion 符号是否满足 `previous_uv = current_uv + mv`；现有 SVGF 方向是否被误改。
7. `RayTCurrent()` 是否仍被错误传成 viewZ。
8. 矩阵是否因 row/column-major 或 `mul` 顺序被转置两次。
9. specular albedo 是否偷懒写成 F0，primary emissive 是否被错误解调。
10. hitT 是否包含 primary 距离、累加路径长度、除 PDF，或只因未写入而为 0。
11. HLSL pack/unpack 和 host Reblur hit-distance parameters 是否完全一致。
12. probabilistic reconstruction mode 是否与实际信号零值语义、Bayer pattern 和 prepass 匹配。
13. NRD 是否在同帧后又执行 SVGF，或 output 被重复 ToneMap。
14. NRD 调度后 descriptor heap、root signature 和资源状态是否恢复。
15. bridge/generation drop 是否在 fence 完成前发生，是否隐藏 wait idle。
16. dynamic switch 创建失败是否错误提交 controller state。
17. profiler 是否因 inactive pass 导致 Total 样本失效，或把 inactive 写成假 0。
18. benchmark JSON 是否仍兼容阶段 8 脚本，证据是否包含实际 EXE/commit/environment。
19. 是否运行了被禁止的 600/1800 秒长测，或把没跑的人工项目写成 PASS。
20. 是否顺手加入 Streamline/DLSS/RELAX/SH 等阶段外内容。

## 12. 交给 Luna 的提示词

```text
你在 C:\zjk\projects\RayTracingDemo 仓库实现阶段 9。先完整阅读：

1. docs/阶段9重建输入契约与NRD最小后端执行方案.md
2. docs/实时DXR渲染器实施方案.md 中阶段 9、10、11
3. docs/阶段8G总体验收记录.md
4. 当前 src/renderer/d3d12.rs、src/renderer/d3d12/render_resources.rs、
   src/renderer/d3d12/profiler.rs、src/realtime/windows_app.rs、build.rs、
   shaders/stage3_triangle.hlsl、stage6_temporal.hlsl、stage6_tonemap.hlsl

严格按方案的 9A→9F 顺序实现，每个工作包完成静态检查后单独 commit，至少拆成：

- 后端/可选构建/依赖锁；
- 共用 reconstruction guides；
- NRD C ABI bridge；
- REBLUR prep/dispatch/compose；
- F3 与 generation 生命周期；
- 短验收证据和文档。

核心范围：建立以后可复用给 DLSS RR 的 noisy HDR、diffuse/specular albedo、
world normal + linear roughness、linear viewZ、dense motion、specular hit distance、
当前/上一帧非抖动矩阵和 reset contract；然后只接
NRD v4.17.3 的 REBLUR_DIFFUSE_SPECULAR 最小后端。

依赖规则：锁定官方 tag v4.17.3，首次 fetch 后记录完整 commit（必须以前缀
792eff1 开头）、NRI v179、MathLib v11、ShaderMake 固定 commit/归档 hash 和各许可 hash。
把 FetchContent 的三个 source dir 指向 fetch 脚本准备的本地目录并启用 fully-disconnected；不要使用
master/latest，不要猜完整 hash，不要让 build.rs 下载网络内容，不要提交上游源码或生成 SDK。默认 feature 必须为空；
没有 NRD SDK 时基础 cargo build/test 和默认 SVGF 必须正常。

使用官方 NRDIntegration + NRI D3D12 路径和 C++ extern "C" POD/opaque handle 桥。
不得让 exception、std::string、allocator 或 COM 所有权穿过 FFI。NRD instance 必须属于
RenderResourceGeneration，F3 和动态内部尺寸切换创建新代并按 fence 退休旧代，
autoWaitForIdle=false，不得在切换中 wait_for_gpu。

不要直接把当前 GBufferDepth/RayTCurrent 当 IN_VIEWZ。当前 motion 是
current-previous pixels，而 NRD 需要 old=new+MV；必须用解析测试证明 motion 的符号、
单位和 viewZprev-viewZ。矩阵必须明确 Rust/HLSL/NRD 的布局转换。不要把每像素随机
jitter 当成 camera jitter，阶段 9 传 0。

NRD 输入/输出必须用锁定版本 NRD.hlsli 的 material factors、normal/roughness pack、
REBLUR hit-distance normalization 和 pack/unpack helper。primary emissive 不得被错误地
当 specular material 解调。specular albedo 不能偷懒等于裸 F0，要按方案引用 NVIDIA RR
指南的 EnvBRDF 参考。不要通过 clamp 隐藏 NaN、firefly 或 PDF 错误。

初次 REBLUR 使用默认设置和 enableAntiFirefly=true。当前最小路线保留现有混合 proposal
与共享 continuation hitT，hitDistanceReconstructionMode=OFF。除非你先完整实现并证明方案
6.5 的 probabilistic skip、无偏权重、Bayer-like 邻域保证和 prepass，否则不要打开 AREA_3X3，
也不要顺手重写采样器。

NRD 调度后必须恢复应用 shader-visible CBV/SRV/UAV heap、sampler heap、root signature/PSO，
并让 TrackedResource 的状态与实际状态一致。bridge 不 Close/Execute command list，
不 Signal/Wait fence。

CLI 是 --denoiser svgf|nrd-reblur，默认 svgf；F3 只切换实际可用后端。
benchmark/capture/title 记录 requested/active backend、NRD version/commit、generation、history
reset、idle wait、显存和 NRD GPU 时间。inactive profiler pass 用 null/active mask，不能伪造 0，
也不能让 Total 样本失效。保持阶段 8 旧 JSON 字段兼容。

不要加入 Streamline、DLSS、RR、RELAX、SIGMA、SH、ReSTIR 或 UI framework，不要删除 SVGF，
不要把 NRD 设为默认，不要更改/放宽阶段 8 门槛。

每个 commit 至少运行 cargo fmt check、cargo test --all-targets --locked、clippy -D warnings、
Debug/Release build 和 git diff --check；9C 后另跑 --features nrd。运行时先做 3 秒 smoke，
最终按文档只做每进程最多 30 秒的短矩阵。严禁运行 600 秒或 1800 秒测试。

保存所有 raw args/stdout/stderr/exit/JSON/hash/environment。未运行的人工 F1/F2/F3、resize、
最小化、hot reload 和画质观察必须写 PENDING MANUAL，不能写 PASS。

遇到 NRD SDK/许可/完整 commit 不可验证、C ABI 无法安全落地、输入契约不明确或必须扩大范围时，
停止对应工作包，保留默认构建可用并报告，不要自行猜测。完成后给出 commit 列表、逐包改动、
测试命令与结果、未完成项和已知风险，等待 Codex review。
```
