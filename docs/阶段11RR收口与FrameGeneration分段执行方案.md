# 阶段 11：RR 收口与 Frame Generation 分段执行方案

日期：2026-08-13

目标设备：NVIDIA GeForce RTX 4060 Laptop GPU

固定 SDK：Streamline v2.12.0

执行方式：Luna 分工作包实现，Codex 按提交 review；每个停止点通过后才进入下一包

## 1. 当前基线与结论

阶段 11 的 RR 主路径已经真实运行。边界稳定工作包最终由以下提交收口：

- `586a819 fix(stage11): stabilize RR opaque boundary footprint`
- `3d9d462 fix(stage11): preserve RR debug resource extents`
- `a09748f test(stage11): record RR boundary acceptance`

RTX 4060 Laptop、`1280x720` 输出、RR Quality 的 121/122 SPP 固定 capture 中：

- 右侧红/灰接缝 mean max-channel diff：`0.6453 -> 0.205523`；
- 接缝 diff > 2 的像素：`723 -> 0`；
- stable surface ID 与静态 primary motion 都 exact diff 0；
- boundary mask 覆盖率 `3.0017%`，低于 5%；
- Total p95 `5.08 ms`，primary visibility + boundary resolve p95 合计 `0.31 ms`；
- 用户已经确认当前静止画面观感良好。

因此 RR 画质算法不再作为下一包的开放修改范围。进入 Frame Generation（FG）前只补一个
可复核的 RR 收口 runner；之后按独立提交重做 manual-hooking 的创建顺序、FG 输入与统计。

## 2. 总体顺序和停止点

严格按以下顺序推进：

1. **11F-R：RR 有界收口 runner**。只补验收证据，不改 shader、采样、ToneMap 或曝光。
2. Codex review 11F-R。Quality/Balanced/Performance 和 feature-off 门槛通过后，允许 11G。
3. **11G-A：FG SDK 锁、部署和 C ABI**。不改 swap chain。
4. **11G-B：proxy device/factory/swap chain 创建边界**。FG 仍保持 off，不提交输入。
5. **11G-C：FG 输入、选项和生命周期状态机**。第一版只生成 1 个中间帧，即 2x display。
6. **11G-D：基础/显示帧统计与诊断**。
7. **11G-E：有界自动验收和人工 FrameView 项**。
8. Codex 最终 review 后再判断阶段 11 是否完成；不能提前进入阶段 12。

不得把上述包合并成一个大提交。11G-B 的 swap chain 生命周期是最高风险边界，必须可单独
回退和 review。

## 3. 全局约束

### 3.1 本轮不做

- 不改 RR/NRD/SVGF 的路径追踪采样、降噪参数、曝光或 ToneMap；
- 不接入 ReSTIR DI/GI；
- 不实现 Dynamic Multi Frame Generation；
- 不生成 2 个以上中间帧；
- 不实现 HDR、全屏独占、GPU 内 HUD 或 UI recomposition；
- 不引入新的图形框架或第二套窗口循环；
- 不靠窗口标题 FPS 宣称 FG 的显示平滑度；
- 不运行 600/1800 秒测试，单个进程外层 timeout 不超过 60 秒。

代码必须有针对非显然契约的注释，重点解释 proxy/native 接口分工、`eValidUntilPresent`、
application frame 与 displayed frame 的区别，以及为什么 resize 前必须 suspend FG；不要写逐行复述
代码的注释。

### 3.2 始终成立的回退契约

- `default` feature 必须继续完全不编译、部署或加载 Streamline；
- `streamline` 和 `streamline-rr` 不得因为新增 FG 而部署 `sl.dlss_g.dll`；
- FG 默认关闭，必须由 `streamline-fg` 编译能力和显式 `--frame-generation on` 同时开启；
- FG 关闭时必须最终使用 native swap chain，不能保留 off-screen proxy copy 开销；
- 自研 SVGF + Native 必须继续构建和运行；
- generated frame 不创建新 frame token，不推进 SPP、动画、相机历史或重建历史。

## 4. 工作包 11F-R：RR 有界收口 runner

### 4.1 文件与复用策略

新增 `scripts/stage11_rr_acceptance.ps1`。不要复制整个 `stage10_acceptance.ps1`：

- Native/SR/NRD 基线通过调用现有阶段 10 runner 的单 case 模式获得，并读取其 summary；
- 新脚本只实现 RR 专属 case、RR gate、capture/diff 和阶段 11 总 summary；
- 所有子进程继续使用 `Start-Process`、stdout/stderr/args/exit 文件和有界 `WaitForExit`；
- 不允许在 helper function 内隐藏启动 GUI/D3D 进程，保持阶段 10 已验证的前台调度方式；
- 生成物只写入已忽略的 `output/stage11/<run-id>/`。

建议参数：

```powershell
param(
  [ValidateSet('Smoke','Matrix','DebugValidation')] [string]$Suite = 'Smoke',
  [ValidateSet('','rr_quality','rr_balanced','rr_performance','rr_animated')] [string]$CaseName = '',
  [string]$RrExe,
  [string]$DefaultExe,
  [string]$NrdExe,
  [ValidateRange(1,5)] [int]$Seconds = 1,
  [ValidateRange(10,60)] [int]$TimeoutSeconds = 60,
  [uint32]$StreamlineApplicationId = 0,
  [switch]$SelfTest,
  [string]$OutputRoot = 'output/stage11'
)
```

Runner 不负责调用 Cargo，也不复制 EXE/DLL。正式矩阵使用不同 `CARGO_TARGET_DIR` 生成三套彼此
隔离的构建，再把路径显式传给脚本，避免后一种 feature 构建覆盖前一种证据。
`sl.interposer.dll` 位于 EXE 同目录，其余 plugin 位于
`streamline-plugins/<feature-set>/`；runner 按 executable 的预期 feature-set 取证，不能把同一开发
目录中其他缓存 feature 的 DLL 算入当前构建。`-SelfTest` 不启动 GPU 进程，使用内存中的合成 JSON
验证至少这些负例：错误 RR
档位、缺失 active pass、出现 hidden SR/NRD pass、idle wait 非零、错误 optimal extent、
Reflex/presentCommon 计数不等，以及 timeout/多 JSON 行；每个负例必须被对应 gate 拒绝。

### 4.2 Case 矩阵

| case | executable | 参数 | 必须检查 |
|---|---|---|---|
| default-native | default | SVGF + Native | Streamline/RR 均未编译，相关 pass null |
| sr-quality | streamline | SVGF + DLSS Quality | SR 有 samples，RR/NRD inactive |
| nrd-quality | nrd,streamline | NRD + DLSS Quality | NRD/SR 有 samples，RR inactive |
| rr-quality | streamline-rr | RR + Quality | RR evaluate/adapter/primary/boundary 有 samples |
| rr-balanced | streamline-rr | RR + Balanced | optimal extent 的 kind/mode 与尺寸正确 |
| rr-performance | streamline-rr | RR + Performance | 不能复用 Quality/Balance extent |
| rr-animated | streamline-rr | Triangle + animation | exit 0、AS 动态、无 device removed |
| rr-debug | Debug streamline-rr | 小输出 1 秒 | InfoQueue CORRUPTION/ERROR 为 0 |

`Smoke` 只执行 rr-quality；`Matrix` 执行三个 RR 档位和 animated，并汇入阶段 10 的三条基线；
`DebugValidation` 只执行一个低分辨率 RR case。每个 case 默认 1 秒，不做重复长跑。

### 4.3 自动硬门槛

每个 RR Release case：

- exit 0、stdout 恰好一行可解析 JSON、GPU 名称包含 `RTX 4060 Laptop`；
- `denoiser.requested/active == dlss-rr`，`dlss_rr.compiled/supported == true`；
- `upscaler.mode` 与 case 一致，render extent 等于 RR optimal settings；
- `rr_evaluate`、`rr_input_adapter`、`rr_primary_visibility`、`rr_boundary_resolve` 均有 sample；
- SVGF temporal/À-Trous、NRD 和普通 DLSS SR evaluate/compose 都为 null/0 samples；
- `gpu_idle_wait_count == 0`，Total p95 `< 16.67 ms`，本地显存占预算 `< 70%`；
- Reflex token、sleep、六个 marker 和 `presentCommon` 都按 application frame 一一对账；
- stderr 不包含 device removed、DRED、NaN/Inf guide 或静默 fallback。

三个档位的 optimal render extent 必须单独记录。不要硬编码 NVIDIA 的具体尺寸，但至少两档实际
尺寸不同，否则 runner 判为配置/查询可疑并失败。

### 4.4 画质证据

不重跑大矩阵。保留 `a09748f` 的 121/122 SPP 接缝记录作为 RR Quality 静止边界证据；runner
只需要验证记录中的 commit 位于当前 HEAD 祖先链、`image_diff --roi` 接口仍有单元测试。
若实现改动触碰 shader、RR resources、ToneMap 或 camera/motion，旧证据自动失效，必须停止并
交给 Codex 决定重拍范围，Luna不能自行调 ROI 或阈值。

### 4.5 Summary 可信性

阶段 11 summary 至少保存：

- schema、run-id、suite、pass/fail 和逐 gate failure；
- `git_head`、`git_tree`、dirty 状态；dirty tree 只能运行开发 smoke，不能写正式 PASS；
- 每个 EXE 的绝对路径、SHA-256、features/provenance；
- Streamline SDK version、production/development flavor，以及已部署 DLL SHA-256；
- 环境、GPU/driver、电源来源；
- 每条命令、stdout、stderr、exit、timeout、elapsed 和解析后的 JSON。

建议提交：

`test(stage11): close bounded RR acceptance gap`

完成后停止，等待 Codex review。此提交不得包含 FG 代码。

## 5. 11G-A：FG SDK 锁、部署与 ABI

> 2026-08-20 更新：本节保留为总体路线。实际实施以
> [`阶段11FrameGeneration-11G-A-SDK部署与ABI执行方案.md`](阶段11FrameGeneration-11G-A-SDK部署与ABI执行方案.md)
> 为准；新版根据本地 Streamline v2.12 指南修正了 `slSetFeatureLoaded` 的边界，并锁定当前 ABI
> v4、manual-hooking 与 stable-plane 基线。

### 5.1 Cargo/build 隔离

新增：

```toml
streamline-fg = ["streamline"]
```

不要让 `streamline-rr` 隐式启用 FG，也不要让 `streamline-fg` 强制启用 RR。目标组合可以是
`--features streamline-rr,streamline-fg`，但两个能力保持正交。

在 `third_party/streamline/version.lock.json` 中以 `optional_feature="streamline-fg"` 固定：

- `include/sl_dlss_g.h`；
- production/development `sl.dlss_g.dll`；
- production/development `nvngx_dlssg.dll`；
- SDK 已有总许可证和 third-party notices。若压缩包没有独立 FG license，不得虚构文件。

`fetch_streamline.ps1` 继续从同一个官方 v2.12.0 archive 验证 hash、安全路径和 NVIDIA 签名。
`build.rs` 仅在 `CARGO_FEATURE_STREAMLINE_FG` 时验证、部署 FG DLL，并把
`STREAMLINE_ENABLE_FG=ON/OFF` 传给 CMake。

### 5.2 C ABI v5

把 bridge ABI 从 v4 升为 v5，Rust/C++ 同步修改并静态检查 size/alignment。至少增加：

- init/support 中的 `enable_dlss_fg`、`fg_supported`、`fg_result`；
- `FrameGenerationOptions`：off/on、`num_frames_to_generate=1`、flags、三个输入/颜色尺寸和格式、
  backbuffer count；第一版不暴露 dynamic target；
- `FrameGenerationState`：status、min dimension、`num_frames_actually_presented`、
  `num_frames_to_generate_max`、VRAM estimate、VSync support、Dynamic MFG support；
- `streamline_bridge_fg_set_options`；
- `streamline_bridge_fg_get_state`，普通每帧查询必须传 null options，VRAM estimate 只在初始化/
  resize 时带 `eRequestVRAMEstimate` 查询一次；
- `streamline_bridge_set_feature_loaded`，只允许受控加载/卸载 `kFeatureDLSS_G`；
- `streamline_bridge_get_native_interface`，用于从 proxy swap chain 获取 native interface。

C++ 侧只在 `STREAMLINE_ENABLE_FG` 时包含 `sl_dlss_g.h` 和引用 `kFeatureDLSS_G`。feature 未编译
时所有 FG 函数返回稳定 `UNSUPPORTED`，不能链接或加载 FG DLL。

### 5.3 支持性规则

- 不能根据“RTX 4060”字符串推断支持；必须以 `slIsFeatureSupported` 和 `DLSSGState` 为准；
- 显式请求 on 而 feature 未编译/SDK 不支持/Reflex 不可用时，启动失败并带 raw result/status；
- HAGS、驱动、OS 等外部条件不满足时给出明确诊断，不静默回退为 off；
- `numFramesToGenerateMax < 1` 时不能设置 on；第一版即使最大值更高也固定请求 1。

建议提交：

`build(stage11): pin optional DLSS frame generation runtime`

## 6. 11G-B：manual-hooking 和 swap chain 创建边界

详细实施、提交拆分和验收硬门槛见
[`阶段11FrameGeneration-11G-B-ManualHooking与交换链边界执行方案.md`](阶段11FrameGeneration-11G-B-ManualHooking与交换链边界执行方案.md)。

### 6.1 当前缺口

当前代码先用 native factory/device 创建 command queue 和 swap chain，之后才升级 swap chain。
这足以让现有 `presentCommon()` 被调用，但不满足 FG 的 manual-hooking 要求：

- `ID3D12Device::CreateCommandQueue` 是被 Streamline hook 的 API；
- `IDXGIFactory::CreateSwapChainForHwnd` 是被 hook 的 API；
- proxy swap chain 的 `Present/GetBuffer/ResizeBuffers/GetCurrentBackBufferIndex` 必须走 proxy；
- 非 hook API 和第三方调用应继续使用 native interface。

### 6.2 集中的创建模型

禁止继续在 `Dx12Renderer::new` 中散落创建/升级逻辑。新增小型私有封装，例如：

```text
StreamlineDeviceInterfaces
  native_device
  optional proxy_device       // 只用于 CreateCommandQueue hook

SwapChainInterfaces
  native_factory
  optional proxy_factory      // 只用于 CreateSwapChain* hook
  native_swap_chain
  optional proxy_swap_chain   // hook 列表中的所有 swap-chain 调用
```

具体顺序：

1. `slInit` 仍早于 DXGI/D3D hook；
2. 创建 native factory 和 native D3D12 device；
3. `slSetD3DDevice(native_device)` 并查询 support；
4. FG feature 编译时，DLSS-G 至少保持 loaded 到 application command queue 创建完成；升级一个
   device clone，通过 proxy device 创建该 queue。这样即使启动配置为 off，后续 F5 开启也不需要
   重建设备和 command queue；
5. 启动配置为 off 时，在创建 swap chain 前卸载 DLSS-G；配置为 on 时保持 loaded。需要 FG
   proxy chain 时升级一个 factory clone，通过 proxy factory 调用
   `CreateSwapChainForHwnd`；
6. 从 proxy swap chain 取得 native swap chain；
7. `Present/GetBuffer/ResizeBuffers/GetCurrentBackBufferIndex/SetFullscreenState` 走 proxy；其他
   非 hook DXGI 调用走 native；
8. FG 未加载时通过 native factory 创建 native swap chain，不保留 off-screen proxy 开销。

不要同时把同一个 COM raw pointer交给两个 Rust owner。沿用当前 upgrade helper 的 AddRef/释放
纪律，并给 proxy/native 指针相等、升级失败和 already-upgraded 加测试。

### 6.3 集中重建函数

实现唯一的 `recreate_swap_chain(reason, fg_loaded, width, height)` 或等价入口，供启动、F5、resize
和恢复复用。它负责：

1. 阻止新帧提交；
2. 若已有 FG，提交 off 和 null tags；
3. 只在显式切换/resize 边界等待 GPU；正常帧仍不得 idle wait；
4. 释放 command-list/backbuffer 引用、RTV 包装和 proxy/native swap chain；
5. 受控 load/unload `kFeatureDLSS_G`；
6. 重新创建正确类型的 chain 和全部 backbuffer RTV；
7. 重置 frame fence/timing、重建 output generation、请求一次历史 reset；
8. 输出一条结构化 lifecycle diagnostic。

本提交只建立创建/重建边界，FG mode 仍 off。feature-off、普通 Streamline SR 和 RR 的 Present
计数必须保持不变。

建议提交：

`feat(stage11): create Streamline-managed FG swap chain`

## 7. 11G-C：FG 输入、选项和生命周期

### 7.1 用户配置

新增强类型：

```text
FrameGenerationMode::{Off, On}
--frame-generation off|on       // 默认 off
F5                              // 仅交互模式切换 off/on
```

第一版 `on` 必须同时满足：

- 编译了 `streamline-fg`；
- Reflex 为 on/on-boost，禁止 `--reflex-mode off`；
- active reconstruction 使用 DLSS SR 或 RR，确保已有同契约 dense depth/motion guides；
- 非 benchmark/capture 特例也使用同一个 application viewport/token。

本轮不为 SVGF + Native 单独创建 FG guides；该组合请求 on 应明确报错。

### 7.2 输入复用

FG 只使用已有资源：

- Depth：Stage 10 的 primary-surface DLSS depth；
- Motion：Stage 10 的 dense primary motion，沿用当前 pixel-space scale/common constants；
- HUD-less color：ToneMap 后的 output-resolution `display_output`；项目没有 GPU UI，因此它就是
  最终场景色；
- Backbuffer：由 proxy swap chain 自动拦截，不传 resource pointer；无 subrect 时不必 tag；
- UI：项目没有 GPU HUD。显式清空 UI Alpha/UIColorAndAlpha stale tag，不创建全尺寸零纹理，
  `enableUserInterfaceRecomposition=false`；OS 硬件光标不是 backbuffer UI。

不新增一份 FG depth/motion/HDR 副本。每个输入起步都使用 `eValidUntilPresent`，内容在同一条
application command queue 上于 Present 前完成。只有 capture/验证证明资源在 Present 前被重用，
才允许缩短单个 tag 的 lifecycle，不能凭猜测全部改成 volatile。

Tag 放在 ToneMap/FG 输入写完后、command list Close 前。ResourceTag state 必须来自 tracked
resource 的真实状态，不能硬编码。Streamline 管理后续 transition，但应用的状态追踪器仍必须在
下一帧获得一致状态。

### 7.3 token、Present 和计数语义

- 每个 application render frame 取得一个 token，并只提交一次 common constants；
- depth/motion/HUD-less tags、Reflex markers、FG options 和目标 Present 使用同一 viewport/frame；
- `slDLSSGSetOptions` 在 Present 线程执行，保证在目标 Present 之前；
- proxy `Present` 每个 application frame恰好一次，不能再手工调用 `presentCommon()`；
- generated frame 不调用 render、不取得 token、不更新 camera/animation/SPP/history；
- PresentStart/End marker 按 application Present 计数，不按实际 display frame 计数。

### 7.4 开关、resize、最小化和失败

使用显式状态机，而不是多个 bool：

```text
Unavailable -> OffNative -> Enabling -> OnProxy -> Suspending -> OffNative
```

- on：检查 support/state，等待显式边界，加载 FG，重建 proxy chain，重建 backbuffer，首个有效
  frame tag 完整输入后再提交 on；
- off：提交 off、清空 tags、等待显式边界、释放 proxy backbuffer/chain、卸载 FG、重建 native
  chain；
- resize：先进入 Suspending，off + null tags；GPU 安全后重建目标尺寸。若用户请求仍为 on，
  至少完成一个有效 application frame 输入后再恢复 on；
- minimize/零尺寸：保持 off，不 Present、不保留有效 tags；恢复时走同一重建入口；
- loading/capture pending/无有效 guides：FG 必须 off 或 tags null；
- `DLSSGState.status != eOk`：记录 raw status，安排安全关闭，不继续显示 active；显式请求的启动
  首帧失败应返回错误，不能静默使用 off。

正常 steady-state 不能调用 `WaitForGpu`。只有用户 F5、resize、最小化/恢复、退出等显式生命周期
边界允许一次可解释 idle wait，并计入单独 diagnostic，不污染 benchmark steady-state 计数。

建议提交：

`feat(stage11): tag FG inputs and present generated frames`

## 8. 11G-D：统计、标题和 JSON

实施状态（2026-09-12）：已完成。实现与验证记录见
[`阶段11FrameGeneration-11G-D-统计与延迟实施记录.md`](阶段11FrameGeneration-11G-D-统计与延迟实施记录.md)。

### 8.1 统计定义

`slDLSSGGetState` 在每个 application Present 后查询一次，普通查询传 null options。因为
`numFramesActuallyPresented` 表示自上次查询以来实际显示的帧数，统计窗口必须累计：

```text
application_frames += 1
displayed_frames += numFramesActuallyPresented
generated_frames += max(numFramesActuallyPresented - 1, 0)
dropped_generated_frames += max(requested_multiplier - numFramesActuallyPresented, 0)
```

同一个时间窗口分别计算：

- base/application FPS = application_frames / elapsed；
- display FPS = displayed_frames / elapsed；
- actual presented multiplier = displayed_frames / application_frames；
- generated/dropped 计数。

不能简单用某一帧 multiplier 乘滚动 FPS。Total GPU p50/p95 仍表示 application frame 渲染成本，
不是 generated frame 的 GPU cost，也不是显示间隔。

### 8.2 JSON 和标题

新增稳定对象：

```json
"frame_generation": {
  "compiled": true,
  "requested": "on",
  "active": "on",
  "support_result_raw": 0,
  "status_raw": 0,
  "requested_generated_frames": 1,
  "max_generated_frames": 1,
  "application_frames": 0,
  "displayed_frames": 0,
  "generated_frames": 0,
  "dropped_generated_frames": 0,
  "actual_presented_multiplier": 0.0,
  "base_fps": 0.0,
  "display_fps": 0.0,
  "estimated_vram_bytes": 0,
  "vsync_supported": false,
  "dynamic_mfg_supported": false,
  "reason": null
}
```

feature-off 时字段仍有稳定 schema，但 `compiled=false`，SDK 值为 null，不加载 DLL。

标题只显示紧凑的 `Base 120 / Display 232 / FG 1.93x`，并继续显示 Total GPU。Reflex latency
report 不可用时写 null/`N/A`，不得根据 FPS 估算 latency。

Capture 仍保存 application `display_output`，不是 generated frame；capture JSON 明确记录
`capture_source="application_display_output"` 和 FG requested/active，不能把它当 FG 图像证据。

建议提交：

`feat(stage11): report base and generated frame metrics`

## 9. 11G-E：FG 有界验收

新增 `scripts/stage11_fg_acceptance.ps1`，仍采用有界子进程和 raw artifact。首轮只运行：

| case | 参数 | 自动门槛 |
|---|---|---|
| feature-off | default | 无 FG DLL/compiled/resource，Native 正常 |
| fg-compiled-off | streamline-fg + DLSS Quality | requested/active off，native chain、无 proxy off 开销 |
| fg-quality | streamline-fg + DLSS Quality + on | support/status ok，至少生成过一帧 |
| fg-rr-quality | streamline-rr,streamline-fg + RR Quality + on | RR 和 FG 同时真实 active |
| fg-animated | Triangle animation + on | 动态 motion、无 device removed |
| fg-debug | 低分辨率 Debug + on | InfoQueue/DRED 0 |

自动门槛：

- 显式 on 时 `compiled/supported/active` 全为真，status 0，max generated frames >= 1；
- warmup 后 `application_frames > 0`、`displayed_frames >= application_frames`，且至少一次
  `numFramesActuallyPresented > 1`；允许 generated frame 因 pacing 被丢弃，不能硬要求恒定 2.0x；
- token、common constants、六个 Reflex marker、proxy Present 和 `presentCommon` 都等于 application
  frame 数，绝不能等于 displayed frame 数；
- `gpu_idle_wait_count == 0` 在稳态 benchmark 中成立；
- FG off case 不保留 proxy/off-screen copy 的额外 backbuffer；
- feature-off executable 不部署或加载 `sl.dlss_g.dll/nvngx_dlssg.dll`；
- resize/F5/minimize 的显式 lifecycle wait 单独计数，不能混入稳态 gate；
- Debug CORRUPTION/ERROR 0，无 device removed、DRED、Present deadlock 或超时。

人工项：

- F5 连续 on/off 两次；
- on 状态 resize、最小化/恢复、退出；
- 相机平移和 Triangle 动画观察 generated frame 是否有明显 UI/边缘异常；
- 用 FrameView 查看 `MsBetweenDisplayChange`。`MsBetweenPresents`、窗口标题或内部 display FPS
  不能替代该外部 pacing 证据；
- Reflex Analyzer/latency report 若当前环境不可用，明确记录 PENDING，不造数值。

建议提交：

`test(stage11): add bounded frame generation acceptance`

## 10. Codex review 重点

1. FG DLL 是否只在 `streamline-fg` 部署，hash/签名/许可是否固定；
2. device/factory 是否在被 hook 的 CreateCommandQueue/CreateSwapChain 前升级；
3. proxy/native COM ownership 是否无双重释放，hook API 是否全部走 proxy；
4. FG off 是否真的重建 native swap chain，而不是 proxy mode off 后长期保留额外 copy；
5. tags 是否使用同一 token/viewport，common constants 是否每帧只提交一次；
6. depth/motion 是否复用正确的 dense primary guides，HUD-less 是否是 ToneMap 后 output color；
7. `eValidUntilPresent` 输入是否活到 Present，resize 前是否 off + null；
8. generated frame 是否错误推进了 frame index、SPP、动画或历史；
9. Reflex/application Present/presentCommon 的计数是否按真实 application frame，而非 display frame；
10. JSON 是否区分 base/display FPS、generated/dropped 和外部 FrameView PENDING；
11. 任何失败是否显式，不允许 active 字段撒谎或静默 fallback；
12. steady-state 是否仍无 CPU/GPU 全局等待，feature-off 是否无专有依赖回退。

## 11. 阶段 11 完成定义

只有以下全部满足才允许进入阶段 12：

- 11F-R 的 RR 三档和 feature-off/NRD/SR 对照通过；
- RR 静止边界保持当前指标，运动中无明显错误历史；
- FG off/on、resize、最小化/恢复和退出无错误；
- FG 与 RR 同时运行，actual presented multiplier 有真实证据；
- base FPS、display FPS、GPU Total 和 latency 的含义没有混淆；
- Reflex marker、Present、presentCommon 对账正确；
- default feature 仍能独立构建和运行；
- 自动证据绑定 commit/tree/EXE/DLL，FrameView/人工项明确 PASS 或 PENDING。

参考依据为仓库固定 SDK 中的：

- `external/streamline-v2.12.0/docs/ProgrammingGuideManualHooking.md`
- `external/streamline-v2.12.0/docs/ProgrammingGuideDLSS_G.md`
- `external/streamline-v2.12.0/include/sl_dlss_g.h`
