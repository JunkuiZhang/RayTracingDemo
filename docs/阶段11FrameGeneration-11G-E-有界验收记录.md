# 阶段 11：Frame Generation 11G-E 有界验收记录

日期：2026-09-12

目标硬件：NVIDIA GeForce RTX 4060 Laptop GPU

状态：**AUTOMATED PASS；人工显示链验收仍为 PENDING，阶段 11 尚未完成**

## 1. 本包结论

11G-E 的自动化部分已经在干净提交 `88ce95e204610d522af21d0951697b0f9e34c82e`
上通过：

- Release 五项矩阵全部正常退出，无 timeout、shutdown hang 或 gate failure；
- FG-off 保持 1 application frame = 1 displayed frame；
- 三个 FG-on case 都取得 SDK 确认的真实生成帧，application/display 为精确 2x；
- RR 与 FG 同时 active，动画 case 保持 TLAS update；
- token、六个 PCL marker、Reflex sleep 和 `presentCommon` 均按 application frame 对账；
- 正式 measurement interval 内 `gpu_idle_wait_count == 0`；
- Debug FG 正常退出，D3D12 Debug InfoQueue 为 0 条消息。

这些结果证明自动化实现、运行期状态和基础资源生命周期已闭环，但不能代替真实显示器上的
FrameView pacing、交互开关和人工运动画质观察。因此 summary 有意保持
`stage_complete=false`，阶段 11 目前不能标记完成，也不能仅凭内部 `display_fps` 宣称显示 pacing
通过。

## 2. Runner 契约

新增 [`scripts/stage11_fg_acceptance.ps1`](../scripts/stage11_fg_acceptance.ps1)，提供：

- `Smoke`：最小 RR+FG 短门禁；
- `Matrix`：feature-off、FG compiled-off、DLSS Quality+FG、RR Quality+FG、动画+FG；
- `DebugValidation`：低分辨率 Debug+FG 与 InfoQueue 门禁；
- `SelfTest`：完全不启动 GPU，验证 runner 会拒绝八种伪 PASS。

runner 保存命令行、stdout、stderr、退出状态、summary，以及以下 provenance：

- git HEAD/tree/dirty；
- GPU、驱动、操作系统和供电来源；
- EXE 与部署 DLL 的 SHA-256；
- Streamline plugin set 和 production/development flavor；
- case 的精确参数与 timeout。

工作区 dirty 时可以开发 smoke，但 summary 必须失败，不能形成正式 PASS。FG 窗口必须真正位于
前台；runner 使用 Win32 前台窗口句柄核验，而不是依赖存在竞态的 winit 缓存日志。只有目标窗口
已最小化时才调用 `SW_RESTORE`，避免前台激活无意改变窗口状态。Debug case 使用 640x360；它只
检查 API/InfoQueue 正确性，Release 画质与计数矩阵仍使用 1920x1080。

## 3. Runner 自测

命令：

```powershell
.\scripts\stage11_fg_acceptance.ps1 -SelfTest
```

结果：PASS，`gpu_started=false`。runner 正确拒绝：

```text
no_generated_frame
marker_counted_display_frames
invented_multiplier
compiled_off_proxy
steady_state_idle_wait
rr_inactive
animated_without_tlas_update
extra_stdout
```

这避免了用请求态、估算倍率、显示帧 marker 或额外 stdout 冒充真实验收。

## 4. Release Matrix 正式证据

run：`output/stage11-fg/20260912-224147-dfefb8aa/summary.json`

环境与 provenance：

- git HEAD：`88ce95e204610d522af21d0951697b0f9e34c82e`；
- dirty：`false`；
- GPU：RTX 4060 Laptop；
- suite：`Matrix`；
- 每个 case 正式采样 1 秒，单 case timeout 20 秒；
- `automatic_pass=true`，所有进程 `exit_code=0`，`timed_out=false`。

| case | app | display | generated | dropped | multiplier | Base/Display FPS | Total p95 | idle waits |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `feature_off` | 95 | 95 | 0 | 0 | 1.00x | 95 / 95 | 11.59 ms | 0 |
| `fg_compiled_off` | 149 | 149 | 0 | 0 | 1.00x | 149 / 149 | 7.10 ms | 0 |
| `fg_quality` | 91 | 182 | 91 | 0 | 2.00x | 91 / 182 | 8.27 ms | 0 |
| `fg_rr_quality` | 61 | 122 | 61 | 0 | 2.00x | 61 / 122 | 13.39 ms | 0 |
| `fg_animated` | 90 | 180 | 90 | 0 | 2.00x | 90 / 180 | 8.05 ms | 0 |

`fg_compiled_off` 虽启用 Streamline/Reflex 编译能力，但 `fg_loaded=0`、native swap chain 和 1x
统计保持成立。三个 FG-on case 都有 `frame_generation_confirmed`，且 token、marker、sleep、
`presentCommon` 等于各自 application frame 数，而不是 display frame 数。RR case 的
`rr_evaluate` active，动画 case 的 `tlas_update_enabled=true`。

## 5. Debug Validation 正式证据

run：`output/stage11-fg/20260912-223914-59555708/summary.json`

- git HEAD：`88ce95e204610d522af21d0951697b0f9e34c82e`；dirty：`false`；
- `automatic_pass=true`，`exit_code=0`，`timed_out=false`，foreground=true；
- application/display/generated/dropped：`114 / 228 / 114 / 0`；
- 实际倍率：`2.00x`；Base/Display FPS：`114 / 228`；
- Total p95：`4.71 ms`；正式区间 idle waits：`0`；
- Reflex token 和六个 marker 均为 `114`；
- stderr：`D3D12 Debug InfoQueue：0 条消息`。

Debug 仍使用完整的 120 个有效 GPU 样本预热，因此正式采样虽然只有 1 秒，外层 timeout 设为 60
秒。它不是性能测试；该上限只容纳未优化 Rust、GPU-based validation 和固定预热，不允许无限等待。

## 6. Debug 验收发现并修复的问题

### 6.1 Windows GUI 栈溢出

首次 Debug run `20260912-222234-996af63a` 在初始化时以 Windows
`0xC00000FD` 退出。`Dx12Renderer` 当时约 74 KiB，主要来自 `GpuProfiler` 内嵌的
33 × 240 个 `RollingStats` 样本。未优化构造路径在多个栈帧移动该对象，耗尽 GUI 线程栈。

修复：统计 ring 改为 `Vec<RollingStats>` 堆分配，并增加渲染器状态必须小于 32 KiB 的回归测试。
这不改变统计容量、窗口语义或 JSON。

### 6.2 过期的 UAV 布局闭合断言

栈问题消失后，Debug 发现 UAV 总数闭合断言仍以 `StableSpecularAlbedo(u38)` 为末项，但真实末项
已经是 `StablePlaneCounter(u39)`。修复断言并在描述符布局单元测试中锁定
`counter + 1 == DXR_UAV_REGISTER_COUNT`；实际 descriptor 创建本身没有缺项。

### 6.3 FG display output 未回迁到 UAV

GPU-based validation 随后报告 Tone Map 把处于 `COPY_SOURCE` 的 `display_output` 当 UAV 写入。
FG 的 `eValidUntilPresent` 要求该资源跨 proxy Present 保持可读；上一帧 Present 返回后，下一帧
Tone Map 前必须显式从 `COPY_SOURCE` 转回 `UNORDERED_ACCESS`。资源追踪器不能从 descriptor 使用
自动推断这一步。

修复后重新运行相同 Debug 路径，原 227 条 ERROR 降为 InfoQueue 0。回归测试也锁定回迁发生在
`GpuPass::ToneMap` 之前。

相关提交：

- `5fdd526 fix(stage11): make Debug FG validation clean`
- `88ce95e test(stage11): bound Debug FG validation`

## 7. 构建与复现

三个 Release flavor 必须放在独立 target 目录，避免不同 feature 的 DLL 集互相污染：

```powershell
$env:CARGO_TARGET_DIR = 'output/stage11-fg-build/default'
cargo build --release --locked

$env:CARGO_TARGET_DIR = 'output/stage11-fg-build/fg'
cargo build --release --features streamline-fg --locked

Remove-Item Env:CARGO_TARGET_DIR
cargo build --release --features streamline-rr,streamline-fg --locked
```

Release matrix：

```powershell
.\scripts\stage11_fg_acceptance.ps1 `
  -Suite Matrix `
  -DefaultExe .\output\stage11-fg-build\default\release\ray_tracing_demo.exe `
  -FgExe .\output\stage11-fg-build\fg\release\ray_tracing_demo.exe `
  -RrFgExe .\target\release\ray_tracing_demo.exe `
  -Seconds 1 `
  -TimeoutSeconds 20
```

Debug：

```powershell
$env:CARGO_TARGET_DIR = 'output/stage11-fg-build/fg-debug'
cargo build --features streamline-fg --locked
Remove-Item Env:CARGO_TARGET_DIR

.\scripts\stage11_fg_acceptance.ps1 `
  -Suite DebugValidation `
  -FgDebugExe .\output\stage11-fg-build\fg-debug\debug\ray_tracing_demo.exe `
  -Seconds 1 `
  -TimeoutSeconds 60
```

## 8. 仍需人工完成的门槛

以下项目保留为 `PENDING`，不能从当前自动结果推断为 PASS：

1. 前台窗口中连续执行两次 F5 on/off 往返，确认 proxy/native 生命周期和画面无异常；
2. FG on 时 resize、最小化、恢复和正常退出；
3. 静止、相机移动和 Triangle 动画下检查边缘、透明体、UI/HUD-less color 是否出现明显伪影；
4. 用 FrameView 检查 `MsBetweenDisplayChange`，确认真实显示节奏接近 2x；
5. 若可用，用外部 Reflex 工具记录端到端延迟；不可用时继续明确为 PENDING。

下一步不是继续增加 FG 功能，而是执行上述人工显示链验收并把 raw/截图/FrameView 证据追加到本
记录。全部通过后再做阶段 11 最终 review，届时才判断能否进入阶段 12。
