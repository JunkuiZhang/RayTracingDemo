# 阶段 11：RTXPT-style stable planes 准生产验收执行方案

## 0. 当前基线与目标

执行基线为 `ca09634`。在该提交上，三 stable plane 的 Build/Fill、NRD 三份独立 history、
RR 单次合并输入和显式嵌套介质已经能在 RTX 4060 Laptop 上真实运行，但必须显式指定
`--path-space-mode stable-planes`，默认仍为 `legacy`。

本工作包只完成以下四件事：

1. 让 stable-plane 每个重要 GPU 阶段和异常事件可测量；
2. 增加“闭合玻璃 + 内部液体”的嵌套介质夹具；
3. 建立不超过数秒/十几秒的 720p、1080p 有界验收；
4. 形成是否允许默认切换的证据。

本轮不实现 Frame Generation、ReSTIR DI/GI、新材质系统或新的降噪器，不重新设计已经通过
功能 smoke test 的 stable-plane 算法。Luna 不得在本轮自行切换默认路径；默认切换由 review
通过后的独立提交完成。

## 1. 总体原则

- 保留 `PathTrace`、`NrdPrep`、`NrdDenoise`、`NrdCompose` 和 `RrInputAdapter` 现有聚合时间，
  新增的细粒度 pass 是这些聚合区间的子项，不能改变 `GpuPass::Total` 边界；
- GPU 统计必须异步 readback，复用三帧 Frame Context 的 fence，不允许调用 `WaitForSingleObject`
  或为了读计数器让 CPU 等 GPU；
- stable-plane 统计只描述 fence 已完成的帧。reset、resize 或 generation 不匹配的数据必须丢弃，
  不能把上一代计数记到当前代；
- `legacy`、SVGF、feature-off 的新增 pass 和统计必须为 `null`/inactive，而不是伪造 `0 ms`；
- 画质指标先记录、再由 review 固定阈值。Luna 不得根据一次运行发明“看起来能过”的阈值；
- 不运行 600/1800 秒长测。自动 runner 的单进程 timeout 上限 60 秒，单 benchmark 为 1–3 秒。

## 2. WP-A：GPU pass 可观测性

### 2.1 新增 pass

在 `src/renderer/d3d12/profiler.rs` 增加以下稳定顺序的 pass：

```text
StablePlaneBuild
StablePlaneFill0
StablePlaneFill1
StablePlaneFill2
NrdStablePrep0
NrdStablePrep1
NrdStablePrep2
NrdStableDenoise0
NrdStableDenoise1
NrdStableDenoise2
NrdStableCompose0
NrdStableCompose1
NrdStableCompose2
RrStableMerge
```

为了避免今后每加一个 pass 都同步维护十几个命名字段，允许在这个独立提交内把
`GpuTimingSample` 的内部存储改成 `[f64; PASS_COUNT]`，`value(pass)` 直接按 enum 下标读取。
外部的 `GpuTimingReport::pass(GpuPass)`、滚动窗口、benchmark histogram 和 JSON 结构保持兼容。

`PASS_COUNT`、query heap、readback buffer、每帧 timestamp 数量、active mask、PIX label 和测试
必须一起更新。新增 PIX label 使用明确的 Stage 11 名称，不能把三个 plane 都标为同一个名字。

### 2.2 记录边界

在 `src/renderer/d3d12.rs` 中按以下契约放置 timestamp：

- `StablePlaneBuild`：只包住 `stage11_stable_plane_build.hlsl` 的 bind、arguments 和 dispatch；
- `StablePlaneFillN`：分别包住 plane N 的 DXR fill dispatch。运行顺序仍可为 2 → 1 → 0，
  pass 名必须表达实际 plane index，不能按循环次数命名；
- `NrdStablePrepN`：只包住对应 plane 的 prep dispatch；
- `NrdStableDenoiseN`：只包住对应 plane 的一次 NRD backend dispatch；
- `NrdStableComposeN`：只包住对应 plane 的 compose dispatch；
- `RrStableMerge`：只包住 stable RR input shader；外层 `RrInputAdapter` 继续作为兼容聚合项；
- legacy NRD 保持现有聚合 pass，不错误激活 `NrdStable*`；
- stable RR 必须启用 `RrPrimaryVisibility`/`RrBoundaryResolve`。它们是与 path-space producer 解耦的
  输出空间边界契约：仅稳定分类出的轮廓和虚拟表面，内部像素直通。原先要求 inactive 的迁移期
  约束已被 2026-09-10 的 Preset F 人工边界复验取代。

D3D12 timestamp 可以逻辑嵌套，因此 `PathTrace` 包含 Build/Fill、NRD 聚合项包含其三个子项是
允许的。禁止把父项改成子项相加；父项必须继续用自己的首尾 timestamp，保留 barrier 与 SDK
调度开销。

### 2.3 JSON 契约

`benchmark_json_line` 的 `passes` 对象新增：

```text
stable_plane_build
stable_plane_fill_0 / _1 / _2
nrd_stable_prep_0 / _1 / _2
nrd_stable_denoise_0 / _1 / _2
nrd_stable_compose_0 / _1 / _2
rr_stable_merge
```

active pass 输出原有 `{p50_ms,p95_ms,valid_samples}`，inactive 输出 `null`。测试至少覆盖：

- stable NRD：Build、Fill0..2、NrdStable* 激活，RR child inactive；
- stable RR：Build、Fill0..2、RrStableMerge 激活，NrdStable* 与旧 boundary pass inactive；
- legacy/SVGF：全部新增 child inactive；
- 每个 child 的 p95 不大于对应父项 p95 不是严格数学保证，不能写成单元测试；只检查时间有限、
  非负和 sample 数一致。

### 2.4 建议提交

```text
refactor(profiler): expose stable-plane child timings
```

该提交只改 profiler、记录点和 JSON/测试，不加入 GPU counter 或场景。

## 3. WP-B：stable-plane GPU 诊断计数器

### 3.1 固定 ABI

在 Rust 与 `stage11_path_space.hlsli` 中定义相同顺序的 12 个 `u32`：

| 下标 | 名称 | 语义 |
|---:|---|---|
| 0 | `pixels_traced` | 本帧参与 Build 的有效内部像素数 |
| 1 | `active_plane_slots` | 所有像素最终 planeCount 的总和 |
| 2..5 | `plane_count_0..3` | 分别拥有 0/1/2/3 个有效平面的像素数 |
| 6 | `plane_overflow_pixels` | 已有 3 个平面但仍有有效候选分支的像素数 |
| 7 | `branch_queue_overflow_events` | 因 6 项队列容量不足而拒绝的有效分支数 |
| 8 | `interior_overflow_events` | 两槽介质列表已满导致的 Enter 失败数 |
| 9 | `false_intersection_rejections` | priority 规则跳过的交点数 |
| 10 | `total_internal_reflection_events` | 发生 TIR 的介质界面数 |
| 11 | `invalid_medium_exit_events` | Exit 找不到对应介质的次数 |

低于吞吐量阈值而主动裁掉的分支不是 queue overflow。`plane_overflow_pixels` 每像素最多增加一次，
不能把剩余队列长度当成像素数。

### 3.2 GPU 写入方式

在 stable-plane generation 中增加一个 48 B default-heap UAV counter buffer，并为 Build table
增加 UAV descriptor。每帧 Build 前用 `ClearUnorderedAccessViewUint` 清零，然后加 UAV barrier。

不要让所有 1080p 像素直接争用同一组全局 atomic。Build shader 使用 8×8 线程组归约：

1. `groupshared uint groupCounters[12]`；
2. 组内前 12 个 lane 清零并执行 group sync；
3. 有效像素只对 groupshared counter 做 `InterlockedAdd`；
4. 再次 group sync 后由前 12 个 lane 把非零值原子加到全局 buffer。

引入 group sync 后，越界线程不能提前 `return`；必须先计算 `inBounds`，越界线程跳过路径工作但
仍参与两次同步，否则非 8 倍数分辨率会死锁。这一点必须有源码契约测试或至少 shader 编译测试。

### 3.3 异步 readback

- default counter buffer 在 Build 完成后转为 `COPY_SOURCE`，复制到与 Frame Context 对应的
  readback offset，再转回 UAV；
- readback buffer 可以是 renderer-owned 的 `12 * 4 * FRAME_COUNT`，不能每帧创建；
- Frame Context 记录 counter 对应的 `generation_id`、内部 extent、是否 stable 和 fence value；
- 只在该 frame fence 已完成时 map 对应切片；不得主动等待；
- generation/extent 不匹配、reset warm-up 或 legacy 帧直接丢弃；
- resize 时 readback buffer 本身可复用，但累计器必须 reset。

增加 `PathSpaceStatsAccumulator`，只累计 benchmark 正式测量区间内、与 GPU timing 同样有效的
completed frame。所有求和在 `u64` 中进行，最终派生：

```text
completed_frames
pixels_traced
active_planes_mean = active_plane_slots / pixels_traced
plane_count_histogram = [count0,count1,count2,count3]
plane_overflow_pixels
branch_queue_overflow_events
interior_overflow_events
false_intersection_rejections
total_internal_reflection_events
invalid_medium_exit_events
```

legacy 模式的 `path_space.stats` 为 `null`。stable capture JSON 使用最近一个同 generation 的已完成
帧，benchmark JSON 使用正式区间累计值，并带 `completed_frames`。不得把尚未完成的当前帧计数
复制到 CPU struct 假装实时数据。

### 3.4 测试

- Rust/HLSL 的 counter 数量、stride 和下标完全一致；
- accumulator 的零帧返回 `null`，除零不会产生 NaN/Infinity；
- 两帧不同 extent/generation 时拒绝错误一帧；
- active mean、histogram、overflow 累加用构造数据验证；
- capture/benchmark JSON 单行且可解析；
- 320×180 smoke 中 `pixels_traced == 57_600`；DLSS Quality 的内部尺寸按 JSON 计算，不能拿输出
  像素数作比较。

### 3.5 建议提交

```text
feat(path-space): add asynchronous stable-plane diagnostics
```

## 4. WP-C：嵌套玻璃/液体夹具

### 4.1 场景入口

新增 `--scene cornell|nested-dielectric`，默认 `cornell`。`--scene` 与 `--model` 互斥；
`--cpu-reference` 也必须继续拒绝实时场景参数。解析、help、README 和单元测试同步更新。

不要把夹具伪装成 glTF：当前 glTF loader 尚未承载 `nested_priority` 和 absorption 的项目扩展，
用 glTF 会默默退回材质默认值。夹具应由 `src/scene/cornell.rs` 的共享 box helper 程序化生成，
并通过 `SceneAsset::nested_dielectric_fixture()` 暴露。

### 4.2 几何和材质

以现有 Cornell 为背景，保留左侧金属箱，把右侧玻璃对象替换为：

- 外层闭合玻璃容器：IOR `1.50`、`nested_priority=1`、轻微非零吸收；
- 内层闭合液体体积：IOR `1.333`、`nested_priority=2`、可辨识但不过饱和的蓝绿色吸收；
- 两个 mesh 都必须是六面闭合体，外法线/三角 winding 一致，不能使用 double-sided 掩盖错误；
- 内层体积完全位于外层边界内，边界不得共面，最小间隔写成命名常量并在 CPU 测试中验证；
- 容器底部沿用“略微嵌入地板”的既有接触策略，不能制造浮空白边或与地板共面；
- stable surface ID 必须唯一、确定，静态帧不能变化。

程序化场景测试至少验证：材质 priority/IOR/absorption、闭合体每个对象恰有 6 个面、法线有限且
单位化、边界间距为正、scene validation 通过。TIR 由 GPU counter 或专门的 Snell 数学单元测试
证明；若固定相机没有触发 TIR，不得谎报夹具已覆盖，可增加一个明确的斜面测试对象或保留为
单元测试证据。

### 4.3 异常门槛

Cornell 与 nested fixture 均要求：

- `interior_overflow_events == 0`；
- `invalid_medium_exit_events == 0`；
- `plane_overflow_pixels` 和 `branch_queue_overflow_events` 必须报告，但首轮不凭空规定为零；
- nested fixture 应出现非零 `false_intersection_rejections` 或由路径/几何说明为何不出现；
- `active_planes_mean` 必须有限并位于 `[0,3]`。

### 4.4 建议提交

```text
feat(scene): add nested dielectric acceptance fixture
```

## 5. WP-D：有界验收 runner

### 5.1 新脚本

新增 `scripts/stage11_stable_planes_acceptance.ps1`，不要把这轮逻辑继续塞进已经很大的
`stage11_rr_acceptance.ps1`。参数建议：

```powershell
-Suite Smoke|Quality|Lifecycle|Gate
-NrdExe <path>
-RrExe <path>
-OutputRoot output/stage11-stable-planes
-TimeoutSeconds 60
-SelfTest
```

脚本遵循现有 runner 的 Git/environment/raw JSON 证据格式；默认输出到带 UTC timestamp 或 commit
短哈希的新目录，不能删除旧证据，也不能覆盖已有 PNG。

### 5.2 Suite 定义

`Smoke`：

- 320×180、8 SPP，stable NRD 和 stable RR 各一次；
- 验证进程退出码、PNG/JSON、consumer、内部尺寸、counter schema 和新增 pass active/null；
- 单次 timeout 不超过 30 秒。

`Quality`：

- 1280×720；NRD Native 与 RR Quality；
- stable 分别 capture 64、127、128 SPP，legacy 至少 capture 127、128 SPP；
- 127/128 用于相邻时域稳定差分，64/128 用于观察收敛，不把两者混成同一指标；
- 同样矩阵至少对 Cornell 执行，nested fixture 对 stable NRD/RR 执行 128 SPP；
- 生成全图 `image_diff` 和下列归一化 ROI 对应的像素 ROI：灯边缘、左右墙/后墙接缝、左镜面与
  地面交界、右侧玻璃顶部、玻璃内部、玻璃/地面接触区；
- ROI 坐标集中放在脚本顶部的命名表中，由输出尺寸换算，不能散落 magic numbers。

`Lifecycle`：

- 真实窗口每项最多观察 15 秒；
- 静止收敛、移动后停止、resize、最小化/恢复和 shader hot reload；
- 自动化只能记录进程是否存活、generation/reset 与最后 JSON；主观水波纹、接缝抽动和灯边缘
  稳定必须标为 `PENDING_MANUAL`，不能由脚本伪造 PASS。

`Gate`：

- stable NRD Native 1920×1080，1 秒 benchmark；
- stable RR DLSS Quality 1920×1080 输出，1 秒 benchmark；
- 每 case 独立进程、先使用程序既有 warm-up；
- 检查 `GpuPass::Total p95 <= 16.67 ms`、GPU 名称包含 RTX 4060 Laptop、stable-plane allocation、
  local VRAM measurement 和所有硬错误计数；
- 不运行超过 3 秒的 benchmark，也不循环 30 次制造伪长测。

### 5.3 画质判定

第一轮 runner 输出 `characterization`，只硬判以下事实：

- 输出存在且尺寸一致；
- JSON/schema/pass/counter 正确；
- 无崩溃、device removed、NaN JSON 或错误 generation；
- `interior_overflow_events`、`invalid_medium_exit_events` 为零；
- 1080p Total p95 不超过 16.67 ms。

相邻帧的 ROI diff 先记录但不设魔法阈值。Codex review 第一轮 raw evidence 后，把经人工确认的
candidate 指标写入一个版本化 manifest，再由后续提交设置非回退阈值。不能用 stable 与 legacy
画面绝对差作为正确性阈值，因为新路径的玻璃能量和折射本来就可能不同。

### 5.4 SelfTest

`-SelfTest` 不启动 renderer，必须覆盖：

- 参数组合和 case 表；
- 超时/非零退出码；
- 只接受 stdout 最后一条单行 JSON；
- active/null pass 契约；
- counter 整数范围和 active mean；
- ROI 从归一化坐标换算后不越界；
- 脏 worktree、缺 exe、缺 feature 的明确结果；
- `PENDING_MANUAL` 不被汇总成 PASS。

### 5.5 建议提交

```text
test(stage11): add bounded stable-plane acceptance runner
```

## 6. WP-E：证据、review 与默认切换

Luna 在前四个提交后只更新验收记录，不得切默认。记录必须包含：

- 实际 commit、tree、dirty 状态；
- RTX 4060 Laptop/驱动/电源来源；
- 各 case 的完整命令、退出码、耗时和 raw JSON 路径；
- profiler/counter/显存表；
- 固定 ROI 表和 `PENDING_MANUAL` 清单；
- 所有失败、跳过和工具限制，不得只贴成功项。

建议提交：

```text
docs(stage11): record stable-plane production gate evidence
```

Codex review 与用户动态观察全部通过后，另开独立提交引入：

```text
--path-space-mode auto|legacy|stable-planes
```

默认 `auto` 的解析规则为：

- SVGF → `legacy`，避免无消费者的 stable-plane 诊断开销；
- NRD/RR → `stable-planes`；
- 显式 `legacy`/`stable-planes` 始终覆盖 auto；
- JSON 必须分别报告 `requested=auto` 与实际 `active`，不能把 resolved value 冒充用户请求；
- legacy 至少再保留一个阶段，不在默认切换提交中删除。

默认切换建议提交：

```text
feat(path-space): default reconstructed paths to stable planes
```

该提交不属于 Luna 本轮交付，除非用户在 review 后再次明确授权。

## 7. Luna 必须执行的测试

每个代码提交至少执行与改动相关的单元测试，最终执行：

```powershell
cargo test --all-targets
cargo test --all-targets --features nrd
cargo test --all-targets --features streamline-rr
cargo check --features nrd,streamline-rr
powershell -ExecutionPolicy Bypass -File scripts/stage11_stable_planes_acceptance.ps1 -SelfTest
```

SDK 已配置且 release exe 可用时，再运行 `Smoke`。`Quality`、`Lifecycle`、`Gate` 只在 Smoke 和
代码 review 无阻断项后运行；任何 runner case 都不得要求 600/1800 秒测试。

全仓库 `cargo fmt --check` 当前存在任务开始前的无关格式差异。Luna 只能格式化自己修改的 Rust
文件，并必须运行：

```powershell
git diff --check
git status --short
```

不得全局格式化或覆盖用户的无关改动。

## 8. Review 阻断项

以下任一项出现即退回修改：

1. CPU 为读取计数器等待 GPU，或每帧创建 readback resource；
2. 非 8 倍数尺寸因 group sync 前提前 return 而潜在死锁；
3. inactive pass 输出 `0 ms` 而不是 `null`；
4. 父 pass 由 child 相加伪造，丢失 barrier/SDK overhead；
5. resize/reset 后把旧 generation counter 记入新 generation；
6. 每像素直接对 12 个全局 counter 做原子争用；
7. nested fixture 使用 open mesh、共面边界或 double-sided 掩盖 winding 错误；
8. runner 删除/覆盖旧证据，或把人工观察自动标 PASS；
9. 根据第一次运行临时调低画质阈值；
10. 夹带默认切换、Frame Generation、ReSTIR 或删除 legacy。

## 9. 完成定义

Luna 本轮完成定义是：WP-A 至 WP-E 的证据提交完成，自动测试与 Smoke 通过，其余 case 给出真实
PASS/FAIL/PENDING_MANUAL，工作区干净。只有随后 Codex review、用户动态观察和 1080p Gate 均
通过，才允许执行独立的 `auto` 默认切换提交。
