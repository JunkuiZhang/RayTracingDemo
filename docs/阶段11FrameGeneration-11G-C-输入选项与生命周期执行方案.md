# 阶段 11：Frame Generation 11G-C 输入、选项与生命周期执行方案

日期：2026-08-29

目标硬件：NVIDIA GeForce RTX 4060 Laptop GPU

基线提交：`6263fb3 fix(streamline): allow signed OTA compatibility updates`

状态：ACCEPTED；Codex review 修复与聚焦 generated-frame 真机门禁均通过，11G-D 已实现

后续依赖升级（2026-09-10）：项目已整体切换到 Streamline 2.14.1/NGX 310.9.1，且 RR 使用
DLSS 4.5 `Preset F`。本文件中 v2.12.0 的路径和字段描述是 11G-C 实施时的历史基线，不应据此
把当前依赖降级。短测试矩阵已在 2.14.1 上重新通过；窗口聚焦的 FG 2x 门禁取得了
`status=0`、`numFramesActuallyPresented=2`、warmup 4 的真机证据；详见
[`阶段11DLSS4.5与Streamline2.14.1升级记录.md`](阶段11DLSS4.5与Streamline2.14.1升级记录.md)。

11G-D 后续统计见
[`阶段11FrameGeneration-11G-D-统计与延迟实施记录.md`](阶段11FrameGeneration-11G-D-统计与延迟实施记录.md)。

Review 修复提交：`58d7cd6 fix(stage11): harden frame generation integration`。默认、Streamline、
Streamline+FG、Streamline+RR+FG 的单元/桥接短矩阵分别通过 176/6、186/6、189/6、195/6。
RTX 4060 Laptop 的短时失焦 smoke 得到 `status=0 actual_presented=1 focused=0 warmup=0`，且 SDK
明确记录失焦暂停；这证明误超时已修复，但不冒充 `actual_presented>=2` 的聚焦人工验收证据。

11G-C 呈现策略：`bIsVsyncSupportAvailable` 只是 SDK/驱动能力位，不能证明当前窗口已经处于
Independent Flip。11G-C 尚未提供独立的 VSync/IFLIP 策略，因此 FG 链加载期间固定使用
`SyncInterval=0`，FG 关闭时维持原有 `SyncInterval=1`。后续若加入用户可控 VSync，必须同时
验证 FrameView PresentMode/IFLIP，不能仅凭该能力位自动开启。

VSync off 不是只把 `Present` 的 interval 改为 0：必须先用
`IDXGIFactory5::CheckFeatureSupport(DXGI_FEATURE_PRESENT_ALLOW_TEARING)` 查询能力，并在交换链创建和
每次 `ResizeBuffers` 时保留 `DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING`，随后才可在 `Present(0, ...)`
传入 `DXGI_PRESENT_ALLOW_TEARING`。三处必须共享同一契约；否则 Streamline 内部补齐 present flag
时会遇到一个未 opt-in 的交换链，可能令 proxy Present 失败或卡住。

短测曾连续得到 `numFramesActuallyPresented=1`。开启 SDK 文件日志后，Streamline 明确报告
`DLSS-G disabled: window not focused`：失焦是驱动主动暂停插帧，不是集成失败。窗口失焦期间仍查询
state，但不消耗 120 个 application Present 的 warm-up 预算；重新聚焦后继续确认。只有窗口聚焦且
连续 120 个完整 Present 仍未观测到至少 2 帧时才判为失败。不能用硬编码 60 Hz Reflex limiter 掩盖
这个状态；帧率策略应留给后续显示刷新率/用户上限设计。

设置环境变量 `RAY_TRACING_STREAMLINE_LOG`（值可为空）可让 Release 构建在可执行文件目录生成
`sl.log`，便于复核驱动侧暂停原因；默认 Release 保持安静。SDK 日志还要求中心针孔相机显式提交
`cameraPinholeOffset=(0,0)`，并建议用 null backbuffer resource + 全输出 extent 明确 full-frame FG
区域。本实现已同时满足这两项，避免把默认 invalid sentinel 或 0×0 backbuffer extent 留给 SDK 猜测。

## 1. 当前基线与本包目标

11G-A 已完成 Streamline v2.12.0 DLSS-G SDK、按 feature set 隔离的部署、support/state/options
C ABI。11G-B 及 Codex review 修复已经完成 linked interposer 的单一 proxy 层、application queue 与
swap-chain hook 路由、native/proxy COM ownership、FG loaded-state ABI v6，以及默认关闭时在
swap-chain 创建前卸载 DLSS-G。当前程序仍不会提交 FG 输入，也不会生成中间帧。

11G-C 要第一次让 DLSS Frame Generation 真实工作，但范围严格限定为：

1. 增加默认关闭的 `--frame-generation off|on` 与交互式 `F5`；
2. 建立单一、可审计的 FG 生命周期状态机；
3. 复用现有 DLSS SR/RR 的 depth、motion 和 ToneMap 后 `display_output`；
4. 使用同一 frame token、viewport、common constants、Reflex markers 和 application Present；
5. 第一版固定 2x display：每个 application frame 只请求 1 个 generated frame；
6. 安全处理启动、F5、resize、最小化/恢复、debug view、资源 generation 切换、错误和退出；
7. 至少从 `slDLSSGGetState` 取得一次 `status == eOk` 且
   `numFramesActuallyPresented >= 2` 的短证据，不能仅凭 options-on 宣称 FG 生效。

本包不做完整 base/display FPS 统计，不修改 benchmark JSON schema；这些属于 11G-D。FrameView、
PresentMon、完整人工切换矩阵和最终验收属于 11G-E。

## 2. 以本地锁定 SDK 为准的硬契约

实现前必须完整阅读：

- `external/streamline-v2.12.0/docs/ProgrammingGuideDLSS_G.md`
- `external/streamline-v2.12.0/docs/ProgrammingGuide.md` 的 frame token、frame-based tagging、common
  constants 和 resource lifecycle 章节
- `external/streamline-v2.12.0/docs/ProgrammingGuideManualHooking.md`
- `external/streamline-v2.12.0/include/sl_dlss_g.h`
- `external/streamline-v2.12.0/include/sl_core_types.h`

必须保持以下契约：

- 每个 application render frame 只取得一个 `FrameToken`；生成帧不取得 token，不执行 render、相机、
  动画、路径追踪、SPP、history 或 profiler pass。
- 同一 frame/viewport 的 common constants 只调用一次。锁定的 Streamline v2.12.0
  `include/sl_consts.h` 已没有旧文档片段中的 `Constants::renderingGameFrames`；不能修改 SDK 布局或在
  bridge ABI 中提供一个被静默丢弃的假字段。是否正在渲染有效游戏帧由本版本的 FG options/tag
  契约表达：无完整 guides、暂停、debug view、resize/minimize 时必须 options-off 并清空 tags。
- `eDepth`、`eMotionVectors` 和 `eHUDLessColor` 使用 `eValidUntilPresent`。在其资源被重用、resize、
  generation/viewport 被替换、FG 关闭或 Streamline shutdown 前，必须以同 frame token 提交 null
  tag，释放 Streamline 持有的引用。
- 用 null resource 的 backbuffer tag 显式提供全输出 extent；proxy Present 已知道资源 owner。项目无 GPU HUD，必须清空
  `eUIColorAndAlpha` 与 `eUIAlpha` stale tag，并保持
  `enableUserInterfaceRecomposition=false`，不能创建无意义的全尺寸透明 UI 纹理。
- `slDLSSGSetOptions` 在 Present 所在线程、目标 Present 之前调用。应用只调用一次 proxy
  `Present`，不能手工调用 `presentCommon()`。
- 在 resize、最大化/最小化、swap-chain 重建或卸载插件前，先 options-off、清空 tags，再进行有界
  GPU wait 和资源释放。
- DLSS-G 默认 D3D12 queue mode 保持 `eBlockPresentingClientQueue`。本包不开放 Vulkan-only queue
  parallelism，也不自行管理 `inputsProcessingCompletionFence`。
- DLSS-G loaded 与 DLSS-G mode 是两个状态：mode-off 不会消除 off-screen swap-chain 开销；显式
  用户关闭最终必须重建在 FG unloaded 状态下创建的 chain。

## 3. 11G-B 后必须保留的边界

### 3.1 linked interposer 只有一层

当前 `sl.interposer.lib` 会让 `CreateDXGIFactory2`/`D3D12CreateDevice` 返回 linked proxy。不要再次
调用 `slUpgradeInterface`，不要构造第二层 proxy，也不要根据 COM 裸指针是否相等判断 FG 是否开启。

FG 状态只能由以下事实决定：

- 用户 requested mode；
- `slIsFeatureLoaded(kFeatureDLSS_G)`；
- 生命周期状态机；
- `slDLSSGGetState` 的 raw status/actual-presented 结果。

即使 FG unloaded，swap-chain 的 hooked owner 仍可能是 Streamline common 的转发接口，用于
`presentCommon()`/Reflex。`SwapChainInterfaces::is_proxied()` 不能充当 FG active telemetry。

### 3.2 关闭时没有 FG off-screen 开销

所有 swap-chain 创建仍通过 `factory_interfaces.hooked().CreateSwapChainForHwnd`。关键区别是创建时
DLSS-G 是否 loaded：

- requested off：queue 创建完成后 unload FG，再创建 chain；
- requested on：support 和前置条件通过后保持 FG loaded，直接创建 FG-managed chain；不要先创建
  off chain、等待一次 GPU、再重建；
- F5 on/off：必须释放旧 backbuffers/chain，在 load/unload 后用相同 description 重建。

## 4. 严格范围

### 4.1 允许修改

- `src/realtime.rs`
- `src/main.rs`
- `src/realtime/windows_app.rs`
- `src/streamline.rs`
- `src/renderer/d3d12.rs`
- `native/streamline_bridge/include/streamline_bridge.h`
- `native/streamline_bridge/src/streamline_bridge.cpp`
- 与本契约直接相关的单元/源码契约测试
- 本执行文档的实施状态

只有确有需要编译桥接时才最小修改 CMake；不得修改 SDK lock、下载脚本、DLL hash/签名策略或
`build.rs` 的 feature-set 部署结构。

### 4.2 禁止修改

- 不做 11G-D 的完整 application/display/generated/dropped FPS 统计与 JSON schema；
- 不做 Dynamic Multi Frame Generation，`numFramesToGenerate` 永远为 1；
- 不加入 GPU UI、ImGui、UI mask 或非全屏 backbuffer subrect；
- 不修改 RR/NRD/SVGF、stable planes、路径采样、曝光、ToneMap 数学或 DLSS preset；
- 不新增 FG depth/motion/color 复制资源；
- 不用窗口 FPS、options-on、proxy pointer 或窗口观感单独证明 FG 生效；
- 不把 `WaitForGpu` 放进 steady-state render；
- 不通过跳过 `slShutdown()`、`TerminateProcess` 或无效 Application ID 掩盖现有 NVIDIA shutdown
  hang；
- 不运行 30/600/1800 秒长测，单个测试/进程外层 timeout 不超过 60 秒；
- 不提前进入 11G-D/E 或阶段 12。

## 5. 工作包 C1：配置、ABI v8 与窄桥接契约

### 5.1 强类型配置

在 `src/realtime.rs` 增加：

```text
FrameGenerationMode::{Off, On}
```

要求：

- `Default` 为 `Off`；
- `as_str()` 只返回 `off|on`；
- `RealtimeConfig` 增加 `frame_generation`；
- CLI 增加 `--frame-generation off|on`，缺值和非法值给出清晰中文错误；
- feature-off 构建可以解析 `off`，但显式 `on` 必须在启动 renderer 前返回“需要
  `--features streamline-fg`”的错误，不能静默降级；
- `on` 与 `--reflex-mode off` 互斥；
- `on` 必须有可提供现有 guides 的 reconstruction：
  - DLSS SR/DLAA 路径：DLAA、DLSS Quality/Balanced/Performance；或
  - DLSS RR 路径：RR + Quality/Balanced/Performance。
  Native upscaler + SVGF/NRD 请求 on 必须报错。本包不为该组合增加 guides。

先在纯配置函数中验证静态组合，再在 device attach 后验证真实 `fg_supported`。不要把这些条件散落
在构造函数和 F5 handler 中。

### 5.2 ABI 从 v6 升到 v8

在 C/Rust 两侧同步升到 7，并只追加 HUD-less format，保留既有字段 offset：

```text
StreamlineBridgeFrameGenerationOptions
  + hud_less_buffer_format: uint32
```

`uiBufferFormat` 保持 0，`enableUserInterfaceRecomposition` 在 C++ 中固定为 false，不为本项目没有的
UI 模式扩大 ABI。`make_fg_options` 必须显式设置：

- `mode = eOff|eOn`；
- `numFramesToGenerate = 1`；
- `numBackBuffers = FRAME_COUNT`；
- `mvecDepthWidth/Height = render_extent`；
- `colorWidth/Height = output_extent`；
- `colorBufferFormat = DXGI_FORMAT_R8G8B8A8_UNORM`；
- `mvecBufferFormat = DXGI_FORMAT_R16G16_FLOAT`；
- `depthBufferFormat = DXGI_FORMAT_R32_FLOAT`；
- `hudLessBufferFormat = DXGI_FORMAT_R8G8B8A8_UNORM`；
- `queueParallelismMode` 使用 SDK 默认；
- `enableUserInterfaceRecomposition = eFalse`。

on 与带 estimate 的 get-state 必须验证完整 resource description；off 可以不带资源描述。所有 out
结构进入函数即清零，错误保留 API 名和 raw `sl::Result`。

### 5.3 null resource tag

当前桥接拒绝 `ResourceTag.resource == nullptr`，这与 NVIDIA 的释放契约冲突。修改现有
`streamline_bridge_set_tags`，但不要另造全局旧式 `slSetTag` API：

- 非空 resource 沿用现有 `sl::Resource` + tracked D3D state + extent；
- null resource 只允许 `state == 0` 且 extent 全为 0，构造
  `sl::ResourceTag(nullptr, buffer_type, eValidUntilPresent, nullptr)`；
- 混合 null/non-null 数组合法；
- 全 null 时 `command_list` 可以为 null；
- 非空 `eOnlyValidNow`/`eValidUntilEvaluate` 仍要求 command list；
- buffer type 和 lifecycle 使用 allowlist，至少锁住本项目已有 tag 与 FG 的 0、1、2、23、69；
- Rust 增加语义 helper `streamline_null_resource_tag(buffer_type)`，禁止 call site 手填半初始化结构。

### 5.4 common constants

FG 与 SR/RR 必须复用现有每 frame/viewport 唯一一次 `slSetConstants`。不要为了 FG 第二次提交
constants，也不要依据已从 v2.12.0 实际头文件删除的 `renderingGameFrames` 成员手工扩展 SDK 对象。
最小化或没有有效 guides 时不 Present，并通过 options-off/null tags 表达暂停。

建议提交：

`feat(stage11): add frame generation config and ABI v8`

## 6. 工作包 C2：唯一生命周期状态机与 chain 重建

### 6.1 状态，不使用多个漂移 bool

增加私有强类型状态，名称可按代码风格调整，但至少表达：

```text
Unavailable(reason)
OffNative
EnablingProxy        // loaded + FG-managed chain，等待首个完整输入
OnProxy              // options-on，持续提交有效输入
SuspendedProxy       // chain/feature 保持 loaded，options-off，等待有效输入恢复
Disabling            // 显式边界中的瞬时状态
FaultPendingDisable(raw_status)
```

`requested` 与 `runtime state` 可以放在一个 owner 中，但不得用
`fg_requested/fg_loaded/fg_active/fg_tags_live/...` 多个无约束 bool 模拟状态机。所有转移集中在
`FrameGenerationController` 或等价 renderer 私有组件中，单元测试覆盖合法/非法转移。

### 6.2 保存重建所需数据

renderer 保存：

- `HWND`；
- 不可变的 `DXGI_SWAP_CHAIN_DESC1` 模板；
- `Option<SwapChainInterfaces>`，使旧 chain 可以在 load/unload 前明确 `take/drop`；
- 上一次成功提交 FG tags 的 `FrameToken` 和 viewport id，仅用于 null-tag 释放；没有 live tags 时为空；
- 独立的 lifecycle idle-wait 计数/原因诊断。本包不进入 benchmark JSON，可以先只输出结构化 stderr。

所有 `hooked_swap_chain()`/`native_swap_chain()` accessor 在 `Option` 为空的短重建区间不得被调用。

### 6.3 统一重建 helper

实现唯一 `recreate_swap_chain_for_frame_generation(target_loaded, extent, reason)` 或等价 helper：

1. 若 FG plugin 当前 loaded 且存在 viewport，先 `slDLSSGSetOptions(eOff)`；
2. 若有 live FG tags，使用保存的同 frame token/viewport 提交 depth、motion、HUD-less、UI color/alpha、
   UI alpha 五个 null tag；全 null 不传 command list；
3. 只在此显式生命周期边界调用一次 `wait_for_gpu()`，并记录 reason；
4. poll capture、invalidate profiler/timing，reset/close command list，释放三个 backbuffer owner；
5. `take/drop` 旧 swap-chain proxy/native owners；
6. 调用 FG loaded-state set/query，严格复核等于 `target_loaded`；
7. 使用 11G-B 的 hooked factory、application queue、原 HWND 和相同 desc 创建新 chain；
8. 重新取得 backbuffers/RTV，复位 frame fence/timing、history、camera previous state；
9. 不重建设备、command queue、factory、RTAS 或非尺寸相关 generation；
10. 输出一行结构化诊断，包含 reason、requested、state、fg_loaded、extent、idle_wait_delta；不打印
    COM 地址。

如果任一步失败，不得留下“旧 chain 已释放但状态仍 OnProxy”的假状态。返回错误前尽力进入
`FaultPendingDisable`；下一安全边界卸载并重建 off chain。不要在 GPU work in flight 时重试 load/unload。

### 6.4 启动与 F5

启动 requested off：沿用 11G-B，queue 创建后 unload，再创建 off chain，状态为 `OffNative`。

启动 requested on：

1. attach 后真实检查 support、Reflex 与 resolved reconstruction；
2. 用完整 options 调用一次 get-state estimate，验证尺寸不低于 `minWidthOrHeight`，且
   `numFramesToGenerateMax >= 1`；
3. 保持 FG loaded，直接创建 chain；
4. 状态为 `EnablingProxy`，options 仍 off；
5. 第一个完整有效 frame 才提交 tags 并在目标 Present 前 options-on。

F5 只在非 benchmark 交互窗口中处理：

- OffNative -> EnablingProxy：完整重建到 loaded chain；
- Enabling/On/Suspended -> OffNative：options-off + null tags + wait + drop chain + unload + recreate；
- Unavailable：只打印一次明确原因，不改变状态；
- 重复键事件必须按物理按下边沿处理，不能因 key repeat 连续重建。

F5 是显式边界，允许一个 idle wait；steady-state render 的 `gpu_idle_wait_count` 仍必须为 0 增量。

建议提交：

`feat(stage11): add frame generation lifecycle state machine`

## 7. 工作包 C3：FG 输入、options-on 与真实 Present

### 7.1 guides 选择

每帧只在 `DebugView::Final` 且 resolved reconstruction 有效时提供 FG 输入：

| 路径 | Depth | Motion |
|---|---|---|
| DLSS SR/DLAA + SVGF/NRD | `generation.dlss.depth` | `generation.dlss.motion` |
| DLSS RR | `generation.rr.depth` | `generation.rr.motion` |

HUD-less color 始终是 output-resolution `generation.display_output`。不使用 gbuffer motion，不使用
specular motion，不新增副本。

### 7.2 Tag 的记录点与资源状态

在 ToneMap 和 `display_output -> swap-chain render target` copy 已记录完成后、command list Close 前：

1. depth、motion 保持其当前 `NON_PIXEL_SHADER_RESOURCE` tracked state；
2. `display_output` 保持 `COPY_SOURCE`；
3. 用 tracked state 创建三个 `eValidUntilPresent` tag；
4. 同次调用追加 `eUIColorAndAlpha`、`eUIAlpha` 的 null tag；
5. full-frame 模式提交 null backbuffer resource + 全输出 extent，不重复持有 swap-chain buffer；
6. 调用 `slSetTagForFrame` 时使用本帧唯一 token、active reconstruction viewport 和当前 command list；
7. 在 Present 返回前，不再把 `display_output` 切回 UAV，也不重用 depth/motion；下一 application
   frame 在第一次写入前由现有 tracker 正常转回目标状态。

因此现有无条件的：

```text
display_output: COPY_SOURCE -> UNORDERED_ACCESS（Present 前）
```

必须改为 FG 有 live `eValidUntilPresent` tags 时保持 `COPY_SOURCE`，FG off 时维持现有行为。注释解释
这是 resource lifecycle 要求，不要写“为了通过测试”。

### 7.3 options 与 Present 的严格顺序

每个有效 application frame：

1. 获取一次 frame token，并按现有 Reflex/PCL 顺序开始 frame；
2. 用同 token/viewport 调用一次 v2.12.0 实际 `sl::Constants`；
3. 记录 SR/RR evaluate、ToneMap、swap-chain copy 和 FG tags；
4. Close/Execute application command list；
5. `RenderSubmitEnd`；
6. 若状态为 Enabling/Suspended 且本帧 guides 完整，调用
   `slDLSSGSetOptions(eOn, numFramesToGenerate=1)`；OnProxy 也保持 options 与目标 Present 明确对账，
   但不要在多个线程重复提交；
7. `PresentStart`；
8. proxy `Present` 恰好一次；
9. `PresentEnd`；
10. 调用一次无 estimate options 的 `slDLSSGGetState` 做状态健康检查。

状态查询规则：

- `status_raw == eOk`：保持 on；
- 首次 `numFramesActuallyPresented >= 2` 时只输出一次确认诊断；
- 前几个 warm-up Present 只有 1 帧可以容忍，不能立即判故障；
- `status_raw != eOk`：记录 raw bitmask，进入 `FaultPendingDisable`，下一安全边界执行 options-off、null
  tags、wait、unload 和 off-chain 重建；不能继续显示 active；
- 显式 startup-on 在合理的有界 warm-up 内始终失败应返回错误；交互 F5-on 失败则安全回退 off 并报告。

本包只保存 last raw state 和一次确认诊断，不累计正式 FPS/generated/dropped 统计，不改变 benchmark
schema。

### 7.4 无有效输入时暂停

以下情况不能提交旧 tags：

- 非 Final debug view；
- resize/minimized/0×0；
- reconstruction viewport 或 generation 正在切换；
- 没有 DLSS SR/RR depth/motion；
- shutdown；
- device/SDK 报告 FG status 错误。

如果 chain 仍需保持 loaded（例如 F1 临时 debug view 或同尺寸 generation 切换），进入
`SuspendedProxy`：options-off、使用 last token 清空 tags，不做 steady-state wait。恢复 Final 且新一帧
guides 完整时重新 tag，再 options-on。资源即将被销毁或 chain/viewport 要替换时，null tags 后必须走
显式 wait。

建议提交：

`feat(stage11): tag inputs and present DLSS generated frames`

## 8. 工作包 C4：resize、generation、切换与退出收口

### 8.1 Resize 与最小化

FG requested on 且 chain loaded 时，resize 流程必须：

1. options-off；
2. null tags；
3. 进入 `SuspendedProxy`；
4. 执行现有 resize 的一次有界 wait；
5. reset command list、释放 backbuffers；
6. 在 FG 仍 loaded 的同一 chain 上调用 hooked `ResizeBuffers`，重建 generation/viewport/backbuffers；
7. 保持 options-off，直到新尺寸首个完整 frame 再恢复。

如果 resize 期间用户 requested 已改 off，走完整 chain drop -> unload -> recreate，不在 proxy chain 上
长期保持 off-screen 开销。

收到 0×0/minimize 时立即停止 Present，options-off 并清 tags；不创建 0 尺寸资源。恢复非零尺寸走同一
resize 入口。不要从 window callback 并发调用 Streamline/DXGI；所有转换仍在应用线程串行完成。

### 8.2 F1/F2/F3/F4 与 generation

- F1 非 Final：暂停 FG；回到 Final 后首个完整 frame 恢复。
- F2/dynamic resolution：如果只改变 render extent，先暂停并清 tag，沿用 generation transaction，
  新 depth/motion 生效后恢复；不得让 SL 持有 retired generation。
- F3/F4：若目标组合仍能提供 DLSS SR/RR guides，使用同一暂停/viewport-generation transaction；若
  目标为 Native 或其他不兼容组合，拒绝切换并提示先按 F5 关闭 FG。不要静默关闭用户显式请求。
- shader hot reload 不替换 FG 输入资源时不重建 chain；若本帧因 reload 失败没有完整输出，则暂停。
- capture 继续捕获 application `display_output`，不是 generated frame。capture frame 的 guides 有效时
  可正常 Present；退出前仍必须 null tags。

### 8.3 Drop 顺序

在现有显式 Drop 中加入且保持注释：

1. 停止提交新帧；
2. 若 FG loaded，options-off；
3. 若 tags live，以 last token/viewport 提交 null tags；
4. wait GPU；
5. free active/retired Streamline viewport resources；
6. `slShutdown()`；
7. 释放 linked proxy swap-chain/queue/factory/device；
8. 释放 native owners 和剩余 renderer resources。

已知目标机在 `NVSDK_NGX_D3D12_Shutdown1(nullptr)` 的 telemetry shutdown 内可能发生厂商侧 hang。
不得改变上述正确顺序来绕开它；测试报告将“渲染/FG 证据”与“clean exit”分别记录。

建议提交：

`fix(stage11): harden frame generation lifecycle boundaries`

## 9. 测试与验收

### 9.1 纯单元与源码契约

至少覆盖：

1. C/Rust ABI 均为 v8，`cameraPinholeOffset` 后的新版 layout 与 size 一致，不再接受旧 v7 layout；
2. C/Rust constants ABI 与锁定的 v2.12.0 `sl_consts.h` 一致，不存在被 bridge 静默丢弃的字段；
3. HUD-less format 映射到 `DLSSGOptions::hudLessBufferFormat`；
4. on options 固定 1 个 generated frame、UI recomposition=false、D3D12 默认 queue mode；
5. null tag 合法、out/错误契约正确，全 null 允许 null command list；
6. feature-off 不引用 `slDLSSG*`/`kFeatureDLSS_G`，显式 CLI on 清晰失败；
7. 配置组合：Reflex off、Native、缺 FG feature、RR 非法 upscaler 均被拒绝；
8. 状态机合法路径与错误回退，包括 startup-on、F5 on/off、suspend/resume、fault；
9. swap-chain 重建顺序严格为 off/null -> wait -> release -> load/unload -> recreate；
10. steady-state render 不调用 `wait_for_gpu`；
11. depth/motion/HUD-less/UI tag 使用同 token/viewport，lifecycle 为 valid-until-present；
12. FG tags live 时 `display_output` 在 Present 前不切回 UAV；
13. 每 application frame 只有一次 common constants、一次 PresentStart/End、一次 proxy Present；
14. 不存在第二次 interface upgrade、手工 presentCommon、Dynamic MFG、FG 专用 guide copy；
15. Drop 在 free resources/slShutdown 前 options-off + null tags。

不要用大量脆弱的完整源码字符串断言替代可执行纯函数/状态机测试；source-contract 测试只锁无法直接
执行的 ABI/顺序边界。

### 9.2 五种 feature 短矩阵

运行：

```powershell
cargo fmt -- --check
cargo test --all-targets --locked --quiet
cargo test --all-targets --locked --quiet --features streamline
cargo test --all-targets --locked --quiet --features streamline-rr
cargo test --all-targets --locked --quiet --features streamline-fg
cargo test --all-targets --locked --quiet --features streamline-rr,streamline-fg
cargo clippy --all-targets --locked -- -D warnings
cargo clippy --all-targets --locked --features streamline-rr,streamline-fg -- -D warnings
git diff --check
```

任何 CMake/build 失败必须修复根因；不得删除 feature-off 测试或让 `streamline-fg` 隐式启用 RR。

### 9.3 RTX 4060 Laptop 有界 smoke

只做最多两个 1 秒 Release case，单进程外层 timeout ≤ 60 秒：

1. `streamline-fg` + SVGF + DLSS Quality + Reflex on + FG on；
2. `streamline-rr,streamline-fg` + RR Quality + Reflex on + FG on。

要求在退出阶段之前取得：

- GPU 为 RTX 4060 Laptop；
- `fg_supported=1`；
- chain 创建时 `fg_loaded=1`；
- 状态经过 `EnablingProxy -> OnProxy`；
- `status_raw=0`；
- 至少一次 `numFramesActuallyPresented >= 2`；
- application Present/token/PCL 仍为一一对应；
- steady-state `gpu_idle_wait_count` 增量为 0；
- RR case 中 RR 真正 active，hidden NRD/SVGF pass 不出现；
- D3D12 Debug InfoQueue 没有本包新增错误。

若进程在输出上述证据后只卡于已知
`NVSDK_NGX_D3D12_Shutdown1 -> Sending Telemetry Shutdown Data`，记录为
`RENDER/FG PASS, CLEAN EXIT BLOCKED BY NVIDIA SHUTDOWN`，附日志与堆栈，不得把它写成 clean-exit PASS，
也不得修改 shutdown 逻辑绕过。若在产生 FG 证据前超时则 smoke FAIL。

HAGS、驱动、显示链或 SDK support 不满足时记录真实 external blocker；不能把 support result 硬编码为
成功。禁止 30/600/1800 秒测试。

### 9.4 Review 硬门槛

- 工作树干净，提交可逐个 review；
- 默认/feature-off 行为不变，FG 默认 off；
- startup-on 不做多余 off-chain 创建/重建；
- F5 off 最终在 FG unloaded 状态重建 chain；
- resize/minimize 前先 options-off + null tags；
- live tags 不引用 retired generation/viewport；
- FG 输入无副本，格式、extent、state、lifecycle 正确；
- same token/viewport/constants/markers/Present 契约正确；
- 至少一条 actual-presented 真实证据，不能只看 options；
- 没有 steady-state GPU idle wait；
- shutdown 厂商 hang 被如实隔离记录；
- 没有提前实现 11G-D/E。

## 10. 提交拆分

不要 squash，建议 4 个提交：

1. `feat(stage11): add frame generation config and ABI v8`
   - 强类型 CLI、组合验证、constants/HUD-less/null-tag ABI 与测试；
2. `feat(stage11): add frame generation lifecycle state machine`
   - 状态机、startup/F5、chain 重建与显式 idle 边界；
3. `feat(stage11): tag inputs and present DLSS generated frames`
   - SR/RR guide 复用、HUD-less/UI null tags、options/present/state 查询；
4. `fix(stage11): harden frame generation lifecycle boundaries`
   - resize/minimize/F1–F4/generation/Drop、回归测试与本文件状态记录。

每个提交必须至少编译其修改涉及的 feature；最后运行完整短矩阵。完成后停止，等待 Codex review。

## 11. 交给 Luna 的提示词

```text
请在 C:\zjk\projects\RayTracingDemo 基于当前 HEAD 严格实施：

docs/阶段11FrameGeneration-11G-C-输入选项与生命周期执行方案.md

必须先完整阅读该文档以及其中列出的本地 Streamline v2.12.0 指南。只做 11G-C；11G-A/B 和 Codex
review 修复已经完成，不要重新设计 linked interposer，不要再次 slUpgradeInterface，不要改 SDK lock、
下载脚本、DLL feature-set 部署或 build.rs。

实现默认 off 的 FrameGenerationMode、--frame-generation off|on 和交互 F5。将 bridge ABI 升到 v8，
补齐 DLSSGOptions::hudLessBufferFormat 和 `Constants::cameraPinholeOffset`，并让 frame-based resource
tag 支持 NVIDIA 要求的 null-tag 释放与 null backbuffer extent。不要向 v2.12.0 实际
`sl::Constants` 添加头文件中不存在的成员。使用单一显式状态机管理
startup、F5、suspend/resume、fault；不要用多个漂移 bool。

FG 只复用已有 DLSS SR/RR depth、motion 和 ToneMap 后 output-resolution display_output；不要新增输入
副本。三个输入都用 eValidUntilPresent，同次清空 UIColorAndAlpha/UIAlpha，并以 null resource +
全输出 extent 标记 full-frame backbuffer。tag 必须在 ToneMap 与 swap-chain copy 记录后、Close 前提交，资源 state 来自 tracker。
live tags 存在时 display_output 在 Present 前必须保持 COPY_SOURCE，下一 application frame再转 UAV。

同一 application frame 只取得一个 token、只提交一次 common constants、一次 PresentStart/End 和一次
proxy Present。FG options-on 在 Present 线程、完整 tags 之后、目标 Present 之前；第一版固定只生成
1 个中间帧。generated frame 不运行任何 render/camera/animation/SPP/history。每次 Present 后只做
健康状态查询；失焦期间不得消耗 warm-up 预算，聚焦后至少输出一次 status=0 且
numFramesActuallyPresented>=2 的真实确认。不得用硬编码 Reflex limiter 掩盖失焦，也不要提前实现
11G-D 的正式 FPS/JSON 累计统计。

F5 off 必须 options-off/null tags/wait/release chain/unload/recreate；startup-on 直接保持 loaded 创建
chain，不能先创建 off chain再多等一次。resize/minimize、F1 debug、F2/F3/F4 generation 和 Drop 严格按
文档处理，live tags 不能引用 retired resources。steady-state 禁止 wait_for_gpu。

按文档拆成 4 个可 review commit，不要 squash。运行五种 feature 测试与两种 clippy；最多两个 1 秒
RTX 4060 Laptop smoke，单进程 timeout 不超过 60 秒，禁止 30/600/1800 秒。已知 slShutdown 可能卡在
NVSDK_NGX_D3D12_Shutdown1：不得跳过 shutdown 或杀进程伪造 clean exit，必须把 render/FG 证据和
clean-exit 结果分开报告。

完成后立即停止并报告：commit hash/作用、全部测试、两个 smoke 的 support/状态转移/status/
actual-presented/token-PCL/idle-wait 证据、shutdown 是否命中已知厂商 hang、未验证项和 git status，
等待 Codex review。不要开始 11G-D/E 或阶段 12。
```
