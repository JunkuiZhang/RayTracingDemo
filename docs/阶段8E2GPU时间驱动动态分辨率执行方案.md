# 阶段 8E-2：GPU 时间驱动动态分辨率执行方案

## 1. 文档状态

- 方案日期：2026-08-09。
- 目标硬件：NVIDIA GeForce RTX 4060 Laptop GPU。
- 代码基线：`3728426`（8E-1 首轮 review 修复完成）。
- 当前状态：**代码、首轮 RTX 4060 Laptop 真机数据和 Codex review 修复已完成；F1/F2 人工确认及无 output resize 污染的 600 秒 idle-wait 复核转入 8G，8E-2 尚不标记最终验收完成。**
- 前置工作：8E-1 已具备独立 `output_extent`/`render_extent`、固定 scale、ToneMap/F1 重采样、事务性 generation 创建、按 fence 退休旧代和相关遥测。

本工作包只增加由已完成 GPU Total timestamp 驱动的动态 scale 控制器。它不改变路径追踪、随机采样、Temporal、À-Trous 或 ToneMap 算法，也不关闭 8E-1 尚未完成的人工画质与长时验收债务。

## 2. 目标与完成边界

实现完成后必须满足：

1. 默认启动仍是固定 `1.0`，行为和 8E-1 完全向后兼容；只有显式 `--dynamic-resolution` 才启用动态模式。
2. 动态控制器只消费 fence 已完成且 profiler 判定有效的 GPU `Total` 样本，不使用 CPU 帧时、FPS、`Present(1)` 阻塞时间或未完成 query。
3. 默认目标为 `14.5 ms`，使用快降慢升、滞回、连续样本门槛、冷却和 resize/reload 升档预热。
4. 动态 scale 限制在 `0.67..1.00`；固定 `--render-scale` 继续支持 8E-1 的 `0.5..1.0`。
5. scale 变化只在新帧命令列表开始记录前发生，继续复用 8E-1 的事务性创建和 fence 退休，不引入 GPU idle wait。
6. 不把旧 generation 的延迟 timestamp 错当作新 generation 的负载反馈。
7. resize、最小化/恢复和成功的 shader reload 后控制器清除 streak，并重新执行 120 个有效样本的升档预热。
8. 标题和 benchmark JSON 能说明为何切换、切换多少次、当前处于何种门槛/冷却状态。
9. RTX 4060 Laptop 的 1920×1080 Cornell 默认动态模式三次 30 秒运行均满足 GPU Total p95 `<= 16.67 ms`，且默认最低内部尺寸不小于约 1280×720。

以下内容不属于 8E-2：

- DLSS、FSR、XeSS、NRD、Ray Reconstruction、锐化或新的上采样器；
- 修改每帧随机种子、冻结噪声、扩大 Temporal 历史或调整降噪参数；
- 8F Inline Ray Query/Wavefront 实验；
- generation 资源缓存池、placed-resource 大堆分配器或跨尺寸历史重投影；
- 补做 8D 大 BLAS compact-copy；
- 宣称阶段 8 已完成。

## 3. 当前代码接口与必须先修正的接缝

### 3.1 已有可复用能力

- `src/resolution.rs` 已有 `RenderScale`、8 像素量化和 `classify_render_extent_change`。
- `Dx12Renderer::set_render_scale` 已保证先完整创建新 generation，再替换 active generation；创建失败不会破坏旧状态。
- 旧 generation 记录精确的 `last_used_fence`，完成后才回收。
- `GpuProfiler::collect` 只在 Frame Context fence 完成后读取 timestamp，并已过滤 history reset、缺失 timestamp 和非法区间。
- `GpuPass::Total` 覆盖 AS、Path Trace、Temporal、À-Trous 和 ToneMap，不包含 Present/垂直同步。

### 3.2 集成前必须解决的风险

1. `render()` 当前在收集 timestamp 前就缓存 active extent。动态决策必须发生在 collect 后、任何新帧 command-list 记录前，然后再读取本帧 extent。
2. `GpuProfiler::collect` 当前只返回 `bool`，控制器若再读取 `last_sample` 容易重复消费。改为一次性返回本次完成的 `Option<GpuTimingSample>`，或提供等价的“只交付一次”接口。
3. generation 切换后仍可能陆续回收 2–3 个旧 generation 提交的 timestamp。Frame Context 必须记录提交时的 generation id；样本可继续进入统计，但只有 id 等于当前 active generation 的样本才能进入动态控制器。
4. `RealtimeConfig` 只有固定 `render_scale`，无法表达“默认固定 1.0”和“用户显式传入固定 1.0”的区别。必须用互斥 enum 建模，而不是用两个可能矛盾的布尔/数值字段。
5. 仅使用 60 个有效样本冷却，在 120/144/165 Hz 显示器上可能超过每秒一次切换。动态控制器必须同时满足“60 个有效样本”和“距离上次真实切换至少 1 秒”。

## 4. 配置与类型设计

### 4.1 分辨率模式必须是互斥 enum

建议在 `src/resolution.rs` 定义：

```rust
pub enum ResolutionMode {
    Fixed(RenderScale),
    Dynamic(DynamicResolutionConfig),
}
```

`RealtimeConfig` 使用 `resolution_mode` 替换单独的 `render_scale`。默认值必须是：

```text
ResolutionMode::Fixed(RenderScale::NATIVE)
```

不要保留 `dynamic_resolution: bool + render_scale: RenderScale` 这种可以构造出冲突状态的组合。

### 4.2 目标时间使用确定性内部表示

CLI 接受毫秒小数，但内部建议使用整数微秒，例如 `TargetGpuTime(u32)`：

- 默认：`14_500 us`；
- 允许范围：`4_000..=50_000 us`；
- parser 必须拒绝 NaN、正负 infinity、空值、非数字和越界；
- JSON/标题再转换为 `f64 ms`；
- 不让 NaN 进入 `PartialEq`、排序、JSON 或控制器状态。

最小值 `4.0 ms` 既避免无意义的负升档阈值，也允许在 RTX 4060 Laptop 上用低目标强制验证降档路径。

### 4.3 默认控制参数

所有参数集中在 `DynamicResolutionConfig`，禁止散落在 renderer 和窗口事件循环：

| 参数 | 默认值 | 语义 |
| --- | ---: | --- |
| target | 14.5 ms | 用户目标 |
| high threshold | target + 1.0 ms | 默认 15.5 ms，严格大于才算 over-budget |
| low threshold | target - 2.0 ms | 默认 12.5 ms，严格小于才算 under-budget |
| dynamic scale | 0.67–1.00 | 只约束动态模式 |
| down streak | 8 | 连续 8 个 over-budget 有效样本 |
| up streak | 120 | 连续 120 个 under-budget 有效样本 |
| down step | 0.050 | 快速降档 |
| up step | 0.025 | 保守升档 |
| cooldown samples | 60 | 切换后至少 60 个当前 generation 有效样本 |
| cooldown time | 1 second | 与样本冷却同时满足 |
| upscale warm-up | 120 | 初始化、resize/restore/reload 后禁止提前升档 |

scale 运算后统一 clamp，并量化到千分位，避免 `0.95 - 0.05` 的二进制浮点累积导致不可预测的相等比较。最终 extent 仍由 8E-1 的 8 像素量化函数决定。

## 5. 纯 Rust 控制器

### 5.1 状态

控制器至少维护：

```text
current requested scale
over_budget_streak
under_budget_streak
cooldown_valid_samples_remaining
last_applied_switch_time
upscale_warmup_remaining
valid_samples_consumed
stale_generation_samples_ignored
downscale_count / upscale_count
at_min_count / at_max_count
last_trigger_total_ms / last_direction
```

时间输入使用调用者传入的单调 `Duration`/`Instant` 差值，使控制器单元测试不依赖 sleep 或系统时钟。

### 5.2 每个样本的固定求值顺序

只对本次 `collect` 返回、`valid == true`、Total 有限且非负、且 generation id 与 active generation 一致的样本执行以下步骤：

1. `valid_samples_consumed += 1`。
2. 递减 `upscale_warmup_remaining`；无效或旧 generation 样本不递减。
3. 若有效样本冷却未结束，递减其计数、清零两个 streak，并停止本次判断。
4. 若距离上次实际切换不足 1 秒，清零两个 streak，并停止本次判断。
5. `total_ms > high_threshold`：over streak 加一、under streak 清零；达到 8 时请求降档。
6. `total_ms < low_threshold`：under streak 加一、over streak 清零；只有升档预热为零且达到 120 时请求升档。
7. 落在闭区间 `[low_threshold, high_threshold]`：两个 streak 都清零。
8. 等于阈值不算越界；禁止在 dead band 中保留旧 streak。

不得改成“最近 8 帧中有 8 帧超标”或只看 rolling p95；本阶段使用连续样本是为了让状态机可解释、可复现。以后若要试验 EMA/percentile，必须作为独立 A/B 工作包。

### 5.3 生成实际会改变 extent 的候选值

达到 streak 门槛后：

1. 从当前 requested scale 按对应步长移动并 clamp。
2. 调用 `render_extent(output_extent, candidate)`。
3. 如果和当前 active extent 相同，继续沿同一方向累加一个步长，直到找到第一个不同 extent 或到达上下界。
4. 到达下界/上界仍无不同 extent 时，记录一次 bound hold，清零对应 streak，不创建 generation、不重置 history、不进入冷却。
5. 找到不同 extent 时返回一个 decision，但只有 `set_render_scale` 成功并确认真实切换后，才提交 controller 的 current scale、切换计数、冷却和 last-switch 时间。

这使控制器决策与 8E-1 的事务性资源创建保持一致。创建失败会让 renderer 返回现有错误并退出，不允许 controller 标题先显示一个未创建成功的 scale。

### 5.4 切换后的状态

一次真实 generation 切换成功后：

- 对应方向的切换计数加一；
- 两个 streak 清零；
- `cooldown_valid_samples_remaining = 60`；
- `last_applied_switch_time = now`；
- `upscale_warmup_remaining = 120`，避免刚降档后立刻反向升档；
- 继续由 8E-1 重置 Temporal history 和 accumulated frames；
- 后续到达的旧 generation 样本只计入 `stale_generation_samples_ignored`，不推进 cooldown/streak/warm-up。

### 5.5 discontinuity reset

提供一个纯方法，例如 `reset_after_discontinuity(current_scale)`：

- 保留当前已成功应用的 scale，不强制跳回 1.0；
- 清零 over/under streak；
- 清零样本 cooldown，但设置 120 个有效样本的 upscale warm-up；
- 清除 last trigger；
- 重新计算新 output extent 下的上下界量化结果；
- 不清除 lifetime 遥测总数。

调用时机：成功 output resize/restore、成功 shader pipeline reload。普通相机移动、F1 视图切换和单纯 history reset 不调用，否则交互期间控制器可能永远无法升档。

## 6. Profiler 与 Frame Context 契约

### 6.1 一次性交付完成样本

推荐把：

```rust
collect(...) -> Result<bool>
```

改为：

```rust
collect(...) -> Result<Option<GpuTimingSample>>
```

返回 `Some` 时，该样本仍按原行为进入 rolling window 和 benchmark accumulator；返回 `None` 时任何动态计数都不能推进。不要为了动态控制器复制第二套 query 解析逻辑。

### 6.2 generation 标记

每个 Frame Context 增加提交元数据：

```text
timing_generation_id: u64
```

提交帧并 Signal 成功后，将当前 active generation id 与 `timing_valid` 一起写入 Frame Context。下一次复用该 Context 并完成 fence 后：

- profiler 正常收集样本；
- 若 id 等于当前 active id，交给 controller；
- 若不同，只增加 stale-generation ignored 计数。

不要把旧 generation 样本直接标成 profiler invalid；它仍是 benchmark 中真实发生的 GPU 工作，只是不适合作为当前 scale 的控制反馈。

### 6.3 render loop 顺序

帧开头的顺序固定为：

```text
poll successful shader reload/reset controller if needed
return early if minimized
wait current Frame Context fence
collect completed timestamp
feed controller if sample generation == active generation
apply optional scale decision transactionally
reclaim retired generations
snapshot the now-active output/render extent and descriptor heap
reset allocator/list and record the new frame
execute, Present, Signal
store fence + timing_valid + timing_generation_id
```

动态切换严禁发生在 command list 已开始记录后，也不能在缓存了 generation descriptor handle 后替换 active generation。

## 7. CLI、F2 与窗口标题

### 7.1 CLI

新增：

```text
--dynamic-resolution         启用 GPU 时间驱动的动态内部分辨率
--target-gpu-ms <毫秒>       动态模式目标，有限数值 4.0..50.0，默认 14.5
```

解析规则：

- 未传新参数：固定 1.0。
- `--render-scale X`：固定 X。
- `--dynamic-resolution`：动态模式，从 1.0 启动。
- `--dynamic-resolution --target-gpu-ms X`：动态模式，自定义目标。
- `--render-scale` 与 `--dynamic-resolution` 同时出现：明确报互斥错误，即使固定值恰好为 1.0。
- 单独传 `--target-gpu-ms`：明确报“仅能与 --dynamic-resolution 一起使用”。
- 两个新选项都属于 realtime option，必须与 `--cpu-reference` 互斥。
- help、README 和 parser tests 同步更新。

不要增加一个可以绕过互斥校验的第二解析路径。

### 7.2 F2

- 固定模式：继续按 `1.00 -> 0.83 -> 0.75 -> 0.67 -> 1.00` 循环。
- 动态模式：F2 不改变 scale、不关闭应用；按键时输出一条明确提示“动态分辨率模式由 GPU 控制，F2 固定档位不可用”。
- 不允许 F2 修改 controller 的 current scale 而不更新其 streak/cooldown 状态。

### 7.3 标题

标题保留 output/render extent 和 generation，并增加紧凑模式状态，例如：

```text
固定 0.67
动态 0.85 / 目标 14.5 ms / C37 U82
```

`C` 表示有效样本 cooldown 剩余，`U` 表示升档预热剩余。标题每 500 ms 更新即可，不逐帧格式化额外日志。

每次真实动态切换只写一条 stderr：触发方向、Total、阈值、连续样本数、旧/新 requested scale、旧/新 extent、新 generation id、旧代 retire fence。bound hold 和普通样本不得刷日志。

## 8. Benchmark JSON 与遥测

保留现有顶层 extent/generation/memory/pass 字段。`resolution_mode` 在动态模式写 `dynamic`，固定模式继续写 `fixed`。增加稳定的 `dynamic_resolution` 字段：固定模式为 `null`，动态模式为对象。

建议结构：

```json
{
  "resolution_mode": "dynamic",
  "dynamic_resolution": {
    "target_gpu_ms": 14.5,
    "high_threshold_ms": 15.5,
    "low_threshold_ms": 12.5,
    "min_scale": 0.67,
    "max_scale": 1.0,
    "current_requested_scale": 0.85,
    "cooldown_valid_samples_remaining": 37,
    "upscale_warmup_remaining": 82,
    "over_budget_streak": 0,
    "under_budget_streak": 12,
    "last_direction": "down",
    "last_trigger_total_ms": 16.2,
    "measurement": {
      "valid_samples_consumed": 1800,
      "stale_generation_samples_ignored": 6,
      "downscale_count": 2,
      "upscale_count": 1,
      "at_min_count": 0,
      "at_max_count": 0,
      "min_requested_scale": 0.85,
      "max_requested_scale": 0.90
    },
    "lifetime": {
      "valid_samples_consumed": 1920,
      "stale_generation_samples_ignored": 8,
      "downscale_count": 4,
      "upscale_count": 1
    }
  }
}
```

具体字段名可以因 Rust 结构微调，但必须同时保留：配置、最终状态、本次 benchmark delta 和 lifetime totals。原因是 120 帧 benchmark warm-up 期间可能已经发生降档；只有 measurement delta 会丢失这部分控制器行为。

其他要求：

- `last_trigger_total_ms` 不存在时为 `null`，不得序列化 NaN/inf。
- `render_min/max` 继续代表 benchmark 测量区间真实 extent。
- dynamic 的真实切换数必须能和 generation create/switch/history-reset delta 对账；warm-up 变化用 lifetime 字段解释。
- `gpu_idle_wait_count` 在动态 scale 切换中必须保持 0。
- JSON 保持单行并扩展 serializer 单元测试，分别覆盖 fixed 和 dynamic。
- 只增加向后兼容字段时保留 `schema_version: 1`；若删除/改名现有字段才升级 schema，本包禁止无理由破坏现有消费者。

## 9. 自动测试

### 9.1 控制器单元测试

至少覆盖：

1. 默认参数和阈值严格等于 14.5/15.5/12.5。
2. 7 个连续高样本不降，第 8 个降；dead band 样本会打断 streak。
3. 119 个连续低样本不升，第 120 个且 warm-up 已结束才升。
4. 等于上下阈值不触发。
5. invalid/None/NaN/inf/负值不推进 streak、warm-up 或 cooldown。
6. 降档步长 0.05、升档步长 0.025，scale 确定性舍入且不越过 0.67/1.0。
7. 只有 60 个样本但不足 1 秒不能切；超过 1 秒但不足 60 个样本也不能切；两者都满足才恢复判断。
8. 实际切换后 streak 清零、cooldown=60、upscale warm-up=120。
9. 到达 min/max 只记录 bound hold，不返回资源切换。
10. 小/奇数 output extent 下会跳过 quantized no-op，找到第一个真实不同 extent。
11. resize reset 保留当前 scale、清除 streak，并重新设置升档预热。
12. 旧 generation sample 只增加 ignored count，不推进任何控制状态。
13. 资源创建 decision 未 commit 时，不提前改变 current scale 或切换计数。

### 9.2 CLI 和序列化测试

覆盖默认、dynamic 默认目标、自定义目标、缺值、非数、NaN/inf、3.99、50.01、与 fixed 冲突、target 单独使用、与 CPU reference 冲突。检查 help 中存在两个新选项。

JSON 测试必须断言：

- fixed 的 `dynamic_resolution == null`；
- dynamic 配置、状态、measurement/lifetime 字段完整；
- 单行且所有数值可由 `serde_json` 正常解析；
- 原有 pass、memory、AS、generation 字段没有消失。

### 9.3 通用检查

```powershell
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build
cargo build --release
git diff --check
```

每个代码 commit 都必须通过 `cargo test` 和对应平台 build；禁止提交一个必须靠后续 commit 才恢复编译的中间状态。

## 10. RTX 4060 Laptop 真机验证

### 10.1 固定模式回归

先确认 8E-1 未回退：

```powershell
target\release\ray_tracing_demo.exe --benchmark-seconds 5 --output-size 1920x1080 --render-scale 1.0 --atrous-mode baseline --command-recording-mode optimized --acceleration-structure-mode baseline
target\release\ray_tracing_demo.exe --benchmark-seconds 5 --output-size 1920x1080 --render-scale 0.67 --atrous-mode baseline --command-recording-mode optimized --acceleration-structure-mode baseline
```

两条都应报告 `resolution_mode=fixed`、dynamic object 为 null、render extent 正确、history/idle/generation 指标合理。

### 10.2 默认动态正式矩阵

在插电、NVIDIA 高性能 GPU、无重后台负载条件下运行 3 次：

```powershell
target\release\ray_tracing_demo.exe --benchmark-seconds 30 --output-size 1920x1080 --dynamic-resolution --atrous-mode baseline --command-recording-mode optimized --acceleration-structure-mode baseline
```

逐次记录：

- Total/Path Trace/Temporal/À-Trous/ToneMap p50/p95 和有效样本数；
- measurement 与 lifetime 的升/降档次数；
- render min/max、最终 scale、history reset；
- generation create/switch/retired/high-watermark；
- stale generation samples；
- GPU idle wait；
- local VRAM usage/budget；
- 实际 GPU 名称和 PIX 状态。

不要只挑最好的一次；报告三次原始数据并用中位结果总结。

### 10.3 强制降档路径

当前 RTX 4060 Laptop 的 Cornell 可能在默认目标下始终保持 1.0，因此另跑低目标验证真实切换：

```powershell
target\release\ray_tracing_demo.exe --benchmark-seconds 20 --output-size 1920x1080 --dynamic-resolution --target-gpu-ms 4.0 --atrous-mode baseline --command-recording-mode optimized --acceleration-structure-mode baseline
```

要求至少发生一次 lifetime downscale，最终不低于动态下界；实际 generation 切换、history reset、retired 和日志能对账。若 1920×1080 在 4.0 ms 目标下仍无法触发，记录该结果后改用仓库可合法加载的更重场景或更高 output size 完成压力路径；不得把目标偷偷降到 parser 下界以下。

升档路径由纯状态机测试硬性覆盖。除非已有合法的高负载场景可以在运行中自然变轻，否则不为真机升档测试增加隐藏 CLI 或测试后门。

### 10.4 Debug + GPU-Based Validation

至少完成：

1. 默认 dynamic，1920×1080，运行至 120 个有效样本后退出。
2. 低目标 dynamic，确认至少一次 generation 切换。
3. 运行中 resize：1280×720 -> 1600×900 -> 1920×1080 -> 恢复。
4. 最小化，等待数秒，恢复；最小化期间不得创建 generation 或推进 controller 样本。
5. 成功 shader hot reload；controller 重新预热，InfoQueue 无状态/lifetime/root argument 错误。
6. F1 循环 11 个视图；F2 在 dynamic 模式只提示，不切换也不退出。

记录完整的 Debug InfoQueue/DRED 结果，不得只写“程序没崩”。

### 10.5 画质与稳定性人工检查

固定相机观察至少 30 秒，重点看灯四周、右侧箱子顶部、遮挡边缘和阴影：

- 动态模式在 scale 稳定后不应比相同固定 scale 增加持续噪声跳动；
- 尺寸切换允许因 history reset 出现短暂重新收敛，但不得留下持续残影、错位、ID 混色或上一尺寸边框；
- 不得通过固定随机种子掩盖闪烁；
- 视图 7/8 的 rejection/history length 在切换首帧按 reset 重新开始，随后正常增长；
- 视图 9 的物体/材质 ID 仍为 point sample，无插值色块。

如果默认 dynamic 在当前 Cornell 始终为 1.0，画质结论只能证明“不启用不退化”；必须结合低目标切换运行检查切换瞬间。

### 10.6 生命周期压力

用可触发降档的配置运行至少 10 分钟；若自然切换次数太少，可以在合法公开场景和目标范围内制造负载变化，但不能用 F2 伪装 dynamic 决策。验收：

- 非 Present `gpu_idle_wait_count == 0`；
- completed fence 前不释放旧代；
- retired queue 在稳定后回落，high-watermark 有界；
- local VRAM usage 不持续阶梯式增长且低于 budget 70%；
- 无 Device Removed、DRED、descriptor 或资源状态错误；
- 真实切换频率不高于每秒一次，不在相邻两档来回振荡。

## 11. 硬验收门槛

以下全部满足才可把 8E-2 标记为“代码和真机验收完成”：

1. 自动检查全通过。
2. 默认 fixed 1.0 和显式 fixed scale 行为不退化。
3. 控制器只消费已完成、有效、属于 active generation 的 GPU Total 样本。
4. 默认 dynamic 1920×1080 三次 30 秒 Total p95 均 `<= 16.67 ms`。
5. 默认动态内部尺寸不低于约 1280×720；动态 scale 从不低于 0.67。
6. 至少一条真机路径发生真实 dynamic generation 切换。
7. 任意两次实际切换同时相隔至少 60 个当前代有效样本和 1 秒。
8. 每次真实切换恰好对应一次 generation switch 和一次必要的 history reset；quantized/bound hold 不重置。
9. 动态切换没有增加 GPU idle wait，retired generation 能按 fence 回收，显存无持续增长。
10. resize/minimize/restore/reload/F1/F2 的 Debug Validation 和人工矩阵通过。
11. benchmark JSON 能区分 fixed/dynamic，并能对账 warm-up 与 measurement 内的行为。
12. 文档记录三次正式数据、强制降档、Debug 结果和明确未完成项。

若默认目标下 Total 已远低于 14.5 ms、始终保持 scale 1.0，这是合法结果，不应为了展示功能而提高负载或降低画质；但强制降档验证仍必须通过。

## 12. 直接拒绝项

出现以下任一项，本工作包不接受：

- 使用窗口 FPS、CPU wall-frame time 或 Present 时长代替 GPU Total timestamp；
- 在 fence 完成前映射 query readback；
- 同一个完成样本被 controller 消费多次；
- generation 切换后用旧代样本继续降/升当前代；
- 每帧或每几帧创建 render generation；
- scale 切换调用 `wait_for_gpu()`、`WaitForSingleObject()` 或 `ResizeBuffers()`；
- 在 command-list 记录中途替换 descriptor heap/generation；
- quantized extent 没变化却重置 history 或计入真实切换；
- resize/reload 后直接使用旧 streak 升档；
- dynamic 模式下 F2 破坏 controller 状态或导致程序退出；
- 修改 shader/降噪/随机采样来“改善”动态分辨率观感；
- 只跑默认 1.0、从未触发真机动态切换就宣称完成；
- 更新文档时把 8E-1、8D 或整个阶段 8 的未完成债务写成已通过。

## 13. 推荐提交拆分

这不是单 commit 任务，建议按以下边界提交；每个 commit 必须独立编译和测试：

### Commit 1：纯控制器与单元测试

```text
feat(stage8-resolution): add deterministic dynamic resolution controller
```

- 新增 mode/config/target/control state 与全部纯 Rust 状态机测试；
- 先不公开 CLI，避免出现“参数可用但 renderer 尚未动态调整”的中间状态。

### Commit 2：CLI、profiler 一次性交付与 renderer 集成

```text
feat(stage8-resolution): drive render scale from completed GPU timings
```

- 增加互斥 CLI 和 help；
- profiler 返回一次性 completed sample；
- Frame Context 标记 generation id；
- 在正确帧边界应用 decision；
- resize/reload/F2 语义完整。

### Commit 3：标题、日志和 benchmark 遥测

```text
feat(stage8-resolution): report dynamic resolution decisions
```

- fixed/dynamic 标题；
- 仅真实切换日志；
- dynamic JSON 配置、状态、measurement/lifetime delta；
- serializer tests。

### Commit 4：验证与文档回填

```text
docs(stage8): record dynamic resolution validation
```

- 真实命令和全部原始 JSON 摘要；
- Debug/人工/lifecycle 结果；
- 更新 README、阶段 8 执行计划、主实施方案和本文状态；
- 未完成项必须保留，不得提前标记阶段 8 完成。

如果实现时某个提交过大，可以继续拆分，但不要 squash Codex 已有历史，也不要把 8F/8G 混进来。

## 14. Codex review 重点

Luna 完成后，Codex 至少检查：

1. CLI 是否从类型层消除了 fixed/dynamic 冲突状态。
2. threshold、严格比较、streak 清零和 warm-up/cooldown 的 off-by-one。
3. 60 样本与 1 秒双冷却是否都以“真实切换成功”为起点。
4. controller decision 与 generation transaction 失败时是否保持一致。
5. Frame Context 的 generation id 是否和对应 timestamp 槽一起受 fence 保护。
6. extent snapshot 是否发生在动态 decision 之后、command recording 之前。
7. stale generation timestamp 是否仍进入 profiler 统计但不进入 controller。
8. resize/minimize/restore/reload 的 reset 语义是否正确；相机移动是否没有误重置 controller。
9. quantized no-op/bound hold 是否不创建资源、不重置 history、不进入 cooldown。
10. benchmark measurement/lifetime 差值是否正确，是否存在类似累计 high-watermark 的口径错误。
11. 动态切换是否继续保持 GPU idle wait 为 0，retired 生命周期是否安全。
12. 默认模式、固定 scale、F1/F2 和 8E-1 JSON 字段是否回归。

## 15. 给 Luna 的提示词

```text
你在 C:\zjk\projects\RayTracingDemo 实现阶段 8E-2。先确认 HEAD 包含 Codex 的 8E-2 方案提交，然后完整阅读：

1. docs/阶段8E2GPU时间驱动动态分辨率执行方案.md（以此为最高优先级规格）
2. docs/阶段8E1输出与内部渲染尺寸解耦执行方案.md 的 generation、fence、resize、history 和禁止项
3. docs/阶段8性能优化执行计划.md 的 8E-2、总体验收和阶段 8 实测结果
4. docs/实时DXR渲染器实施方案.md 当前阶段 8 状态

严格只做 8E-2：互斥 fixed/dynamic 配置、纯 Rust GPU 时间控制器、只消费 fence 完成且属于 active generation 的有效 Total timestamp、快降慢升滞回、60 有效样本 + 1 秒双冷却、resize/reload 预热、帧边界 generation 切换、F2 语义、标题/日志和 benchmark measurement+lifetime 遥测。

不要修改任何 HLSL、随机种子、Temporal/À-Trous/ToneMap 算法；不要加入 DLSS/FSR/NRD/锐化/Inline Ray Query；不要用 CPU/FPS/Present 时间；不要在动态切换中 wait_for_gpu；不要把旧 generation timestamp 用来控制当前 generation；不要把阶段 8 或旧的人工验收债务写成已完成。

按文档第 13 节拆成多个可编译 commit，至少保持“纯控制器”“GPU 集成”“遥测”“验证文档”边界。不要 squash、rebase 或改写 Codex/Luna 已有历史。不要提交本地生成物、截图、benchmark 临时文件或第三方模型。

每个代码阶段运行 cargo fmt --all -- --check、cargo test；最终必须运行 cargo clippy --all-targets --all-features -- -D warnings、cargo build、cargo build --release、git diff --check。按第 10 节在 RTX 4060 Laptop 上完成固定回归、默认 dynamic 三次 30 秒、低目标强制降档、Debug GPU-Based Validation、resize/minimize/restore/hot-reload/F1/F2 和生命周期验证。缺少任何人工步骤时明确写“未完成”，不得猜测通过。

最终回复给我：

- commit 列表；
- 控制器状态机和 render-loop 接入点；
- CLI 冲突矩阵；
- 全部自动检查结果；
- 三次默认动态原始 JSON 摘要和中位结论；
- 强制降档、generation/history/stale sample/idle wait/VRAM 对账；
- Debug InfoQueue/DRED 与人工画质结果；
- 尚未完成项和建议 Codex review 的高风险位置。

完成后保持工作区干净，交给 Codex review，不要自行继续 8F 或 8G。
```

## 16. 官方参考

- Microsoft [`Timing (Direct3D 12 Graphics)`](https://learn.microsoft.com/en-us/windows/win32/direct3d12/timing)：timestamp 是 GPU 在前序工作完成位置采样的计数，必须结合 queue frequency 并使用浮点换算时间。
- Microsoft [`ID3D12CommandQueue::GetTimestampFrequency`](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12commandqueue-gettimestampfrequency)：GPU timestamp tick frequency 的官方接口。
- Microsoft [`DirectX-Graphics-Samples`](https://github.com/microsoft/DirectX-Graphics-Samples)：D3D12 资源、fence、profiling 和 DXR 实现参考。
