# 阶段 10：DLSS Super Resolution 与 Reflex 执行方案

日期：2026-08-10
目标硬件：NVIDIA GeForce RTX 4060 Laptop GPU
目标平台：Windows 10/11、D3D12、MSVC x64
执行者：Luna 负责实现，Codex 负责逐提交 review 和最终验收
参考 SDK：NVIDIA Streamline v2.12.0 release

## 1. 当前结论与阶段边界

阶段 9 的 NRD 工程路径、短矩阵和交互生命周期已经可用；连续动态画质仍由用户人工
签收，但不阻塞本阶段。本阶段只完成以下四项：

1. 可选 Streamline 基础设施和明确的能力探测；
2. DLSS Super Resolution 与 DLAA；
3. Reflex Low Latency 和 PCL latency markers；
4. RTX 4060 Laptop 上的有界正确性、画质、性能和生命周期验收。

以下内容明确不属于阶段 10：

- DLSS Ray Reconstruction；
- DLSS Frame Generation、Multi Frame Generation 和 Reflex 2 Frame Warp；
- 用 DLSS SR 替换 SVGF 或 NRD 的降噪职责；
- 自动下载未锁版本的 SDK，或把网络访问隐藏在 `build.rs` 中；
- 600/1800 秒长测；
- 为追逐性能数字修改路径追踪、NRD 或 SVGF 的采样算法。

职责必须保持正交：

```text
Path Trace → SVGF 或 NRD → HDR 合成 → DLSS SR/DLAA → ToneMap → swap chain
                降噪器                    超分器
```

DLSS Ray Reconstruction 将在阶段 11 作为第三个重建/降噪后端评估。NRD 仍有必要保留，
因为它是无 DLSS RR 时的独立后端、质量基线和非 RR 运行模式的回退路径。

## 2. 不可破坏的基线

- 默认 Cargo feature 仍为空；默认运行仍是 `SVGF + Native`。
- 缺少 Streamline SDK、DLL 或 NVIDIA 支持时，feature-off 构建和运行不得受影响。
- `nrd` 与 `streamline` 必须正交，四个组合都能构建：default、nrd、streamline、
  nrd+streamline。
- `--denoiser svgf|nrd-reblur` 只选择降噪器；不得借此隐式选择 DLSS。
- 原生 Native 路径在 feature-off 和 feature-on-but-native 两种情况下都保持相同资源、
  shader 和像素输出，不允许多一次无意义的 resample。
- 所有模式切换、resize、最小化/恢复都使用现有 fence/generation 生命周期；正常帧和
  F4 切换不得 `wait_for_gpu`。
- 不把 CPU 墙钟或窗口 FPS 冒充 DLSS GPU 成本；新增 pass 使用 GPU timestamp。
- SDK 返回错误必须有稳定日志和结构化 JSON，不得静默回落到另一个画质模式。

## 3. 官方资料与版本锁

实现前先完整阅读锁定版本的下列官方资料，不要依据博客、旧 sample 或记忆猜接口：

- [Streamline v2.12.0 release](https://github.com/NVIDIA-RTX/Streamline/releases/tag/v2.12.0)
- [Streamline Programming Guide](https://github.com/NVIDIA-RTX/Streamline/blob/v2.12.0/docs/ProgrammingGuide.md)
- [DLSS Programming Guide](https://github.com/NVIDIA-RTX/Streamline/blob/v2.12.0/docs/ProgrammingGuideDLSS.md)
- [Reflex Programming Guide](https://github.com/NVIDIA-RTX/Streamline/blob/v2.12.0/docs/ProgrammingGuideReflex.md)
- [Manual Hooking Guide](https://github.com/NVIDIA-RTX/Streamline/blob/v2.12.0/docs/ProgrammingGuideManualHooking.md)

版本策略：

- 锁定 release `v2.12.0`，同时在 lock 文件记录解包后的完整 Git commit、release asset
  文件名、下载 URL、archive SHA-256、关键 DLL SHA-256 和许可文件 SHA-256。
- Streamline 自 v2.7.32 起不再把二进制 artifact 放在源码仓库；必须使用官方 release
  asset。不要从 tag source ZIP 假装取得可部署 DLL。
- 本文不预填未经本机下载验证的 asset 文件名或 hash。Luna 首次执行官方 fetch 后记录
  真实值；不得编造 SHA-256。
- Release 运行只部署 NVIDIA 签名的 production DLL。Debug 可部署 development DLL，
  但窗口标题、日志和 JSON 必须明确 `development=true`，严禁将其混入 Release。
- 复制全部适用的 LICENSE/NOTICE；缺任一文件时 feature 构建应给出可操作错误。

## 4. 目标架构

### 4.1 Feature 与模块

在 `Cargo.toml` 新增独立 feature：

```toml
[features]
default = []
nrd = []
streamline = []
```

建议模块和目录：

```text
third_party/streamline/lock.json
scripts/fetch_streamline.ps1
native/streamline_bridge/CMakeLists.txt
native/streamline_bridge/include/streamline_bridge.h
native/streamline_bridge/src/streamline_bridge.cpp
native/streamline_bridge/src/streamline_bridge_smoke.cpp
src/upscaler.rs
src/streamline.rs                       # 仅 feature=streamline
src/renderer/d3d12/streamline.rs        # D3D12 资源、tag、evaluate、状态恢复
shaders/stage10_dlss_input.hlsl
scripts/stage10_acceptance.ps1
docs/阶段10验收记录.md
```

若现有模块边界更适合别的命名，可以小幅调整，但 C ABI、Rust 策略层和 renderer 资源层
必须分开，不能把整个 SDK API 堆进 `d3d12.rs`。

### 4.2 帧顺序

最终帧顺序固定为：

1. 获取唯一 Streamline frame token；
2. `slReflexSleep`；
3. PCL `SimulationStart`；
4. 更新相机、动画、非抖动矩阵、全局 projection jitter 和 DLSS motion；
5. PCL `SimulationEnd`、`RenderSubmitStart`；
6. AS、PathTrace、SVGF/NRD；
7. Final 视图且启用 DLSS 时，合成 render-resolution HDR input；
8. 提交 constants、options、frame-based resource tags，调用 DLSS evaluate；
9. 重新绑定被 Streamline 改写的 D3D12 command-list 状态；
10. ToneMap 到 output-resolution `display_output`；
11. close/execute command list，PCL `RenderSubmitEnd`；
12. PCL `PresentStart`，`Present`，PCL `PresentEnd`；
13. manual-hooking 路径按官方 v2.12.0 sample 确保 `presentCommon()` 恰好一次；
14. signal fence、提交 frame 统计。

同一帧的 token、viewport id 必须贯穿 constants、options、tags、evaluate 和 Reflex/PCL。
不能在其中任何一步重新获取 token。

## 5. 工作包 10A：锁定 SDK 和可选构建

### 实现

1. 新增 `third_party/streamline/lock.json`，schema 至少包含：
   `version`、`tag`、`commit`、`asset_name`、`asset_url`、`archive_sha256`、
   `files[] { path, sha256, purpose }`、`licenses[]`。
2. 新增显式执行的 `scripts/fetch_streamline.ps1`：
   - 只接受 lock 中的 HTTPS GitHub release URL；
   - 下载到临时目录，先校验 archive hash 再解包；
   - 检查目录逃逸、关键头文件、lib、production/development DLL 和许可；
   - 原子移动到 `external/streamline-v2.12.0/`；
   - 已存在且 hash 全匹配时幂等退出；
   - 不删除或覆盖不匹配目录，给出明确修复提示。
3. `build.rs` 仅在 `CARGO_FEATURE_STREAMLINE` 时验证本地 SDK、构建 bridge、链接并部署；
   feature-off 不读取 SDK 路径，也不复制 Streamline DLL。
4. 支持 `STREAMLINE_SOURCE_DIR` 覆盖默认目录，并加入 `rerun-if-env-changed`。
5. Debug/Release 选择匹配的 lib 和 DLL；DLL 复制到 Cargo profile 可执行目录，策略与
   WinPix/NRD notice 部署一致。
6. CMake 保持 `FETCHCONTENT_FULLY_DISCONNECTED` 或等价策略；禁止构建期联网。

### 验收

- `cargo build --locked` 在完全没有 Streamline 目录时通过。
- `cargo build --features streamline --locked` 缺 SDK 时快速失败，错误包含 fetch 命令。
- fetch 二次运行不改文件；篡改 archive 或关键 DLL 时 hash 校验失败。
- Release 目录只有 production DLL；Debug 目录只有 development DLL。
- 不提交 release ZIP、DLL 或巨大 SDK 树，除非许可证和仓库策略明确允许；默认只提交
  lock、fetch 脚本和许可部署逻辑。

### 建议提交

`build(stage10): pin optional Streamline SDK`

## 6. 工作包 10B：C++ C ABI 与生命周期

### ABI 规则

- 导出函数全部 `extern "C"`、`noexcept`，内部捕获全部异常。
- 不跨边界传递 STL、C++ 引用、异常、allocator 或 COM 智能指针。
- Rust 传入的 D3D12/DXGI 指针默认是借用；bridge 若 AddRef 必须在文档和 destroy 中对称。
- C struct 使用固定宽度整数、显式 `struct_size`/`version`，Rust `#[repr(C)]` 对照测试
  size、align 和 offset。
- 所有返回值映射成稳定 bridge result code，并允许取得短错误字符串。

### 最小导出面

bridge 至少封装以下能力，不要求导出和 SDK 同名的薄壳：

- init/shutdown、设置 D3D12 device、取得 adapter LUID；
- 查询 DLSS、Reflex、PCL support/requirements；
- upgrade/unupgrade swapchain，明确返回后的 COM 所有权；
- `slDLSSGetOptimalSettings`；
- viewport create/free；
- `slGetNewFrameToken`；
- set constants/options/tags/evaluate DLSS；
- set/get Reflex options、sleep、PCL markers；
- manual hooking 所需的 per-frame present common；
- SDK/version/plugin/DLL flavor 诊断。

初始化使用 `PreferenceFlag::eUseManualHooking` 和 frame-based resource tagging；
`featuresToLoad` 只列 DLSS、Reflex、PCL，不加载 RR/FG。建议在创建 DXGI/D3D12 之前
`slInit`，device 成功后 `slSetD3DDevice`，swapchain 创建后通过 `slUpgradeInterface`
取得升级接口。若 locked guide 对具体顺序有更严格要求，以 v2.12.0 官方文档为准并在
代码注释写明章节。

shutdown 顺序必须是：停止提交新帧 → 等待现有 frame fence → free viewport resources →
unupgrade/release swapchain → Streamline shutdown → 释放 device/DXGI。不得依赖 Rust/C++
静态析构顺序。

### 验收

- bridge smoke 能在 RTX 4060 Laptop 上打印 v2.12.0、adapter LUID 和三项 support 状态。
- 重复 init/destroy、unsupported feature 和空指针都返回稳定错误，不崩溃。
- `presentCommon` 计数严格等于实际 Present 帧数；resize/minimize 不造成增长失配。
- Debug Layer/InfoQueue 无 ERROR/CORRUPTION。

### 建议提交

`feat(stage10): add Streamline D3D12 bridge`

## 7. 工作包 10C：DLSS 帧输入契约

这是本阶段最重要的正确性包。不要直接把 Stage 9 NRD motion 的 XY 当作 DLSS motion，
除非用锁定 sample 和解析测试证明方向、单位、抖动约定完全一致。

### 模式和 extent

新增稳定枚举：

```text
native
dlaa
dlss-quality
dlss-balanced
dlss-performance
```

新增 CLI `--upscaler <模式>`，默认 `native`。DLAA 的 render extent 等于 output extent；
DLSS 各模式的 render extent 必须来自 `slDLSSGetOptimalSettings`，禁止硬编码 0.67、0.58
等经验比例。记录 optimal、min、max 和 sharpness 建议值。

第一版中 `--dynamic-resolution` 与非 Native upscaler 显式互斥，并给出清晰错误；F2 在
DLSS/DLAA 激活时输出 `render_scale_manual_change blocked=upscaler_owned_extent`。在固定
optimal 模式稳定前，不要把现有动态控制器强行塞入 SDK min/max。若 10E 后续确需兼容，
必须作为独立提交实现并单独 review。

### 矩阵、抖动和运动矢量

- `sl::Constants` 的矩阵使用 row-major，且 projection/view 矩阵不包含 jitter。
- jitter 以 pixel-space 独立字段传入；current/previous 都由单一帧状态保存。
- 引入每帧一个确定性 global projection jitter，用锁定 v2.12.0 sample 的推荐序列和相位
  算法；不要把现有每像素随机采样偏移冒充 camera jitter。
- primary ray 使用 global projection jitter；BSDF/light 随机维度仍保持现有序列。
- 新增 DLSS 专用 dense `RG16F` 或 `RG32F` motion 资源，覆盖相机和刚体动画。
- 若 motion 以 pixels 存储，提交 `mvecScale={1/renderWidth,1/renderHeight}`；方向和
  `motionVectorsJittered` 必须由官方约定及测试锁定，不凭 Stage 9 的注释猜测。
- camera/animation、F3、F4、resize、render extent、shader reload、debug Final 往返等
  discontinuity 都显式 `reset=true`，同时重置 jitter phase 和历史。

必须增加 CPU 解析测试：静态相机/物体、纯相机平移、纯旋转、物体刚体移动、projection
jitter、reset 和 extent 改变。至少选三个世界点，用当前和上一帧矩阵解析计算预期屏幕
位移，与 shader/bridge 约定比较；容差和正负方向写死在测试中。

### 资源与颜色

- `dlss_input_hdr`：render extent、`RGBA16_FLOAT`、linear HDR、pre-exposed 约定明确；
- `dlss_output_hdr`：output extent、`RGBA16_FLOAT`；
- `dlss_depth`：render extent 的 2D `R32_FLOAT` 深度适配资源，格式、数值空间和 clear
  value 必须匹配锁定 sample。若官方要求 device/NDC depth，就从 jittered primary hit
  经 projection 转换，并正确设置 near/far、`depthInverted`；除非 v2.12.0 文档明确
  允许，否则不得把线性 NRD viewZ 直接作为 DLSS depth；
- `dlss_motion`：render extent 的 dense 2D motion；
- `dlss_exposure`：1×1 显式 exposure=1.0，当前固定曝光下使用
  `useAutoExposure=false`、`preExposure=1`，避免不同运行的自动曝光漂移。

新增 `stage10_dlss_input.hlsl`，只在 Final+DLSS 模式合成当前 active denoiser 的 diffuse
和 specular 到 `dlss_input_hdr`。Native 继续使用现有 ToneMap 两路 SRV，不经过此 shader。
所有 HLSL 同时加入 build-time compile 和 Debug hot reload；任何一条 pipeline 创建失败时
保持旧 pipeline，禁止半更新。

### 建议提交

`feat(stage10): define DLSS frame input contract`

## 8. 工作包 10D：DLSS evaluate 与显示链

每帧严格执行：

1. `slDLSSGetOptimalSettings` 只在模式/output extent 变化时查询，不要每帧重建资源；
2. `slSetConstants`、`slDLSSSetOptions` 使用同一 frame token/viewport；
3. 用 `slSetTagForFrame` 标记 input color、output color、depth、motion、exposure；
4. tag 中携带真实 D3D12 resource state 和完整 extent/subresource；
5. 显式 transition 后调用 `slEvaluateFeature(kFeatureDLSS, ...)`；
6. evaluate 后恢复 descriptor heaps、compute root signature、root tables/descriptors/
   constants 和 PSO；不要假设 Streamline 保留 app command-list state；
7. 将 output 转为 ToneMap SRV，再写 output-resolution `display_output`。

F1 非 Final debug view 默认绕过 DLSS，继续用原生最近邻/现有 debug 映射展示输入数据；
UI/调试文字绝不作为 DLSS input。返回 Final 时 reset DLSS history。当前程序没有独立 UI
composite，因此此策略等价于“DLSS 只处理场景 HDR，显示层在其后”。以后增加 UI 时也
必须保持此顺序。

扩展 `GpuPass`：`DlssCompose`、`DlssEvaluate`。inactive pass 在 JSON 为 `null`，active
sample 数必须与 Total 对齐。DLSS evaluate 的 GPU timestamp 必须覆盖 SDK 记录到同一
command list 的工作。

### 建议提交

`feat(stage10): evaluate DLSS Super Resolution`

## 9. 工作包 10E：事务性模式切换和资源退休

F4 循环：

```text
native → dlaa → dlss-quality → dlss-balanced → dlss-performance → native
```

切换必须先查询 support/optimal settings，再完整创建新 generation 和 SDK viewport，成功
后才替换 active。失败保留旧模式、旧 extent 和旧 history。每个 generation 使用唯一
viewport id；旧 viewport 和它的 DLSS input/output/depth/motion/exposure 资源按该代最后
提交 fence 退休，fence 完成后才调用 free resources。若 SDK 明确不允许并存 viewport，
先停止实现并把官方限制交给 Codex，不得偷偷改成每次 F4 全 GPU idle。

resize 可以沿用现有有界 output resize 等待，但必须：

- minimized 时不 evaluate、不 free active viewport；
- restore/resize 查询新的 optimal settings并事务性建代；
- `presentCommon` 仍和实际 Present 一一对应；
- old extent 的 tags 不得进入 new viewport frame；
- swapchain upgrade/unupgrade 引用计数对称。

稳定日志：

```text
upscaler_switch from=<...> to=<...> generation=<n> viewport=<id> \
output=<WxH> render=<WxH> history_reset=1 idle_waits=0 retire_fence=<n>
```

显式请求不支持模式时启动失败，不静默退回 Native。若需要回退，后续可另加
`--allow-upscaler-fallback`；本阶段不要默认启用。

### 建议提交

`feat(stage10): switch DLSS modes with retired viewports`

## 10. 工作包 10F：Reflex 与 PCL

新增模式：`off`、`on`、`on-boost`，CLI 为 `--reflex-mode`。在支持硬件且 Streamline
启用时默认 `on`；feature-off 或不支持时为 `unavailable`，不能伪报已启用。

按官方 v2.12.0 要求，即使 mode 为 off，只要 Reflex/PCL supported，也要保持正确的
`slReflexSleep` 和 PCL markers 调用。marker 放置：

- `SimulationStart`：相机/动画和帧常量更新前；
- `SimulationEnd`：上述 CPU simulation 完成后；
- `RenderSubmitStart`：开始记录本帧 GPU 工作前；
- `RenderSubmitEnd`：ExecuteCommandLists 后；
- `PresentStart`：Present 前；
- `PresentEnd`：Present 返回后。

`slReflexSleep` 放在 simulation 开始前，每个实际渲染帧一次；minimized、pending capture
只轮询而未提交帧时不得生成一套假的 markers。可选实现鼠标左键 latency ping，但不是
完成阶段的硬门槛。

JSON 至少报告：support、requested/active mode、sleep call count、每类 marker count、
token count、presentCommon count、顺序错误数、SDK report 是否可用。只有通过 NVIDIA
Reflex Verification HUD/FrameView 才能宣称 latency 改善；普通日志只能证明 marker
完整性。

### 建议提交

`feat(stage10): add Reflex markers and telemetry`

## 11. 工作包 10G：CLI、标题、capture 与 hot reload

- 窗口标题升级为阶段 10，显示 denoiser、upscaler、Reflex、output/render extent 和
  DLSS evaluate GPU ms，但避免塞入全部 SDK 诊断。
- benchmark/capture JSON 增加 `streamline`、`upscaler`、`reflex` 对象和 SDK/provenance；
  feature-off 字段稳定存在但为 `compiled=false`/`null`，便于脚本比较。
- capture metadata 记录 upscaler requested/active、output/render extent、SDK version、
  viewport generation、reset 和 denoiser。
- F4、resize、F1 debug→Final、F3 denoiser切换都能正确 reset DLSS history。
- `stage10_dlss_input.hlsl` 纳入 Debug reloader 的完整 pipeline transaction；reload 后
  DLSS generation 不需要立即销毁，但下一帧必须 reset history。
- 帮助文本清楚说明 `--render-scale`/`--dynamic-resolution` 与 DLSS 的互斥规则。

### 建议提交

`feat(stage10): expose DLSS and Reflex runtime controls`

## 12. 工作包 10H：有界验收基础设施

新增 `scripts/stage10_acceptance.ps1`，沿用阶段 9 的 run-id、raw args/stdout/stderr、
environment、summary schema、child timeout 和 self-auth provenance。默认不得跑长测。

### 自动化矩阵

1. 静态检查：
   - `cargo fmt --all -- --check`
   - default 与 all-features 的 tests/clippy
   - 四种 feature 组合 build
   - PowerShell AST parse
   - `git diff --check`
2. Release 1 秒 smoke，1920×1080 output：
   - Native+SVGF
   - DLAA+SVGF
   - Quality+SVGF
   - Balanced+SVGF
   - Performance+SVGF
   - Quality+NRD
3. 每个子进程有 60 秒 timeout；Debug 可用 180 秒 timeout，但实际 workload 仍只 1 秒。
4. Debug GPU-Based Validation：Native+SVGF、Quality+SVGF、Quality+NRD 各一次；
   InfoQueue ERROR/CORRUPTION 必须为 0。
5. 固定 Cornell capture：等待至少 64 submitted frames，再截 Native、DLAA、Quality、
   Balanced、Performance。比较 alpha、NaN/Inf、黑边、extent 和结构指标；不要要求不同
   算法 exact pixel equality。
6. F4 全回环、F3 在 Quality 中往返、F1 debug→Final、resize/minimize/restore 和 HLSL
   hot reload 使用真实窗口消息短测。

### 硬门槛

- RTX 4060 Laptop、1920×1080 output 下各 active DLSS 模式 GPU Total p95 ≤16.67 ms；
- 每个模式使用 SDK 返回的合法 render extent，DLAA 必须等于 output extent；
- `gpu_idle_wait_count=0` 对普通帧和 F4 切换成立；
- peak local VRAM/budget <70%；
- inactive pass 为 `null`，active pass sample 与 Total 对齐；
- 无 device removed、panic、NaN/Inf、结构化黑边或错误 history；
- feature-off Native capture 与阶段 9 reference 在同一 commit 构建下 exact 或既有严格门槛
  通过；feature-on Native 也必须与 feature-off Native 一致；
- Reflex token、sleep、marker、Present 和 presentCommon 计数对账，顺序错误为 0；
- 不得因为没有 Verification HUD 就把延迟改善写成 PASS，该项保持
  `PENDING EXTERNAL TOOL`。

画质人工项单列：移动相机观察薄几何、门框、灯具边缘、镜面顶部、disocclusion 和红绿
墙交界。DLSS 模式不得出现持续拖影、闪烁放大、错误曝光或 UI 模糊。没有可信视觉观察
时写 `PENDING USER VISUAL CONFIRMATION`，不要冒充通过。

### 建议提交

`test(stage10): add bounded DLSS and Reflex acceptance`

## 13. 提交顺序与 review 规则

可以也应该拆成多个 commit，不要求一个大提交。建议严格按下列顺序：

1. `build(stage10): pin optional Streamline SDK`
2. `feat(stage10): add Streamline D3D12 bridge`
3. `feat(stage10): define DLSS frame input contract`
4. `feat(stage10): evaluate DLSS Super Resolution`
5. `feat(stage10): switch DLSS modes with retired viewports`
6. `feat(stage10): add Reflex markers and telemetry`
7. `feat(stage10): expose DLSS and Reflex runtime controls`
8. `test(stage10): add bounded DLSS and Reflex acceptance`
9. `docs(stage10): record RTX 4060 acceptance`

每个 commit 都必须能编译；涉及 ABI 的提交必须带 layout/error tests，涉及 HLSL 的提交
必须同步 build-time compile、runtime hot reload 和 descriptor contract test，涉及资源切换
的提交必须带 fence/rollback 测试。不要把 SDK fetch、桥接、motion、evaluate、Reflex 和
验收脚本压成一个无法 review 的提交。

Luna 完成每个工作包后先停下自查并保留 commit。全部完成后交给 Codex 时提供：

- commit 列表与每个 commit 的职责；
- `git status --short`；
- 四 feature 组合的构建结果；
- test/clippy 结果；
- acceptance run-id、summary 路径和所有失败/待人工项；
- SDK asset、DLL、EXE SHA-256 和 GPU/driver provenance；
- 没有执行的项目及原因。

## 14. 禁止事项

- 不实现 RR/FG/Reflex Frame Warp。
- 不静默 fallback，不吞 SDK error，不把 unsupported 写成 PASS。
- 不硬编码 DLSS render scale，不用旧 preset A–F；preset 由 v2.12.0 默认/官方当前建议
  决定，除非有配对画质证据支持覆盖。
- 不复用 NRD motion 而缺少单位/方向/jitter 证明。
- 不把 jitter 烘进传给 Streamline 的矩阵。
- 不在每帧查询 optimal settings、重建 viewport 或 wait for GPU。
- 不在 `build.rs` 下载网络资源。
- 不部署 development DLL 到 Release。
- 不让 UI/debug overlay 进入 DLSS input。
- 不执行 600/1800 秒测试；本轮最长单子进程 workload 为 10 秒，默认 1 秒。
- 不修改或美化验收门槛来让结果通过。

## 15. 可直接交给 Luna 的提示词

```text
你要在 C:\zjk\projects\RayTracingDemo 实现阶段 10。先完整阅读：
1. docs/阶段10DLSS超分与Reflex执行方案.md
2. docs/阶段9验收记录.md
3. docs/实时DXR渲染器实施方案.md 的阶段 9–11
4. 本方案列出的 NVIDIA Streamline v2.12.0 官方 Programming Guide、DLSS、Reflex、
   Manual Hooking 文档。

严格按 10A→10H 实施，并按文档建议拆成多个可独立 review、可编译的 commit；没有必要
合成一个 commit。默认 feature 必须继续完全不依赖 Streamline，nrd 与 streamline 必须
正交。锁定官方 v2.12.0 release asset 和真实 SHA-256，禁止编造 hash、构建期联网或把
development DLL 部署到 Release。

架构必须是 PathTrace → SVGF/NRD → HDR compose → DLSS SR/DLAA → ToneMap。阶段 10
不得实现 DLSS Ray Reconstruction、Frame Generation 或 Reflex 2 Frame Warp。DLSS motion
必须通过官方约定和解析测试证明方向、单位与 jitter；不得未经证明直接复用 NRD motion。
各 DLSS render extent 必须来自 slDLSSGetOptimalSettings，禁止硬编码比例。Streamline
manual-hooking evaluate 后完整恢复 D3D12 command-list 状态；Present 与 presentCommon
严格一一对应。F4 模式切换用唯一 viewport/generation 和 fence 退休，不得正常切换时
wait_for_gpu。Reflex/PCL token、sleep、marker 的位置与计数严格按方案。

每个工作包完成后运行与风险相称的短测试再 commit。最终运行 docs 中的有界 Stage 10
矩阵；不要跑 600/1800 秒测试，默认每个 benchmark workload 1 秒。无法使用 NVIDIA
Verification HUD 或无法可信观察动态画质时，保持 PENDING，不得写 PASS。

交付时给出 commit 顺序、每包职责、git status、四种 feature build、tests/clippy、
acceptance run-id/summary、SDK/DLL/EXE hash、GPU/driver provenance，以及所有 FAIL、
PENDING、BLOCKED。不要自行修订阶段边界或验收门槛；遇到 SDK 限制导致架构冲突时先
停止并报告给 Codex。
```
