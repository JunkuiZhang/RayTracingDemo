# 阶段 11：Frame Generation 11G-B Manual Hooking 与交换链边界执行方案

日期：2026-08-29

目标硬件：NVIDIA GeForce RTX 4060 Laptop GPU

基线提交：`14e7ad8 fix(streamline): isolate cached feature plugin sets`

状态：READY FOR LUNA；只实施 11G-B，完成后停止并交给 Codex review

## 1. 本工作包的目标

11G-A 已经完成 Streamline v2.12.0 的 DLSS Frame Generation（DLSS-G）SDK 锁、按 feature-set
隔离的 DLL 部署、真实 adapter support 查询、FG options/state C ABI，以及
`slUpgradeInterface` / `slGetNativeInterface` 的最小桥接。

11G-B 只修正 D3D12/DXGI manual-hooking 创建边界：

1. `CreateCommandQueue` 必须由升级后的 proxy device 调用；
2. `CreateSwapChainForHwnd` 必须由升级后的 proxy factory 调用；
3. swap chain 同时持有 proxy/native 两个明确所有权的接口；
4. Streamline hook 列表中的 swap-chain 调用统一走 proxy，其他调用走 native；
5. FG 插件在默认关闭时必须于 swap-chain 创建前卸载，不能产生 DLSS-G off-screen backbuffer、额外
   present queue 或 copy/synchronization 开销；
6. 集中创建、resize、释放和 shutdown 顺序，为 11G-C 的显式 on/off 重建状态机留出唯一入口。

本包仍然**不生成中间帧**。不新增 `--frame-generation`、F5 开关、FG tags、FG options-on、
generated-frame 统计或 benchmark schema 字段。11G-B 的成功标准是 manual-hooking 路由正确，且所有
现有 default/Streamline/SR/RR 路径行为不回退。

## 2. 已核对的官方契约与当前缺口

本方案以本地锁定的 Streamline v2.12.0 为唯一 SDK 契约来源：

- `external/streamline-v2.12.0/docs/ProgrammingGuideManualHooking.md`
- `external/streamline-v2.12.0/docs/ProgrammingGuideDLSS_G.md`
- `external/streamline-v2.12.0/include/sl_hooks.h`
- `external/streamline-v2.12.0/include/sl_core_api.h`

### 2.1 Manual hooking 必须覆盖的 D3D12/DXGI API

本项目实际使用的 mandatory hook 是：

| 接口 | 本项目调用 | 11G-B 路由 |
|---|---|---|
| `ID3D12Device::CreateCommandQueue` | 启动时一次 | proxy device |
| `IDXGIFactory::CreateSwapChainForHwnd` | 启动；11G-C 将用于切换重建 | proxy factory |
| `IDXGISwapChain::Present` | 每个 application frame | proxy swap chain |
| `IDXGISwapChain::GetBuffer` | 创建/重建 RTV | proxy swap chain |
| `IDXGISwapChain::ResizeBuffers` | resize | proxy swap chain |
| `IDXGISwapChain::GetCurrentBackBufferIndex` | render/resize | proxy swap chain |
| `IDXGISwapChain::SetFullscreenState` | 当前未调用 | 未来若调用必须走 proxy |

项目目前没有调用 `Present1`、`ResizeBuffers1`、`CreateSwapChain` 或
`CreateSwapChainForCoreWindow`；不要为未使用 API 增加抽象。若实现过程中新增其中任何调用，必须同步
加入路由封装和测试。

`MakeWindowAssociation`、adapter 枚举、`GetDesc*` 等不在 hook 列表中的调用继续使用 native
factory/swap chain。第三方 SDK、NVAPI、PIX 和现有 renderer 资源创建继续使用 native device。

### 2.2 当前实现为什么不合格

当前 `Dx12Renderer::new` 的顺序是：

1. native `CreateDXGIFactory2`；
2. native `D3D12CreateDevice`；
3. `slSetD3DDevice`；
4. native device `CreateCommandQueue`；
5. native factory `CreateSwapChainForHwnd`；
6. 对已经创建的 swap chain 调用 `slUpgradeInterface`。

第 6 步足以让当前 proxy `Present` 调到 `presentCommon()`，但不能补回第 4、5 步已经绕过的 creation
hooks。11G-B 必须在接口创建后、任何被 hook 的方法调用前升级对应的 device/factory clone。

### 2.3 loaded 与 on/off 是两个不同状态

`slInit` 请求的 feature 默认 loaded。`slSetFeatureLoaded(kFeatureDLSS_G, false)` 会卸载并解除
DLSS-G hooks；`slDLSSGSetOptions(mode=off)` 只关闭插帧，不消除已加载 DLSS-G 所引入的 off-screen
swap-chain 开销。两者不能混用。

`slSetFeatureLoaded` 的硬约束：

- 只能在 device 已通过 `slSetD3DDevice` 绑定后调用；
- 调用期间不得并发执行 D3D/DXGI API；
- 已有 GPU work 时必须先 flush/wait；
- 未在 `slInit` 请求的 feature 不得加载；
- feature-off bridge 不得 include、引用或调用 `slSetFeatureLoaded(kFeatureDLSS_G, ...)`。

11G-B 启动时没有 GPU work，因此不需要额外 idle wait。正确顺序是：保持 FG loaded，让 proxy device
创建 application command queue；随后在任何 swap-chain API 之前把 FG unload，再由 proxy factory
创建默认关闭的 swap chain。这样 11G-C 以后可以在显式边界重新 load FG 并重建 chain，而当前默认
路径没有 DLSS-G off-screen copy 开销。

## 3. 严格范围

### 3.1 允许修改

- `native/streamline_bridge/include/streamline_bridge.h`
- `native/streamline_bridge/src/streamline_bridge.cpp`
- `src/streamline.rs`
- `src/renderer/d3d12.rs`
- 与上述契约直接相关的 Rust/C++ 单元测试和本执行文档

只有确实需要编译桥接时才最小修改 `native/streamline_bridge/CMakeLists.txt`。本包不应修改
`build.rs`、SDK lock、DLL 部署或下载脚本。

### 3.2 禁止修改

- 不添加 `--frame-generation`、F5 或其他用户开关；
- 不调用 `streamline_bridge_fg_set_options` 的 on 模式；
- 不提交 depth/motion/HUD-less/backbuffer/UI FG tags；
- 不实现 generated frame，不把窗口 FPS 当作 FG 证据；
- 不改 RR、NRD、SVGF、stable planes、路径追踪采样、曝光或 ToneMap；
- 不改 DLSS SR/RR viewport、token、common constants 或资源格式；
- 不增加 steady-state `WaitForGpu`，不把已有 resize wait 移进正常帧；
- 不运行 600/1800 秒测试；单个进程外层 timeout 不超过 60 秒；
- 不提前实施 11G-C、11G-D 或 11G-E。

## 4. 工作包 B1：FG loaded-state C ABI v6

### 4.1 ABI

把 C/Rust ABI 从 5 升到 6。结构体布局不变，新增两个窄接口：

```c
StreamlineBridgeStatus streamline_bridge_fg_set_loaded(
    StreamlineBridge* bridge,
    uint32_t loaded);

StreamlineBridgeStatus streamline_bridge_fg_is_loaded(
    StreamlineBridge* bridge,
    uint32_t* out_loaded);
```

不要暴露任意 `feature_id`，避免 Rust 任意 load/unload DLSS、RR、Reflex 或 PCL。这个 ABI 只管理
`sl::kFeatureDLSS_G`。

### 4.2 错误契约

- 非 `streamline-fg` bridge 构建：两个入口直接返回 `STATUS_UNSUPPORTED`，不得要求有效 bridge；
- FG 构建：`loaded` 只能为 0/1；out pointer 进入函数即预清零；
- bridge 未初始化返回 `NOT_INITIALIZED`；device 尚未绑定返回 `NOT_INITIALIZED`；
- `fg_requested == false` 返回 `UNSUPPORTED`；
- SDK 失败返回 `SDK_ERROR`，`last_error` 必须含 API 名和 raw `sl::Result`；
- 成功后调用 `slIsFeatureLoaded` 复核，不在 C++ 中维护可能漂移的 shadow bool；
- `fg_is_loaded` 只报告 loaded 状态，不能把 `DLSSGOptions::mode` 或 support 混入该值。

注释只解释 loaded 与 mode 的区别、无并发 D3D/DXGI 的调用边界，不逐行翻译代码。

### 4.3 Rust 封装

在 `StreamlineRuntime` 上增加私有的强语义方法，例如：

```text
set_frame_generation_loaded(bool) -> Result<()>
frame_generation_loaded() -> Result<bool>
```

renderer 不直接散落调用 raw FFI。错误继续走现有 `streamline_error_with_detail`，保留 bridge 的
`last_error`。

## 5. 工作包 B2：native/proxy 接口所有权模型

### 5.1 不要复制两个 owner 指向同一个裸指针

新增小型私有 RAII 结构，名称可按现有风格调整，但职责必须集中：

```text
StreamlineDeviceInterfaces
  native: ID3D12Device
  proxy: Option<ID3D12Device>

StreamlineFactoryInterfaces
  native: IDXGIFactory6
  proxy: Option<IDXGIFactory6>

SwapChainInterfaces
  native: IDXGISwapChain3
  proxy: Option<IDXGISwapChain3>
```

原则：

- `native.clone()` 产生独立 AddRef 后才交给 `upgrade_interface`；
- `upgrade_interface` 成功返回的 proxy 只由一个 Rust COM owner 接管；
- `get_native_interface(proxy)` 返回的 AddRef 只由一个 Rust COM owner 接管；
- native/proxy raw pointer 相等是允许的；只有 SDK 明确返回的新 AddRef 才能构造第二个 owner，不能
  从同一个裸引用擅自调用两次 `from_raw`；
- `STATUS_ALREADY_UPGRADED` 不能造成引用泄漏或 double release；
- 任一步失败时，已创建接口通过 RAII 回收，Streamline bridge 仍按现有 constructor guard shutdown；
- 不用 `ManuallyDrop` 掩盖所有权问题，不引入全局 COM registry。

优先把现有 generic `upgrade_interface<T>` 补齐为对称的 `get_native_interface<T>`，并让组合结构只
使用这两个 helper。不要在多个 call site 手写裸指针转换。

### 5.2 Renderer 字段

用组合结构替换当前并列的：

```text
swap_chain
streamline_swap_chain
```

factory 目前是 `new` 的局部变量；11G-B 后应由 renderer 持有 native/proxy factory，供 11G-C 的
受控 chain 重建复用。device 的 native 接口仍是 renderer 的主 device；proxy device 只用于
hooked `CreateCommandQueue`，可以放进集中结构并保持到 Streamline shutdown。

字段和显式 Drop 顺序必须保证：

1. 等待 GPU；
2. 释放 Streamline viewport/feature resources；
3. `slShutdown`；
4. 释放 proxy swap chain/factory/device；
5. 最后由 Rust 正常释放 native DXGI/D3D 对象。

不要依赖“看起来刚好”的字段声明顺序完成 2–4；在现有 `Drop` 中显式 `take/drop` 需要提前释放的
Option，或使用一个清晰的 owner whose shutdown method 明确顺序。

## 6. 工作包 B3：集中创建和 hook 路由

### 6.1 启动顺序

`Dx12Renderer::new` 必须收敛为以下顺序：

1. `slInit(eUseManualHooking)`，仍早于相关 DXGI/D3D hooks；
2. 创建 native factory，立即从独立 clone 升级并保存 proxy factory；adapter 枚举等非 hook API
   继续走 native factory；
3. 创建 native D3D12 device，立即从独立 clone 升级并保存 proxy device；
4. `slSetD3DDevice(native_device)` 并完成真实 support 查询；
5. 由 proxy device 调用唯一一次 application `CreateCommandQueue`；
6. `streamline-fg` 构建调用 `fg_set_loaded(false)` 并用 `fg_is_loaded` 复核；此时尚无 GPU work，
   `gpu_idle_wait_count` 不增加；
7. 由已经升级的 proxy factory 调用 `CreateSwapChainForHwnd`；
8. 保存返回的 proxy swap chain，并用 `get_native_interface` 取得 native swap chain；
9. `MakeWindowAssociation` 走 native factory；
10. backbuffer `GetBuffer` 走 proxy swap chain。

feature-off 构建不创建任何 proxy，不调用 bridge API，保持纯 native 路径。

普通 `streamline`、RR-only、FG-only 和 RR+FG 都使用相同 manual-hooking 路由；区别只在请求/加载的
插件集合。FG-only 与 RR+FG 在本包都必须于 swap-chain 创建前确认 FG unloaded。

### 6.2 集中 helper

不要继续让 `Dx12Renderer::new` 散落 raw creation/upgrade。新增私有 helper，例如：

```text
create_device_interfaces(...)
create_application_command_queue(...)
create_swap_chain_interfaces(...)
```

或一个等价的 `StreamlinePlatformInterfaces::create(...)`。选择能减少参数和所有权跳转的最小设计，
不要建立通用 RHI。

`create_swap_chain_interfaces` 必须接收完整且不可变的 `DXGI_SWAP_CHAIN_DESC1`，统一完成：

- 正确 factory 选择；
- `CreateSwapChainForHwnd`；
- cast 到 `IDXGISwapChain3`；
- proxy/native 配对；
- 错误上下文；
- 不在 helper 内创建 RTV 或等待 GPU。

### 6.3 Hook 调用统一路由

`SwapChainInterfaces` 至少提供语义明确的访问器：

```text
hooked() -> &IDXGISwapChain3
native() -> &IDXGISwapChain3
is_proxied() -> bool
```

以下现有 call site 全部改用 `hooked()`：

- frame index 获取；
- `Present`；
- resize 前 frame index 获取；
- `ResizeBuffers`；
- `create_render_targets` 中的 `GetBuffer`。

不要保留一个容易误用的模糊 `active_swap_chain()` 名称。以后若添加非 hook API，必须显式使用
`native()`。

`presentCommon()` 仍由 proxy `Present` 恰好触发一次。不要新增手工 `presentCommon()` 调用；现有
`reflex_present_common_count` 继续按成功的 application Present 计数，不按 display frame 计数。

### 6.4 Resize 和未来重建边界

11G-B 不实现 FG on/off 切换，因此 resize 仍使用当前安全流程：显式边界 wait、释放 command-list/
backbuffer 引用、proxy `ResizeBuffers`、重取 proxy `GetBuffer`、重置 frame timing 和 history。

但 resize 中所有 swap-chain 操作必须通过组合结构；不得把 native/proxy 重新拆散。新增一个私有的
`release_backbuffers_for_swap_chain_change` 或等价 helper 可以消除启动/resize/11G-C 重建将共享的
重复逻辑，但不要提交一个未被调用的“未来状态机”。

11G-C 才实现真正的：off/null tags → wait → release chain → load/unload FG → recreate chain → 首个
有效输入帧再 options-on。11G-B 只保证当前 owner 和 creation helper 能被该流程复用。

## 7. 诊断要求

不要修改 benchmark JSON schema（统计属于 11G-D）。允许增加简短 stderr lifecycle diagnostic，必须
是一行并可人工核对，例如：

```text
streamline_swap_chain state=created manual_hooking=1 proxy=1 fg_loaded=0 output=1280x720
```

不得打印 COM 地址。至少能区分：

- feature-off native chain；
- Streamline proxy chain；
- `streamline-fg` 编译但 FG 在 chain 创建时 unloaded。

若默认构建不输出该行也可以；测试不能依赖本地化错误全文。

## 8. 测试与验收

### 8.1 单元/源码契约测试

至少覆盖：

1. ABI version C/Rust 同为 6，既有结构 size/offset 不变；
2. feature-off 的 `fg_set_loaded` / `fg_is_loaded` 返回 `UNSUPPORTED`；
3. FG bridge 对 loaded 非 0/1、null out、未 set device 的行为符合第 4.2 节；
4. C++ source contract 确认仅 FG 编译块引用 `slSetFeatureLoaded` / `slIsFeatureLoaded`；
5. `get_native_interface` 失败时 out pointer 仍为 null；
6. native/proxy owner 的 already-upgraded、pointer-equal 和失败路径不会 double-own；
7. 所有当前 hooked swap-chain call site 都通过集中封装；
8. feature-off source/编译路径不引用 `RAY_TRACING_STREAMLINE_PLUGIN_SUBDIR` 之外的新 FG 符号；
9. 不存在 FG options-on、FG tags、`--frame-generation` 或 F5 新逻辑。

如果 COM ownership 无法用纯单元测试真实观察，可增加一个极小的 Windows-only integration test，创建
factory/device 后验证 upgrade → get-native → drop 顺序；测试失败必须能在 60 秒内退出。不要通过大量
脆弱的字符串断言替代可执行测试。

### 8.2 五种 feature 矩阵

运行：

```powershell
cargo test --all-targets --locked --quiet
cargo test --all-targets --locked --quiet --features streamline
cargo test --all-targets --locked --quiet --features streamline-rr
cargo test --all-targets --locked --quiet --features streamline-fg
cargo test --all-targets --locked --quiet --features streamline-rr,streamline-fg
```

不得删除 `streamline-plugins/base|rr|fg|rr-fg` 中其他缓存功能集。按 11G-A 的不可变目录契约核对
DLL；本包不应改变 DLL 矩阵。

### 8.3 RTX 4060 Laptop 短 smoke

提交后用干净 Release 构建运行最多两个 1 秒现有 benchmark：

1. `--features streamline-fg` + SVGF + DLSS Quality；
2. 可选 `--features streamline-rr,streamline-fg` + RR Quality，仅当第一个 smoke 无法覆盖
   `presentCommon`/RR 回归时运行。

要求：

- exit code 0，外层 timeout ≤ 60 秒；
- `git_dirty=false`；
- GPU 名称为 RTX 4060 Laptop；
- FG support 仍来自 SDK 真查询；
- diagnostic 显示 chain 创建时 `fg_loaded=0`；
- `gpu_idle_wait_count == 0`（正式 benchmark 区间）；
- `present_common_count == application Present/token count`；
- RR smoke 中 RR 仍真实 active，hidden NRD/SVGF pass 不得出现；
- 不宣称 FG 已工作，不要求显示 FPS 翻倍。

只做短测，不运行 30/600/1800 秒 benchmark，不做 FrameView 长捕获。

### 8.4 Review 硬门槛

以下全部满足才算 11G-B 完成：

- `git diff --check` 通过且工作树干净；
- feature-load ABI 的 feature-off/failure/out-pointer 契约正确；
- queue creation 确实走 proxy device；
- swap-chain creation 确实走 proxy factory；
- hooked swap-chain API 全部走 proxy，non-hook API 有明确 native 入口；
- FG 在默认 swap-chain 创建前已 unload，未引入 off-screen copy 开销；
- `presentCommon()` 每 application frame 仍恰好一次；
- 没有新增 steady-state GPU idle wait；
- 五种 feature 测试和短 smoke 通过；
- 没有提前实施 11G-C/D/E。

## 9. 提交拆分

不要 squash，建议 3 个提交：

1. `feat(stage11): expose frame generation load state ABI v6`
   - C/Rust ABI、feature isolation、错误契约与单元测试；
2. `refactor(stage11): own Streamline native and proxy interfaces`
   - device/factory/swap-chain RAII 组合、generic native helper、shutdown 顺序；
3. `feat(stage11): route manual hooks through Streamline proxies`
   - queue/swap-chain 集中创建、FG 默认 unload、hook call-site 迁移、短诊断与回归测试。

若第 3 个提交过大，可以再拆出：

4. `test(stage11): lock manual-hooking swap chain boundary`

不要把机械重命名、ABI、生命周期和测试全部压成一个提交。每个提交都必须能编译；最后一个提交后
停止，等待 Codex review。

## 10. 交给 Luna 的提示词

```text
在 C:\zjk\projects\RayTracingDemo 基于当前 HEAD 实施
docs/阶段11FrameGeneration-11G-B-ManualHooking与交换链边界执行方案.md。

只做阶段 11G-B。基线 11G-A 已完成，不要改 SDK lock、DLL 部署和 build.rs。把 Streamline bridge ABI
升到 v6，新增只管理 kFeatureDLSS_G 的 loaded-state set/query；非 streamline-fg 构建必须返回
UNSUPPORTED。按 Streamline 2.12 manual-hooking 契约，用 proxy device 创建 application command
queue，用 proxy factory 创建 swap chain，同时以明确 RAII 所有权保存 proxy/native swap-chain。
Present、GetBuffer、ResizeBuffers、GetCurrentBackBufferIndex 等已用 hook API 必须统一走 proxy，
non-hook API 走 native。

本包 FG 仍默认关闭：slInit 请求的 FG plugin 先保持 loaded 直到 proxy CreateCommandQueue 完成，然后
必须在任何 swap-chain API 之前受控 slSetFeatureLoaded(kFeatureDLSS_G, false) 并复核 unloaded，避免
off-screen backbuffer、额外 present queue 和 copy/synchronization 开销。启动时尚无 GPU work，不得
因此增加 idle wait。保持 presentCommon 每个 application Present 恰好一次，并保证 Streamline
shutdown 早于 proxy/native DXGI/D3D 对象销毁。

禁止开始 11G-C：不要添加 --frame-generation/F5，不要提交 FG options-on、FG tags、generated
frames 或统计，不要改 RR/NRD/SVGF/stable planes/曝光/ToneMap/benchmark schema。按文档建议拆成
3–4 个可 review commit，不要 squash；写解释 loaded-vs-mode、COM ownership 和 shutdown 顺序的
必要注释。

运行文档规定的五种短测试矩阵，提交后最多做两个 1 秒 RTX 4060 Laptop smoke，单进程 timeout
不超过 60 秒；禁止 30/600/1800 秒长测。完成后停止，报告 commits、测试命令与结果、smoke 中
fg_loaded/presentCommon/gpu_idle_wait 证据、未验证项和 git status，等待 Codex review。
```
