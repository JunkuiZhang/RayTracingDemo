# 阶段 11：Frame Generation 11G-A SDK 部署与 ABI 执行方案

日期：2026-08-20

状态：COMPLETED AND REVIEWED；实现与修复提交范围 `929fc15..14e7ad8`

目标设备：NVIDIA GeForce RTX 4060 Laptop GPU

固定 SDK：Streamline v2.12.0

基线提交：`c726112 docs(stage11): close stable-plane production acceptance`

## 1. 本工作包的目标

本工作包只建立 DLSS Frame Generation（Streamline 名称为 `DLSS-G`，feature 为
`sl::kFeatureDLSS_G`）的可编译基础：

1. 将 FG 头文件和运行库纳入现有的 SDK hash、签名来源和按 feature 部署契约；
2. 把 Rust/C++ bridge ABI 从 v4 升到 v5，加入 FG support/options/state 和 native-interface
   能力；
3. 让 `streamline-fg` 构建能够在创建 D3D12 device 后查询真实 adapter support；
4. feature-off 时不编译 `sl_dlss_g.h`、不引用 `kFeatureDLSS_G`、不部署或加载 FG DLL；
5. 用短构建和单元测试证明能力隔离，没有改变现有渲染输出与 swap chain。

本包**不会生成任何中间帧**。不添加 `--frame-generation` 参数，不 upgrade device/factory，
不重建 swap chain，不提交 FG 资源标签，不改 Present、RR/NRD/SVGF、stable planes、曝光或
ToneMap。真正的 proxy 创建边界属于下一包 11G-B。

## 2. 以本地 SDK 为准的契约修正

早期总方案中的 `slSetFeatureLoaded` 不纳入本包，也不能把它当成 FG 的普通 on/off 开关。
Streamline v2.12 官方指南明确区分：

- 普通运行时开关使用 `slDLSSGSetOptions` 的 `DLSSGMode::eOff/eOn`；
- `slSetFeatureLoaded` 会完全卸载并 unhook plugin，只用于多 swap-chain 等高级场景；
- 本项目是单窗口、单 swap-chain，并采用 manual hooking，11G-B 会只 upgrade 明确选中的
  device/factory/swap chain，因此没有必要引入第二套 feature-loaded 状态机。

另有以下强制契约：

- `slInit()` 仍早于任何 DXGI/D3D12 API；
- `slShutdown()` 仍早于销毁 DXGI/D3D12 components 和 upgraded proxy；
- support 只能来自目标 adapter LUID 的 `slIsFeatureSupported(kFeatureDLSS_G, adapter)`，禁止按
  GPU 名称推断；
- 加载 FG plugin、查询 support 或设置 tags 都不会自动开启插帧；11G-C 才能调用
  `slDLSSGSetOptions(eOn)`；
- 普通每帧 `slDLSSGGetState` 必须传 `nullptr` options；只有初始化/resize 的一次性 VRAM
  estimate 查询可以传带 `eRequestVRAMEstimate` 的完整 options。

## 3. Cargo 与构建隔离

### 3.1 Cargo feature

在 `Cargo.toml` 新增：

```toml
streamline-fg = ["streamline"]
```

正交性必须保持：

- `streamline-rr` 不隐式启用 `streamline-fg`；
- `streamline-fg` 不隐式启用 `streamline-rr`；
- 完整目标组合是 `--features streamline-rr,streamline-fg`；
- `default`、`streamline`、`streamline-rr` 的部署结果中均不得出现 `sl.dlss_g.dll` 或
  `nvngx_dlssg.dll`。

### 3.2 version lock

在 `third_party/streamline/version.lock.json` 中加入下列 `optional_feature` 条目。hash 已从仓库
当前固定的官方 v2.12.0 archive 复核：

| path | sha256 |
|---|---|
| `include/sl_dlss_g.h` | `1fc18cbe004e280df1f787276d08a1b28b8a8c4c65856fbaa659f56dff6a915d` |
| `bin/x64/sl.dlss_g.dll` | `1fec3f8fdfc59d78c4445c276c1a0fb798bf251985f348597dc2b44d0c995e52` |
| `bin/x64/nvngx_dlssg.dll` | `135eaf0733c1e37381a8c28abcf7a862404a54132b81787c04e35d09efc5e36f` |
| `bin/x64/development/sl.dlss_g.dll` | `fa1f72c54ea3efc92baa5574d112e7b04c4d10949d394c8ec9a0aeba51963a3e` |
| `bin/x64/development/nvngx_dlssg.dll` | `0d33b5de65d60a943c33bd096d574297302b214a1c5baa6bcb74bc1150a608de` |

每项都写 `"optional_feature": "streamline-fg"`。SDK archive 没有独立
`nvngx_dlssg.license.txt`，不得虚构许可证；继续使用已经锁定和部署的 SDK 总许可证、third-party
notices 与 `nvngx_dlss.license.txt`。

### 3.3 `build.rs`

把当前 `rr_enabled: bool` 参数扩展为能分别表达 RR/FG 的显式配置，禁止用一个模糊的
`streamline_optional_enabled`：

- 监听 `CARGO_FEATURE_STREAMLINE_FG`；
- lock validator 分别跳过未启用的 `streamline-rr` 与 `streamline-fg` 条目，未知
  `optional_feature` 必须报错，不能默认验证或默认跳过；
- 仅 FG feature 开启时验证/部署当前 profile 对应的 `sl.dlss_g.dll` 和
  `nvngx_dlssg.dll`；
- 给 CMake 传 `-DSTREAMLINE_ENABLE_FG=ON/OFF`；
- production 只复制 `bin/x64`，Debug 只复制 `bin/x64/development`；
- 现有 copy-if-changed、输出目录定位、SDK 版本和 hash 校验方式保持不变。

实现收口补充：Cargo 会缓存 build-script，不能靠切 feature 时删除另一个构建的 DLL 再复制回来。
`sl.interposer.dll` 保持在 EXE 同目录，其余 plugin 按
`streamline-plugins/base|rr|fg|rr-fg` 隔离，并由编译进可执行文件的相对目录选择。各目录不可
互删；正式验收仍使用独立 `CARGO_TARGET_DIR`，但普通开发目录连续切 feature 也不得扫描错误的
可选 plugin。

不要修改 `fetch_streamline.ps1` 的下载来源、archive hash 或签名验证；lock 新条目应被现有下载后
校验自然覆盖。若脚本目前对 optional feature 有独立过滤，才做最小的 RR/FG 泛化并补测试。

## 4. Bridge ABI v5

### 4.1 版本与布局

同步将 C 的 `STREAMLINE_BRIDGE_ABI_VERSION` 和 Rust 的 `ABI_VERSION` 改为 `5`。所有跨 ABI
结构继续使用固定宽度类型、`#[repr(C)]`、`struct_size`、`abi_version`；C++ `static_assert` 与 Rust
`size_of` 测试必须一一对应。

在 `StreamlineBridgeInitDesc`/`InitDesc` 增加 `enable_dlss_fg: uint32_t`，推荐放在最后一个
feature enable 字段之后、指针之前。在 `StreamlineBridgeSupport`/`Support` 增加：

```text
fg_supported: uint32_t
fg_result: uint32_t
```

不要只断言总 size；再用 C++ `offsetof` 和 Rust `offset_of!` 至少锁住所有新增字段和首个指针/
`adapter_luid` 的偏移，避免 C/Rust 恰好同 size 但顺序不同。

### 4.2 FG options

新增 `StreamlineBridgeFrameGenerationOptions` / Rust `FrameGenerationOptions`，只暴露第一版需要的
字段：

```text
struct_size, abi_version
mode                         // 0=off, 1=on；拒绝 auto/dynamic
num_frames_to_generate       // 第一版只能是 1
flags                        // 正常 set-options 必须为 0
num_back_buffers
mvec_depth_width, mvec_depth_height
color_width, color_height
color_buffer_format
mvec_buffer_format
depth_buffer_format
```

不暴露 dynamic target、Vulkan queue parallelism、HUD/UI recomposition、API error callback 或 MFG
倍数。`mode=on` 时尺寸/format/backbuffer count 必须非零；`mode=off` 允许用同一份完整描述，以便
11G-C 在 suspend/resize 前显式关闭。

C++ 转换函数只能生成：

- `DLSSGMode::eOff/eOn`；
- `numFramesToGenerate = 1`；
- `queueParallelismMode = eBlockPresentingClientQueue`；
- `enableUserInterfaceRecomposition = eFalse`；
- 不设置 dynamic-resolution/MFG flags。

### 4.3 FG state

新增 `StreamlineBridgeFrameGenerationState` / Rust `FrameGenerationState`：

```text
struct_size, abi_version
status_raw
min_width_or_height
num_frames_actually_presented
num_frames_to_generate_max
estimated_vram_usage_bytes: uint64_t
vsync_support_available
dynamic_mfg_supported          // 只诊断，不允许启用
```

不向 Rust 暴露 `inputsProcessingCompletionFence`。v2.12 的 D3D12 第一版固定
`eBlockPresentingClientQueue`，且项目只在 presenting queue 上复用输入；把该 fence 暴露出来会在尚无
Vulkan/并行 queue 模式时制造第二套无用同步协议。

### 4.4 C 函数

新增：

```c
StreamlineBridgeStatus streamline_bridge_fg_set_options(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeFrameGenerationOptions* options);

StreamlineBridgeStatus streamline_bridge_fg_get_state(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeFrameGenerationOptions* estimate_options,
    StreamlineBridgeFrameGenerationState* out_state);

StreamlineBridgeStatus streamline_bridge_get_native_interface(
    StreamlineBridge* bridge,
    void* proxy_interface,
    void** out_native_interface);
```

`estimate_options == nullptr` 表示低开销状态/计数查询；非 null 时 bridge 必须验证完整 options，复制
后只添加 `eRequestVRAMEstimate` 再调用 SDK。调用方不能直接把该 flag 塞进普通 set-options。

`get_native_interface` 是 11G-B 会使用的 manual-hooking primitive，本包只实现 ABI、错误传播和
null/invalid 测试，不在 renderer 中调用。输出指针进入函数时必须先置 null；成功时遵循 COM 返回
引用语义，由调用方拥有并释放。

feature 未编译时三个 FG 函数都返回稳定的 `STATUS_UNSUPPORTED`，不能引用 FG header/helper/
feature symbol。`get_native_interface` 属于 Streamline core，不受 FG 宏限制，但必须要求 bridge 已
初始化且参数非 null。

## 5. C++ 实现约束

在 CMake 新增 `STREAMLINE_ENABLE_FG`，并只在该宏为 1 时 include `sl_dlss_g.h`。

不要继续扩展当前依赖“optional feature 总在数组尾部”的截尾算法。用固定容量数组加显式 count
按请求追加 feature：Reflex、PCL、普通 DLSS、可选 RR、可选 FG。这样 RR/FG 四种组合都不会错载
另一个 plugin。`enable_dlss_fg != 0` 而 bridge 未编译 FG 时，create 必须返回
`STATUS_UNSUPPORTED` 并写清楚 `请启用 streamline-fg`。

adapter support 结果使用具名 index/字段，不要靠裸 `support_results[4]` 在各函数重复猜顺序。
`set_d3d_device` 对已编译且已请求的 FG 调用真实 `slIsFeatureSupported`；未请求或未编译时写入
SDK 的 feature-not-supported raw result。`query_support` 只做结构化抄写，不能二次按 GPU 名称
推断。

`fg_set_options`、`fg_get_state` 和 `get_native_interface` 的所有 SDK 错误都走现有
`set_error`，保留 raw `sl::Result` 和调用名。不要 catch 后吞错，也不要在 production stdout
打印日志。

## 6. Rust 接线边界

`src/streamline.rs` 同步 ABI v5、extern declarations、布局测试和常量。新增最小常量：

```text
FRAME_GENERATION_OFF = 0
FRAME_GENERATION_ON = 1
```

不要在本包创建完整 FG runtime state machine。

`StreamlineRuntime::create_before_dxgi` 增加 `enable_fg` 参数，调用处只传
`cfg!(feature = "streamline-fg")`；这只让 feature 构建在 `slInit` 请求/加载 FG plugin，不代表
运行时开启插帧。`attach` 的 support 记录应能保存真实 FG support/result，但本包不因 unsupported
而让普通 `--features streamline-fg` 启动失败，因为 CLI 还没有显式请求 on。11G-C 加入显式
`--frame-generation on` 后才执行 fail-fast。

除上述 init/support plumbing 外，不修改 `Dx12Renderer` 的 swap chain、Present、resize、F3/F4、
frame token、SPP 或 benchmark schema。若为了证明 support 而必须改 JSON，本包停止并交给 Codex
决定，Luna 不自行扩 schema。

## 7. 测试与验收

### 7.1 必须补的自动测试

Rust/C++ source 或可执行测试至少覆盖：

1. ABI version 为 5，所有 C/Rust size 和新增字段 offset 一致；
2. feature-off 的 FG API 返回 `UNSUPPORTED`，不加载 SDK 也可完成 invalid-argument 测试；
3. `enable_dlss_fg=1` 在非 FG bridge 构建中被拒绝；
4. mode 非 off/on、`num_frames_to_generate != 1`、缺失尺寸/format/backbuffer 被拒绝；
5. `fg_get_state(nullptr estimate_options)` 和带 estimate options 走不同 SDK 参数契约，可用 source
   contract test 锁住；
6. `get_native_interface` 把 out pointer 预清空，null proxy/out 被拒绝；
7. build lock 对未知 optional feature fail closed；
8. RR-only 与 FG-only 的 feature list/部署清单互不污染。

不要通过测试私有实现文本的每个细节；source contract test 只用于无法在无 GPU 单测中观察的
SDK 调用边界。

### 7.2 构建矩阵

使用独立 `CARGO_TARGET_DIR` 或在每次构建前检查目标 DLL，至少执行：

```powershell
cargo test --all-targets --locked --quiet
cargo test --all-targets --locked --quiet --features streamline
cargo test --all-targets --locked --quiet --features streamline-rr
cargo test --all-targets --locked --quiet --features streamline-fg
cargo test --all-targets --locked --quiet --features streamline-rr,streamline-fg
```

再分别检查对应 profile 输出目录的 DLL：

| feature | RR DLL | FG DLL |
|---|---:|---:|
| default | 无 | 无 |
| streamline | 无 | 无 |
| streamline-rr | 有 | 无 |
| streamline-fg | 无 | 有 |
| streamline-rr,streamline-fg | 有 | 有 |

允许做一个 `--features streamline-fg` 的 1 秒现有 benchmark，证明只查询 support、不改变渲染与
Present；外层 timeout 不超过 60 秒。不得运行 600/1800 秒测试，不得宣称 FG 已工作，不得以窗口
FPS 作为验收。

### 7.3 review 硬门槛

以下全部满足才算 11G-A 完成：

- 工作树只含本包范围内文件；
- `git diff --check` 通过；
- 五种构建组合通过；
- DLL 矩阵准确；
- default/streamline/RR 行为无回退；
- renderer 仍没有 FG proxy swap chain、FG tags 或 FG options-on 调用；
- 注释解释 feature 隔离、普通 on/off 与 load/unload 的区别、低开销 state query，不逐行复述；
- 实现和测试证据拆成可 review 的提交。

## 8. 提交拆分与停止点

建议三个提交，不要 squash：

1. `build(stage11): pin optional DLSS frame generation runtime`
   - Cargo feature、lock、build.rs、CMake feature flag 与 DLL 部署；
2. `feat(stage11): expose DLSS frame generation bridge ABI v5`
   - C header/C++、Rust FFI/init/support/native-interface；
3. `test(stage11): lock frame generation capability isolation`
   - ABI/负例/feature matrix 测试和必要的简短文档更新。

完成第三个提交后立即停止，向 Codex 报告提交范围、命令结果、DLL 清单和任何未验证项。不要开始
11G-B，不要修改 swap chain。

## 9. 交给 Luna 的提示词

```text
在 C:\zjk\projects\RayTracingDemo 基于当前 HEAD 实施
docs/阶段11FrameGeneration-11G-A-SDK部署与ABI执行方案.md。

只做 11G-A：锁定 Streamline v2.12.0 的 DLSS-G 头文件/DLL，新增正交的 streamline-fg Cargo
feature，把 Rust/C++ bridge ABI 升到 v5，接入真实 adapter FG support/options/state 和
get_native_interface 的最小 ABI。禁止开始 11G-B：不要 upgrade device/factory/swap chain，不要改
Present/resize，不要添加 --frame-generation，不要提交 FG tags，不要改 RR/NRD/SVGF、stable
planes、曝光、ToneMap 或 benchmark schema。

严格遵守文档中的契约修正：普通 FG on/off 属于 slDLSSGSetOptions，不要新增或滥用
slSetFeatureLoaded；普通 state 查询传 null options，只有初始化/resize 的 VRAM estimate 才传带
eRequestVRAMEstimate 的完整 options；feature-off 不得 include/reference/deploy/load DLSS-G。

按文档建议拆成 3 个 commit，不要 squash。写解释非显然生命周期/ABI/feature 隔离的注释。运行五种
feature 的短测试矩阵与 DLL 部署核对，单个进程 timeout 不超过 60 秒，不运行 600/1800 秒测试。
完成后停止，不要继续 11G-B；报告 commit、测试结果、DLL 矩阵、未验证项和 git status，等待 Codex
review。
```
