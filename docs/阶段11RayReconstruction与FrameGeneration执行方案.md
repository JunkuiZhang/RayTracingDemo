# 阶段 11：DLSS Ray Reconstruction 与 Frame Generation 执行方案

日期：2026-08-12
目标硬件：NVIDIA GeForce RTX 4060 Laptop GPU
目标平台：Windows 10/11、D3D12、MSVC x64
执行方式：Luna 分工作包实现，Codex 逐提交 review、修复和验收
锁定 SDK：仓库现有 NVIDIA Streamline v2.12.0 release

## 1. 决策结论

阶段 11 可以开始，但必须分成两个先后独立的产品增量：

1. **11A–11F：DLSS Ray Reconstruction（RR）**。把未降噪的路径追踪 HDR 与材质、
   深度、运动和镜面 hit distance 直接交给 RR，由一次 `kFeatureDLSS_RR` evaluate 同时
   完成降噪和超分；先达到可对比、可切换、可验收。
2. **11G–11H：DLSS Frame Generation（FG）**。只有 RR 和现有基础帧路径稳定、基础帧率
   与 Reflex 计数正确后才改造 swap chain/Present；FG 不得用来掩盖基础帧的噪声、拖影、
   延迟或性能问题。

第一轮只交给 Luna 实现 11A–11D 的 RR 最小真实路径，**禁止同时实现 FG**。RR 会改变
重建分支，但不需要改造 Present；FG 会接管 Present、资源有效期和帧统计。把两者混在
同一轮或同一提交里，会让画质错误、资源状态错误和交换链错误无法独立定位。

最终目标管线如下：

```text
Path Trace ─┬─→ SVGF ─┐
            ├─→ NRD ──┼─→ HDR compose ─→ Native 或 DLSS SR ─→ ToneMap ─┐
            └─→ noisy HDR + RR guides ─→ DLSS RR ────────→ ToneMap ────┤
                                                                          ├─→ UI compose ─→ Present
                                                     FG 关闭：1 个应用帧 ─┤
                                                     FG 开启：应用帧+生成帧 ┘
```

硬规则：

- `DLSS RR` 是**融合的降噪器和超分器**，不是 `NRD → RR`，也不是 `RR → DLSS SR`。
- RR 输入必须是未经过 SVGF/NRD 的线性 HDR noisy radiance；输出已经是 output extent HDR，
  之后只 ToneMap 一次。
- RR 不得再次添加 `ReconstructionPrimaryEmissive`。noisy HDR 中的 emissive 只出现一次。
- NRD、SVGF 和普通 DLSS SR 必须保留，作为对照、回退和非 RR 路径。
- FG 只生成显示帧，不运行游戏逻辑、相机、路径追踪、累积或历史更新。

## 2. 进入阶段 11 的基线与债务处理

阶段 10 的 Release 自动短矩阵已通过：RTX 4060 Laptop 上 Native、DLAA、DLSS
Quality/Balanced/Performance 和 Quality+NRD 均能真实 evaluate，GPU idle wait 为 0。近期
又修复了 DLSS HDR 重复合成以及 SVGF 稀疏历史能量偏暗；固定 128 SPP 的 SVGF/NRD/DLSS
亮度已回到可比较范围。由此阶段 11 的 SDK、身份、frame token、Reflex/PCL 和 DLSS 输入
基础可复用。

下列旧债务继续记录，但不阻塞 RR 的工程接入：

- 阶段 10 的 Debug GPU Validation、人工 DLSS 画质和外部 Reflex Verification HUD/
  FrameView 证据仍未最终签收；
- 阶段 9 的连续运动、透明路径局部高光和公开大模型仍缺最终人工/资产验证；
- 不运行 600、1800 秒或同等级长测。

进入 FG 前必须额外满足：

- RR 与至少一个非 RR 基线通过同场景静态、相机运动和动画短验收；
- 基础应用帧 Total p95 仍低于 `16.67 ms`，不能只看生成后 FPS；
- Reflex token、Simulation/RenderSubmit/Present marker 与应用帧严格对账；
- resize、最小化、恢复和关闭 RR 不产生 device removed 或残留资源。

## 3. 不可破坏的兼容性

- 默认 Cargo feature 仍为空，默认运行仍是 `SVGF + Native`。
- feature-off 构建不得读取 Streamline SDK，不得部署 NVIDIA DLL，也不得创建 DLSS/RR/FG
  资源。
- 现有 `streamline` 只表示阶段 10 的 DLSS SR/Reflex/PCL；新增功能分别使用：

  ```toml
  streamline-rr = ["streamline"]
  streamline-fg = ["streamline"]
  ```

- 必须继续覆盖 default、`nrd`、`streamline`、`streamline-rr`、
  `nrd,streamline-rr` 的构建；FG 加入后再增加 `streamline-fg` 与 all-features。
- 缺 SDK 文件、DLL、签名或硬件支持时，显式请求的模式必须报可操作错误；不得静默换成
  NRD、SVGF、DLSS SR 或 Native。
- 正常帧、F3/F4 切换不得调用全局 `wait_for_gpu`。只允许用现有 generation + fence
  延迟回收；swap chain 重建属于 FG 生命周期例外，必须有专门的有界流程。
- 新增代码要有解释“为什么”的注释，特别是信号语义、矩阵/运动方向、资源所有权、
  Streamline 生命周期和 Present 线程约束；不要写逐行复述代码的注释。

## 4. 模式选择和运行时策略

### 4.1 RR 的 CLI 映射

在 `DenoiserBackend` 增加 `DlssRayReconstruction`，稳定字符串为 `dlss-rr`。沿用现有
`--upscaler` 表达 RR 的性能档位，避免再引入一个含义重叠的质量枚举：

```text
--denoiser dlss-rr --upscaler dlss-quality
--denoiser dlss-rr --upscaler dlss-balanced
--denoiser dlss-rr --upscaler dlss-performance
```

解析规则必须集中并可单元测试：

- `--denoiser dlss-rr` 且未显式给 `--upscaler`：解析为 `dlss-quality`，日志和 JSON 同时
  记录 requested/resolved；
- RR + `native` 或 RR + `dlaa`：启动失败。锁定的 v2.12.0 RR guide 要求 HDR low
  resolution，第一版不得猜测 DLAA 行为；
- RR 需要 `streamline-rr`，普通 DLSS SR 仍只需要 `streamline`；
- RR active 时 F4 只循环 Quality → Balanced → Performance；F3 离开 RR 后保留最近质量
  档，形成 `RR ↔ SVGF/NRD + DLSS SR` 的同分辨率 A/B；
- 每次重建后端、质量档、render/output extent 或相机不连续变化，都把当前帧的 RR
  `reset=true` 并创建/切换 generation；不污染其它后端历史。

### 4.2 RR 的分支职责

现有 HLSL 的 `NrdEnabled` 不能重命名成泛化的“重建开启”后直接用于 RR。它还控制
NRD 专属的 Primary Surface Replacement、Bayer probabilistic lobe、deterministic glass
与第二 transmission layer。RR 需要相机真正可见的 primary surface guides 和自己的
reflection reconstruction，不能接收 NRD 的虚拟镜面 primary surface。

因此第一版 RR 的 Path Trace 保持 `NrdEnabled=0`，复用普通低方差 path estimator 产生的
完整 noisy radiance，并复用实际 primary surface 的 reconstruction guides。NRD 专属
transmission layer 不送入 RR。若玻璃在 RR 下出现已确认的透明层错误，再在 11E 依据官方
`ColorBeforeTransparency`/transparency tags 单独设计；禁止先传全零或伪造 guide。

### 4.3 动态分辨率

第一轮 11A–11D 只支持 RR 的固定 optimal input extent。RR + `--dynamic-resolution` 必须
明确报“阶段 11E 尚未开放”，不能静默冻结 scale。11E 再读取
`DLSSDOptimalSettings::{renderWidthMin/Max,renderHeightMin/Max}`：仅当 SDK 报告有效范围时，
才允许现有控制器在范围内改变 extent，并为每个 tag 提供当帧真实 extent；越界变化要
走 generation 切换和 RR reset。

## 5. RR 输入契约

以仓库锁定的 `ProgrammingGuideDLSS_RR.md` 和 `sl_dlss_d.h` 为唯一接口依据。所有 RR
输入都是 input/render extent，输出是 output extent；格式、空间、单位与来源如下：

| Streamline tag | 资源与格式 | 必须满足的语义 |
|---|---|---|
| `kBufferTypeScalingInputColor` | `reconstruction_noisy_hdr`, `RGBA16_FLOAT` | 线性、pre-exposed HDR；完整 noisy radiance，diffuse/specular/emissive 各一次；未经过任何时空滤波或 ToneMap |
| `kBufferTypeScalingOutputColor` | 新的 `rr_output_hdr`, `RGBA16_FLOAT` | output extent；只供 ToneMap/后续显示使用，不再交给 DLSS SR |
| `kBufferTypeAlbedo` | `reconstruction_diffuse_albedo`, `RGBA16_FLOAT` | RGB 为线性 diffuse reflectance，不含光照；不得 sRGB；alpha 不参与 tag 语义 |
| `kBufferTypeSpecularAlbedo` | `reconstruction_specular_albedo`, `RGBA16_FLOAT` | RGB 使用锁定 guide 的 EnvBRDF 近似；线性、有限、非负；不得把 F0 或已乘光照的 specular radiance 冒充 specular albedo |
| `kBufferTypeNormalRoughness` | 新的 `rr_normal_roughness`, `RGBA16_FLOAT` | RGB 为**有符号、归一化的世界空间 shading normal**，A 为 linear roughness；`normalRoughnessMode=ePacked` |
| `kBufferTypeMotionVectors` | 现有 DLSS `motion`, `RG16_FLOAT` | dense camera+dynamic-object motion；`previousPixel-currentPixel`；与 common constants 的 `mvecScale` 完全一致；不是 NRD 2.5D motion |
| `kBufferTypeDepth` | 现有 DLSS `depth`, `R32_FLOAT` | 与 motion 同一实际 primary surface 的 inverted HW depth；`depthInverted=true` |
| `kBufferTypeSpecularHitDistance` | `reconstruction_specular_hit_distance`, `R32_FLOAT` | 从 primary specular ray origin 到 hit point 的世界空间距离；无有效镜面 hit 时为 0；不得使用 viewZ、NRD normalized hitT 或二级 transmission hitT |
| `kBufferTypeExposure`（若当前 SR 路径需要） | 现有 exposure | 固定 preExposure=1 的同一曝光契约；RR 忽略 auto exposure/sharpness，不得借 exposure 再提亮 |

`reconstruction_normal_roughness` 当前把 normal 编码为 `normal * 0.5 + 0.5`，不能直接 tag
给 RR。为了不破坏 NRD prep，新增一个轻量 input-adapter compute/graphics pass，仅解码并
归一化 normal、复制 roughness；同时对 NaN/Inf 和零长度 normal 采用可诊断的稳定回退。

RR options 使用：

- `colorBuffersHDR=true`、`preExposure=1`、`exposureScale=1`；
- `alphaUpscalingEnabled=false`、`normalRoughnessMode=ePacked`；
- `worldToCameraView` 为当帧非抖动 world→view，`cameraViewToWorld` 为其逆矩阵；两者的
  row-major 转换要有数值单元测试；
- 质量 preset 先使用 v2.12.0 的 `eDefault`，不要未经图像 A/B 擅自固定 D/E；
- 同一帧的 frame token 和 viewport 必须贯穿 common constants、DLSS SR compatible
  options、RR options、tags 和 evaluate。

必须增加 debug/capture contract 测试，至少能验证：normal 解码后的长度、roughness
范围、albedo 非负、motion 静态相机近零、depth 有限、spec hit distance 单位，以及 RR
input/output extent。不能只用 `shader.contains()` 证明像素语义正确。

## 6. 工作包 11A：SDK 锁、feature 和能力探测

### 实现

1. 在 `third_party/streamline/version.lock.json` 增加本机实际文件的 SHA-256，不得编造：
   - `include/sl_dlss_d.h`；
   - production/development `sl.dlss_d.dll`；
   - production/development `nvngx_dlssd.dll`；
   - 适用的 LICENSE/NOTICE（若已有锁项则复用，不重复部署）。
2. `scripts/fetch_streamline.ps1` 与 `build.rs` 仅在 `streamline-rr` 开启时要求、校验和部署
   RR 文件；普通 `streamline` 构建不得被 RR DLL 缺失阻塞。
3. Release 只部署 production DLL，Debug 只部署 development DLL；所有可部署 DLL 继续
   验证 NVIDIA Authenticode signer。
4. bridge 在条件编译下把 `sl::kFeatureDLSS_RR` 加入 `featuresToLoad`，support query 返回
   `rr_supported` 与原始 `rr_result`；不根据 RTX 型号硬编码支持。
5. ABI 从 v3 升级为 v4，Rust/C++ 的 size、align、offset 和 version 测试同步更新。老结构
   不得靠未初始化尾部字段“兼容”。

### 验收

- `cargo test --locked`、`cargo test --features streamline --locked` 不需要 RR DLL 即通过；
- `cargo test --features streamline-rr --locked` 能构建 bridge；
- 篡改任一 RR DLL/hash 时构建快速失败并指出准确路径；
- RTX 4060 Laptop 的启动 JSON 同时给出 DLSS SR、RR、Reflex、PCL support/result；
- 显式请求 RR 但 support 非 0 时，在创建大纹理前失败。

### 建议提交

`build(stage11): pin optional DLSS RR runtime`

## 7. 工作包 11B：扩展 Streamline C ABI

### 实现

在 `native/streamline_bridge` 和 `src/streamline.rs` 增加专用、强类型的 RR API：

- `rr_get_optimal_settings`，返回 optimal/min/max extent；
- `rr_set_options`，包含 mode、output extent、HDR/exposure、packed normal-roughness、
  world→view 与 view→world；
- `rr_get_state`，至少返回 `estimatedVRAMUsageInBytes`；
- `evaluate_rr`，固定 evaluate `sl::kFeatureDLSS_RR`；
- `free_rr_resources`，固定 free RR feature。

不要把 `uint32_t feature_id` 暴露给 Rust 任意传入。SR/RR 各用专用入口或 bridge 内部强类型
枚举，避免把 RR viewport 误 free 为 DLSS SR。阶段 10 已证明 v2.12.0 的提前
`slAllocateResources` 会和 frame-based tags/lazy allocation 错序；RR 继续由第一次
`slEvaluateFeature` lazy allocate，**不得重新引入每帧或 frame 0 allocation**。

所有导出仍为 `extern "C"`、noexcept 边界、固定宽度字段、显式 struct header、失败清理
对称。新增矩阵拷贝要注释 row-major 原因，并用非对称矩阵测试防止转置两次后“看似通过”。

### 验收

- bridge invalid/null/unsupported/repeated shutdown 测试不崩溃；
- C++ `static_assert` 与 Rust layout tests 覆盖全部新增结构；
- support → optimal → options → state 的错误路径保留 SDK result 和 bridge message；
- 不改变普通 DLSS SR、Reflex/PCL 的调用计数和现有短测结果。

### 建议提交

`feat(stage11): expose DLSS RR through Streamline bridge`

## 8. 工作包 11C：融合模式策略和资源 generation

### 实现

1. 加入 `DenoiserBackend::DlssRayReconstruction`、CLI 规则、启动错误、Display/JSON 和测试。
2. 把渲染分支表达为明确的 `ReconstructionPath::{Svgf,Nrd,DlssRr}` 或等价内部策略，禁止
   到处散落 `denoiser == ...` 与 `upscaler != native` 的组合判断。
3. `RenderGenerationDesc` 分开表达 `with_dlss_sr` 与 `with_dlss_rr`；RR 复用 DLSS depth/
   motion guides，但只创建一个 RR output，不创建/执行 SR compose input。
4. generation key 至少包含 output extent、render extent、重建路径、RR quality mode；旧
   generation 只在 fence 完成后回收，回收时 free 对应 RR viewport resources。
5. RR active 时禁用 SVGF Temporal/À-Trous、NRD Prep/Denoise/Compose 和 DLSS SR Compose/
   Evaluate；GPU profiler 中这些 pass 必须为 null/0 samples，而 RR input prep/evaluate 有
   独立 timestamp。
6. ToneMap 输入模式新增 RR output，并确保静态函数测试覆盖 SVGf、NRD、DLSS SR 和 RR。

### 验收

- 解析、模式兼容矩阵、F3/F4 循环和 generation key 有单元测试；
- feature-off Native 资源数与阶段 10 基线不变；
- RR active 的 inactive pass 均为 null，不允许先运行 NRD 再丢弃结果；
- 模式切换不全局 GPU idle，历史 reset 只发生一次且不跨后端污染。

### 建议提交

`feat(stage11): define fused ray reconstruction mode`

## 9. 工作包 11D：RR 输入适配与最小真实 evaluate

### 实现顺序

1. 新增 RR generation resources：`rr_normal_roughness`、`rr_output_hdr`；按实际需要复用
   Stage 10 DLSS depth/motion/exposure，禁止复制语义相同的纹理。
2. 新增 `stage11_rr_input.hlsl` 或边界清晰的等价 shader，生成有符号 normalized world
   normal + packed linear roughness，并在 Debug 构建统计 invalid guide 像素。
3. 保持 `NrdEnabled=0` 生成 RR noisy/guides；不要让 NRD PSR、probabilistic lobe 或第二
   transmission layer泄漏进 RR。
4. 在 Path Trace 后调用 input adapter；提交 common constants、兼容 DLSS options、RR
   options 与全部必需 tags；使用同一个 token/viewport/command list 调用
   `slEvaluateFeature(kFeatureDLSS_RR)`。
5. evaluate 后按 manual hooking guide 重新绑定 renderer 后续需要的 root signature、heap、
   pipeline 和 viewport/scissor；不要假设 Streamline 恢复 command-list state。
6. RR output 直接进入一次 ToneMap。不得执行 HDR compose、NRD emissive compose、DLSS SR
   evaluate 或第二次 ToneMap。
7. 错误必须带 frame、viewport、mode、input/output extent、SDK result；默认终止显式请求，
   只有用户明确开启已有 fallback 开关时才允许降级，并记录 requested/active/reason。

### 最小验收

- RTX 4060 Laptop 上 `dlss-rr + quality` 真实返回成功，RR evaluate GPU samples > 0、
  output 非全黑/非 NaN、idle wait 0；
- 同一 executable 再跑 `SVGF + DLSS Quality`，证明普通 SR 未回退；
- RR 输出整体与后墙/地面 ROI luminance 相对 NRD+DLSS 基线不应无理由偏离超过 5%；超过
  时先检查重复 emissive、preExposure、输入是否已 denoise，不能靠 ToneMap gain 调齐；
- 静态 64→128 帧 RR 图像应趋于稳定，镜面/玻璃 ROI 不出现全屏盐粒、黑块或错误历史；
- 一次小输出 Debug GPU Validation 有界运行，InfoQueue ERROR/CORRUPTION 为 0；
- 所有测试单项最长 60 秒，禁止 600/1800 秒长测。

### 建议提交

`feat(stage11): execute minimal DLSS ray reconstruction`

## 10. 工作包 11E：RR 生命周期、动态分辨率与诊断

11D 通过 Codex review 后再实现：

- F3/F4 实机往返、F1 debug view、resize、最小化/恢复、shader hot reload；
- 对相机 teleport、模型动画 discontinuity、extent/mode change 发送严格的一帧 reset；
- 支持 RR reported min/max 范围内的动态分辨率，或在 SDK 不支持时保留明确启动错误；
- capture JSON 增加 RR support/result、requested/active、mode、optimal/min/max、VRAM estimate、
  input guide invalid counts 和每个 RR pass 的 p50/p95；
- 新增 RR guides debug views，至少包括 noisy HDR、diffuse/spec albedo、signed normal、roughness、
  depth、motion、spec hit distance、output HDR；
- 若真实玻璃出现透明层 ghost/disappear，再依据官方同格式要求实现
  `ColorBeforeTransparency`；没有证据则保持 deferred。

建议提交：

`fix(stage11): harden RR lifecycle and diagnostics`

## 11. 工作包 11F：RR 对照验收

新增 `scripts/stage11_rr_acceptance.ps1`，沿用阶段 10 的有界子进程、单 JSON、EXE/hash/
commit 绑定，不要复制一套弱化 runner。短矩阵固定输出 1920×1080：

| case | feature | 模式 | 自动硬门槛 |
|---|---|---|---|
| default-native | default | SVGF + Native | feature-off、无 Streamline DLL/资源、回归基线 |
| sr-quality | streamline | SVGF + DLSS Quality | DLSS samples > 0、RR null、idle wait 0 |
| nrd-quality | nrd,streamline | NRD + DLSS Quality | NRD/DLSS samples > 0、RR null |
| rr-quality | streamline-rr | RR Quality | RR samples > 0、NRD/SVGF/SR inactive、尺寸/亮度/invalid guides 合格 |
| rr-balanced | streamline-rr | RR Balanced | 同上，optimal extent 必须来自 RR query |
| rr-performance | streamline-rr | RR Performance | 同上，不能复用 Quality extent |
| rr-animated | streamline-rr | Triangle.gltf + animation | 有效动态 motion、无 crash/device removed |
| rr-debug | streamline-rr | 低分辨率 Debug Validation | InfoQueue 0、60 秒内有界退出 |

每个 Release case 先 1 秒 smoke；capture 只取必要的 64/128 SPP 节点和固定 ROI。不得用
神经网络输出做跨驱动逐像素 golden gate，但必须检查有限值、luminance、temporal MAE/RMSE
趋势、边缘 ghost ROI 和静态高频残差。最终由用户对镜面、右侧玻璃、面积灯周围和相机运动
做人工签收。

建议提交：

`test(stage11): add bounded RR acceptance matrix`

## 12. 工作包 11G：Frame Generation（RR 完成后）

### 12.1 SDK 与能力

- 开启独立 `streamline-fg` feature，只锁定和部署实际需要的 `sl_dlss_g.h`、production/
  development `sl.dlss_g.dll`、`nvngx_dlssg.dll` 及许可；继续校验 hash 和 NVIDIA 签名。
- bridge 增加 `kFeatureDLSS_G` support、`slDLSSGSetOptions/GetState`、VRAM estimate、
  `numFramesActuallyPresented` 和 feature load/unload 的强类型接口。
- RTX 4060 Laptop 属于目标设备但不能据型号假设 FG、fixed multiplier 或 Dynamic MFG
  支持；全部以 support/state 为准。第一版只做单个 generated frame（2x display），动态
  MFG 后续单独评估。

### 12.2 必须重做的 manual-hooking 边界

阶段 10 的“swapchain 创建后 upgrade，用于 `presentCommon()`”不足以证明 FG swapchain
interception 正确。按照 v2.12.0 Manual Hooking Guide，FG 实现必须：

1. 在创建 swapchain 前升级 D3D12 device、command queue 所需接口和 DXGI factory；
2. 通过升级后的 factory 创建 Streamline proxy swapchain；
3. renderer 明确区分 proxy（所有被 hook 的 Present/GetBuffer/ResizeBuffers 等调用）与
   native interface（仅非拦截调用）；
4. `presentCommon()` 每个应用 Present 恰好一次；不能既 proxy Present 又手工重复调用；
5. FG 开/关按官方建议先置 off、停止提交、等待 fence、释放 backbuffer、重建 swapchain，
   再恢复；禁止在活跃 Present 中直接 ResizeBuffers。

这是 11G review 的最高风险区，必须独立提交，不能夹带 RR 画质改动。

### 12.3 FG 输入和帧语义

- backbuffer：由 Streamline swapchain 拦截，output extent；
- depth/motion：复用 Stage 10 的同一 primary-surface DLSS guides，使用当帧实际 extent；
- HUD-less color：本项目当前没有 GPU 内 HUD，ToneMap 后、复制到 backbuffer 前的
  `display_output` 可作为 hud-less，颜色空间和后处理必须与 backbuffer一致；
- UI：没有客户端 UI 时明确 tag 全透明 UI alpha 或按官方允许方式 null，不得把窗口标题
  当 UI texture；系统硬件光标保持 OS 路径并人工检查；
- 所有 FG inputs 初版使用 `eValidUntilPresent`，资源 generation 必须活到对应 Present；
  只有实测证明被提前复用时才换更短生命周期；
- paused/minimized/loading/resize/无有效 guides 时，先关闭 FG 并把 tags 设 null。

FG options 必须在 Present 线程或由明确同步保证对目标 Present 的顺序。Reflex 是 FG 的
硬依赖，同一 application frame index 必须匹配 common constants 与 PresentStart/End。
generated frame 不获得新的 simulation/render frame token，不推进 SPP，不更新历史。

### 12.4 统计与验收

同时展示和写入 JSON：

- base/application FPS 与 Total GPU p50/p95；
- `numFramesActuallyPresented` 推导的 generated/display FPS；
- Reflex 可用性和 latency report（不可用时明确 null，不造数字）；
- FG mode、requested/generated/dropped frames、VRAM estimate；
- `Present`/`presentCommon`/Reflex marker 对账。

FrameView 的 `MsBetweenDisplayChange` 是最终 frame-pacing 外部证据；不能用普通窗口 FPS 或
`MsBetweenPresents` 宣称显示平滑度。内部 runner 只做正确性与短稳定性，外部 FrameView/
Reflex evidence 由用户运行并归档。

建议拆分提交：

1. `build(stage11): pin optional DLSS frame generation runtime`
2. `feat(stage11): create Streamline-managed FG swapchain`
3. `feat(stage11): tag FG inputs and present generated frames`
4. `feat(stage11): report base and generated frame metrics`

## 13. 工作包 11H：最终门槛

阶段 11 只有同时满足以下条件才能标记完成：

- RR 三档能真实运行并与 SVGF/NRD 对照，静止和运动中无明显错误历史；
- RR active 没有 NRD/SVGF/SR 的隐藏成本，没有重复曝光/合成/发光；
- FG 开关、resize、最小化/恢复、退出可重复，Debug InfoQueue 和 DRED 无错误；
- 基础 FPS、生成后 FPS、实际 presented multiplier 和 latency 的含义不混淆；
- Reflex marker 与应用 Present 严格对账，FG 关闭时没有代理残留开销或错误帧；
- 关闭全部 NVIDIA 可选 feature 后，默认自研路径仍能构建、运行并保持阶段 10 前的资源
  与像素契约；
- 所有自动证据有 commit、tree、EXE/hash、SDK/DLL flavor 和命令；人工画质/FrameView
  项明确标记 PASS 或 PENDING，不把未运行写成通过。

## 14. Luna 第一轮实施范围（11A–11D）

第一轮允许 4–5 个独立提交，按以下顺序完成并停下等待 Codex review：

1. RR SDK/feature/support；
2. ABI v4 与 RR bridge；
3. 融合模式/CLI/generation 策略；
4. RR input adapter 与最小真实 evaluate；
5. 仅在需要时提交短测/记录，不能把失败输出伪装为验收通过。

本轮禁止：

- Frame Generation、swapchain/Present 大改；
- RR + NRD 串联、RR 后再 DLSS SR；
- 改 ToneMap/exposure 来“调亮”；
- 让 `NrdEnabled=1` 复用 PSR/transmission 输入；
- 静默 fallback；
- 600/1800 秒长测、下载新资产；
- 重写阶段 9 的采样器、GGX、玻璃或 NRD 算法；
- 合并/改写本轮开始前的用户提交。

完成后 Luna 必须报告：每个 commit hash/意图、改动文件、实际执行命令和结果、RR support
原始 result、真实 run-id/关键 GPU samples、仍未运行的人工项目和已知风险。Codex review
将重点核对 SDK 锁、ABI layout、normal/roughness、noisy HDR/发光能量、motion/depth、
spec hit distance、inactive pass、资源状态和 lazy allocation。
