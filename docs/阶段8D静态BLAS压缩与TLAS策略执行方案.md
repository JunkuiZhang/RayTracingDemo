# 阶段 8D：静态 BLAS 压缩、TLAS 策略与 AS 预算执行方案

## 1. 目的、起点与交付边界

本文细化 [`阶段8性能优化执行计划.md`](阶段8性能优化执行计划.md) 的 8D 工作包，供 Luna 独立实现、测试并分批提交，之后由 Codex review。

- 实现代码基线：`748eb18 perf(stage8): adopt optimized command recording`。实际工作从包含本文的 `main` 最新 HEAD 开始，不要 checkout 回旧提交而丢失本方案。
- 目标硬件：NVIDIA GeForce RTX 4060 Laptop GPU。
- 当前默认命令记录模式：`optimized`；本工作包不得改回 baseline。
- 当前默认 À-Trous 模式：`baseline`；8B 的 shared 路径仍只是未采纳实验。
- 当前实现状态：8D 代码、小模型 A/B、Debug GPU Validation 和首轮 Codex review 修复已完成；AS 默认仍为 baseline，大 BLAS compact copy 与人工画质/生命周期矩阵仍待验收。
- 8D 目标：压缩真正能减少实际 allocation 的静态 BLAS；让 TLAS 只在确有刚体实例动画时承担 update 成本；正确释放只在初始化期需要的资源；输出可审计的 AS 内存与策略遥测。
- 8D 不等于阶段 8 完成。8A 长时矩阵、8C 画面对比、8E 动态分辨率和 8G 总体验收仍是后续工作。

Luna 只实现本文范围。允许拆成多个 commit，而且推荐按第 10 节拆分；不要把所有工作压进一个难以 review 的提交。

## 2. 当前实现审计

当前主要实现位于：

- `src/renderer/d3d12/raytracing.rs`
- `src/renderer/d3d12.rs`
- `src/realtime.rs`
- `src/main.rs`

现状如下：

1. `AccelerationStructures::build` 为每个 primitive 创建一个 committed BLAS，flags 只有 `PREFER_FAST_TRACE`。
2. BLAS、TLAS 在同一个初始化 command list 中构建，随后只等待一次初始化 fence。
3. TLAS 无论场景是否会动画，初次 build 都使用 `ALLOW_UPDATE | PREFER_FAST_TRACE`。
4. `required_scratch_size` 无条件把 `UpdateScratchDataSizeInBytes` 纳入容量，`release_build_resources` 实际不释放 scratch。
5. 每帧先调用 `SceneGeometry::prepare_animation`；只有它返回 dirty 时才执行 TLAS update，所以静态场景的逐帧 AS GPU 时间已经是 0，但静态场景仍承担了 update-capable TLAS 和常驻 update scratch 的内存代价。
6. `SceneGeometry::instance_descriptors` 直接从保存的 BLAS resource 取得 GPU VA。压缩后如果没有重建 instance desc，TLAS 会继续引用旧 BLAS 地址。
7. benchmark JSON 有 AS pass 时间和整体 local VRAM usage/budget，但没有 BLAS original/final allocation、压缩数量、TLAS 策略或常驻 scratch 数据。

## 3. 必须保持的 DXR 约束

### 3.1 BLAS 与实例动画

当前动画只改变 instance transform，不改 vertex/index buffer，因此 BLAS 几何仍是静态的。即使启用 `--animate-model`，满足条件的 BLAS 仍可压缩；变化的是 TLAS 是否允许 update。

不得为 BLAS 增加 `ALLOW_UPDATE`，也不得实现 refit、skinning、morph target 或动态顶点流。

### 3.2 TLAS flags 必须成对一致

新增纯策略函数，根据模式和真实动画能力返回 TLAS 策略：

```text
baseline：保持旧行为，初次 TLAS 始终 ALLOW_UPDATE | PREFER_FAST_TRACE
optimized + animate_model + 存在 animation group：ALLOW_UPDATE | PREFER_FAST_TRACE
optimized + 其他情况：仅 PREFER_FAST_TRACE
```

动态 TLAS 的初次 build、prebuild query 和后续 update 必须使用兼容的 base flags。update 时只在同一 base flags 上增加 `PERFORM_UPDATE`；禁止 query 一套 flags、build 另一套 flags。

静态 TLAS 不做 compaction。当前阶段不为 TLAS 引入第二套地址或重建路径。

### 3.3 BLAS compaction flags

```text
baseline：PREFER_FAST_TRACE
optimized：PREFER_FAST_TRACE | ALLOW_COMPACTION
```

只有使用 `ALLOW_COMPACTION` 构建的 BLAS 才能查询 compacted size 并执行 compact copy。禁止把 `ALLOW_COMPACTION`、`PERFORM_UPDATE` 或 `ALLOW_UPDATE` 混用到同一个 BLAS。

### 3.4 对齐与真实分配收益

DXR 返回的 compacted size 是逻辑结果大小，不等于 committed resource 实际占用。每个目标大小至少按 `D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BYTE_ALIGNMENT` 对齐，并使用 `ID3D12Device::GetResourceAllocationInfo` 计算原始和候选 resource 的实际 allocation bytes。

压缩决策必须满足：

1. compacted size 非 0；
2. compacted size 不大于原 `ResultDataMaxSizeInBytes`；
3. 对齐后的目标资源有效；
4. candidate allocation bytes **严格小于** original allocation bytes。

只减少逻辑 bytes、但 committed allocation 不变时必须跳过 compact copy，记录 `no_allocation_saving`。不要声称这种情况节省了实际显存。

## 4. A/B 配置与策略模型

新增：

```text
--acceleration-structure-mode baseline|optimized
```

建议类型：

```rust
pub enum AccelerationStructureMode {
    Baseline,
    Optimized,
}
```

要求：

- 加入 `RealtimeConfig`、CLI parser、help、CPU reference 冲突判断和测试。
- 8D 实现提交期间默认保持 `baseline`，先保证旧路径可回退。
- 只有第 12 节采纳门槛全部通过，最后的数据/决策提交才可把 AS mode 默认切换为 `optimized`。
- 模式只控制 AS 构建策略；不得复制两套 render loop。
- 启动 stderr 和 benchmark JSON 都必须报告真实 AS mode，避免 A/B 混淆。
- `--command-recording-mode` 的默认仍是 `optimized`，两种配置互不覆盖。

把 flags 选择拆成可单元测试的纯函数，不要在多个 build/update 调用点手写条件表达式。建议模型：

```rust
struct AccelerationStructurePolicy {
    compact_blas: bool,
    tlas_allow_update: bool,
    blas_flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS,
    tlas_build_flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS,
}
```

`tlas_allow_update` 的 optimized 条件必须是 `animate_model && geometry.has_animation_groups()`，不能只看 CLI，也不能只看场景是否含任意 instance。

## 5. 两阶段初始化与 Fence 生命周期

BLAS compacted size 必须由 GPU build 后产生，CPU 读取 size 后才能创建 compact destination，因此 optimized 路径不能继续假设所有 AS 在一个 command list 中完成。

### 5.1 Phase A：原始 BLAS build 与 size readback

1. 查询所有 BLAS prebuild info，创建 original BLAS。
2. optimized 使用 `PREFER_FAST_TRACE | ALLOW_COMPACTION`；baseline 使用旧 flags。
3. 构建全部 original BLAS，每个 build 后保留正确 UAV 顺序。
4. optimized 为所有 BLAS 分配一个连续 postbuild default buffer和一个 readback buffer。
5. 使用 `EmitRaytracingAccelerationStructurePostbuildInfo` 查询 `COMPACTED_SIZE`。可以一次提交全部 BLAS GPU VA，输出项按 primitive index 稳定对应。
6. 在复制 query 结果到 readback 前建立正确的 UAV/CopySource 顺序；readback 资源保持 CopyDest 语义。
7. close、execute、signal，并只在初始化路径等待 Phase A fence。

baseline 不需要发出 compaction query，但建议仍经过相同的“两阶段初始化外壳”，Phase B 直接复用 original BLAS。这样后续 TLAS 构建和资源生命周期只有一套控制流。

### 5.2 CPU 决策

Phase A fence 完成后才能 Map/read compacted sizes。逐 BLAS 调用纯决策函数并产生稳定记录：

```text
primitive_index
original_result_bytes
reported_compacted_bytes
original_allocation_bytes
candidate_allocation_bytes
decision = compacted | invalid_size | no_allocation_saving | disabled
```

无效 size 不能 panic 或继续 copy；应回退到 original BLAS，并在报告里计数。

### 5.3 Phase B：compact copy 与 TLAS build

1. Phase A fence 完成后才能 Reset 对应 command allocator/list。
2. 为决定 compact 的 BLAS 创建目标 resource，执行 `CopyRaytracingAccelerationStructure(..., COMPACT)`。
3. compact destination 在 TLAS build 前必须完成正确的 UAV 同步。
4. 未压缩的 primitive 继续使用 original BLAS；最终 BLAS vector 的顺序必须与 `instance_primitive_indices` 完全一致。
5. 用最终 BLAS GPU VA 重新生成所有 `D3D12_RAYTRACING_INSTANCE_DESC`。
6. 按第 3.2 节策略 query/build TLAS。
7. close、execute、signal，并等待 Phase B 初始化 fence。
8. 只有 Phase B fence 完成后，才可释放作为 compact source 的 original BLAS、postbuild default/readback、只用于 build 的 scratch 和上传资源。

初始化已有 fence value `1`，当前首个渲染 fence 从 `2` 开始。引入第二次初始化提交后必须统一管理单调递增的 fence value，不能让首帧复用仍代表初始化工作的值。推荐写一个仅初始化使用的 execute/signal/wait helper，最终 `next_fence_value` 从最后一个初始化值加一开始。

禁止把初始化等待复制到 render、resize、hot reload 或每帧 update 路径。

## 6. 资源所有权与 scratch 生命周期

建议将初始化中的 pending 资源显式建模，不要依赖局部变量恰好活到函数末尾：

```text
PendingAccelerationStructures
  original_blas
  prebuild info / compaction records
  shared scratch
  postbuild default/readback（optimized）

AccelerationStructures
  final_blas（压缩目标或未压缩 original）
  tlas
  frame_data
  update_scratch（仅动态 TLAS）
  initialization_retirements（仅存活到 Phase B fence）
  telemetry
```

具体要求：

- compact source original BLAS 必须活到 compact copy 对应 fence 完成。
- 未压缩 original BLAS 要移动进 final BLAS vector，不能同时进入 retirement 列表。
- 静态 TLAS 的 build scratch 在 Phase B fence 后释放。
- 动态 TLAS 只保留满足 `UpdateScratchDataSizeInBytes` 的 scratch；可以复用初始化 scratch，但容量必须由实际策略计算。
- `required_scratch_size` 改为接受 `tlas_allow_update`，静态模式不得无条件纳入 update scratch。
- `update()` 必须检查 `tlas_allow_update`。如果调用方违反策略，应返回带上下文的错误，而不是访问已经释放的 scratch。
- `instance_gpu` 的逐帧绑定不能因静态 TLAS 优化而失效。不要在 8D 顺手压缩 frame data 或重构 root SRV。

## 7. 遥测与 benchmark JSON

新增只读报告类型，例如：

```rust
pub struct AccelerationStructureStats {
    pub mode: &'static str,
    pub tlas_update_enabled: bool,
    pub blas_count: usize,
    pub compacted_blas_count: usize,
    pub invalid_compacted_size_count: usize,
    pub no_allocation_saving_count: usize,
    pub original_result_bytes: u64,
    pub final_result_bytes: u64,
    pub original_allocation_bytes: u64,
    pub final_allocation_bytes: u64,
    pub allocation_bytes_saved: u64,
    pub allocation_saving_ratio: Option<f64>,
    pub tlas_result_bytes: u64,
    pub tlas_allocation_bytes: u64,
    pub retained_update_scratch_required_bytes: u64,
    pub retained_update_scratch_allocation_bytes: u64,
}
```

benchmark JSON 增加顶层对象 `acceleration_structures`，至少包含以上聚合字段。还要保留逐 BLAS 的整数记录，字段名稳定、顺序按 primitive index；不要输出 COM 地址、本机路径或不可复现的指针。

计算规则：

- 所有总数使用 checked/saturating 累加，避免畸形模型导致溢出。
- `allocation_bytes_saved = original_allocation_bytes - final_allocation_bytes`，不得下溢。
- ratio 只有 denominator 非 0 且结果 finite 时才输出数字，否则为 `null`。
- baseline 的 compacted count 必须为 0，final allocation 等于 original allocation。
- optimized 的每个 compacted entry 必须满足 final allocation 小于 original allocation。
- local VRAM usage/budget 继续来自现有 adapter telemetry；AS 统计不能伪装成进程总显存。
- `GetResourceAllocationInfo` 返回 `UINT64_MAX` 时表示查询失败，必须拒绝；original/TLAS 查询失败应返回带上下文错误，candidate 查询失败则保留 original BLAS 并记录 invalid。
- 动态 TLAS scratch 必须同时报告 DXR required bytes 和 committed allocation bytes；兼容 JSON 字段 `retained_update_scratch_bytes` 表示 committed allocation，不得再填逻辑宽度。
- 这是向 schema v1 增加对象，不删除或改名现有字段；除非做破坏性变更，否则不要擅自更改 `schema_version`。

启动时向 stderr 输出一行简洁摘要，例如：

```text
DXR AS：optimized，BLAS 8/12 compacted，allocation 1536 -> 1024 KiB，TLAS update=false，retained scratch=0 KiB
```

stdout 在 benchmark 模式仍必须恰好只有一行 JSON。

## 8. 明确不做的内容

- 不压缩 TLAS。
- 不实现 BLAS update/refit、skinning、morph target 或顶点动画。
- 不合并 primitive、改变 hit-group/geometry index 映射或重排 instance ID。
- 不迁移 placed resource、heap suballocation、通用 scratch allocator 或 residency manager。
- 不修改 shader、SBT/root signature、descriptor layout、材质或随机采样。
- 不实施 8E render extent/dynamic resolution。
- 不实施 8F Inline Ray Query。
- 不增加每帧 fence wait、`Flush` 或全局 GPU idle。
- 不提交 PIX capture、构建产物、临时 readback 数据或本机专用大模型。

## 9. 单元测试最低要求

必须把不依赖 COM/GPU 的部分拆成纯函数测试：

1. CLI：baseline/optimized 正确解析；缺失值和未知值报错；与 `--cpu-reference` 冲突。
2. 默认：实现阶段默认 AS mode 为 baseline；命令记录默认仍为 optimized。
3. policy matrix：
   - baseline 静态/动画均保持旧 TLAS update-capable 行为；
   - optimized 静态不含 `ALLOW_UPDATE`；
   - optimized 只有 `animate_model && has_animation_groups` 才含 `ALLOW_UPDATE`；
   - optimized BLAS 含 `ALLOW_COMPACTION`，baseline 不含。
4. initial/update flags：动态 update 在相同 base flags 上只增加 `PERFORM_UPDATE`。
5. scratch：静态策略不计 update scratch；动态策略计入，且返回 BLAS build/TLAS build/TLAS update 三者最大值。
6. alignment：0、已对齐、非对齐和接近 `u64` 上限输入不会溢出。
7. compaction decision：invalid size、逻辑变小但 allocation 不变、真实 allocation 变小三种结果准确。
8. final BLAS selection：压缩与未压缩混合时保持 primitive 顺序。
9. telemetry aggregate：count、bytes、saving、ratio 与无分母行为正确。
10. benchmark JSON：仍为单行合法 JSON；旧字段保留；AS mode、策略、聚合和逐项字段稳定；NaN/Infinity 输出为 null。

不要为单元测试引入 WARP，不要让普通 `cargo test` 依赖 DXR 设备。

## 10. 推荐提交顺序

### 8D-0：模式、纯策略与遥测契约

建议提交：

```text
feat(stage8-dxr): add acceleration structure policy telemetry
```

任务：

1. 增加 `AccelerationStructureMode`、CLI/help/冲突判断。
2. 增加纯 policy、alignment、compaction decision 和 aggregate 类型及测试。
3. 扩展 benchmark JSON serializer 测试。
4. 暂不改变现有 GPU 行为，默认 baseline。

### 8D-1：两阶段初始化骨架

建议提交：

```text
refactor(stage8-dxr): split acceleration structure initialization
```

任务：

1. 拆分 Phase A/CPU decision/Phase B 所有权。
2. baseline 暂时全部选择 original BLAS，不发 compaction query。
3. TLAS 仍保持旧 flags，使该提交只改变初始化组织，不改变渲染策略。
4. 正确推进两次初始化 fence 和 `next_fence_value`。

### 8D-2：TLAS 策略和 scratch 释放

建议提交：

```text
perf(stage8-dxr): specialize TLAS update policy
```

任务：

1. optimized 按真实动画能力选择 TLAS flags。
2. 初次 build/update 使用兼容 flags。
3. 静态 Phase B fence 后释放 scratch，动态只保留 update 所需容量。
4. 加入策略、防误调用和 scratch 测试。

### 8D-3：BLAS postbuild query 与 profitable compaction

建议提交：

```text
perf(stage8-dxr): compact profitable static BLAS
```

任务：

1. 实现 postbuild default/readback 和 size 读取。
2. 只对真实 allocation 变小的 BLAS执行 compact copy。
3. 重建 instance desc 并在 Phase B 构建 TLAS。
4. Phase B fence 后退休 original/query/build-only 资源。
5. 输出真实逐项与聚合遥测。

### 8D-4：实测、默认决策和文档

建议提交：

```text
docs(stage8): record acceleration structure optimization results
```

任务：

1. 完成第 11、12 节验证。
2. 把所有真实命令、三次运行结果、allocation 数据和 validation 结果追加到 [`阶段8性能优化执行计划.md`](阶段8性能优化执行计划.md)。
3. 有代表性大 BLAS 且全部采纳门槛通过时，才把 AS mode 默认改为 optimized；否则保留 baseline，写明缺少的资产或失败门槛。
4. 更新主方案和本文状态，但不得把阶段 8 标记完成。

每个提交都必须能 `cargo test` 和 `cargo build`，不得提交故意不能构建的中间状态。

## 11. 自动化与真机正确性验证

### 11.1 必跑命令

```powershell
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build
cargo build --release
git diff --check
```

### 11.2 Debug + GPU-Based Validation

至少运行：

```powershell
target\debug\ray_tracing_demo.exe --benchmark-seconds 1 --output-size 1280x720 --atrous-mode baseline --command-recording-mode optimized --acceleration-structure-mode baseline
target\debug\ray_tracing_demo.exe --benchmark-seconds 1 --output-size 1280x720 --atrous-mode baseline --command-recording-mode optimized --acceleration-structure-mode optimized
target\debug\ray_tracing_demo.exe --model assets\gltf\Triangle\NonIndexedMultiNode.gltf --benchmark-seconds 1 --output-size 1280x720 --atrous-mode baseline --command-recording-mode optimized --acceleration-structure-mode optimized
target\debug\ray_tracing_demo.exe --model assets\gltf\Triangle\NonIndexedMultiNode.gltf --animate-model --benchmark-seconds 1 --output-size 1280x720 --atrous-mode baseline --command-recording-mode optimized --acceleration-structure-mode optimized
```

全部必须正常退出，并报告 `D3D12 Debug InfoQueue：0 条消息`；不得出现 invalid postbuild destination、AS copy mode、source lifetime、TLAS instance address、scratch size、build flags mismatch 或 device removed。

至少人工运行 optimized AS 动画路径 30 秒，切换全部 F1 视图并做一次最小化/恢复。确认几何不消失、instance ID/材质不漂移、动画仍围绕 world pivot、历史拒绝与运动向量没有异常。

### 11.3 画面一致性

AS 策略不应改变任何像素。固定 seed、同尺寸、同帧/历史长度的 baseline/optimized 最终视图和全部 F1 视图应一致。若程序没有自动截图接口，必须明确记录为人工对比，不得写“bit-identical”。

当前仓库只有小型离线 glTF 夹具，它们可能全部落在 committed resource 的同一 allocation 粒度内。若 `compacted_blas_count=0` 且 decision 全是 `no_allocation_saving`，这是合法结果，不得降低阈值或用逻辑 bytes 冒充实际显存收益。

要正式采纳 compaction 默认，必须另用一个可合法加载、具有至少一个跨 allocation 粒度的大 BLAS 的代表性静态 glTF。不要把本机大模型或下载产物提交进仓库；在报告中记录模型名称、来源/版本、primitive/triangle 数和是否纳入 Git。若没有该资产，明确写“compaction 大模型实测未完成”。

## 12. RTX 4060 Laptop A/B 与采纳门槛

关闭 Debug Layer/GPU Validation，使用 Release、插电、相同驱动和电源设置。显式固定 `--atrous-mode baseline --command-recording-mode optimized`，避免同时比较 8B/8C。

### 12.1 建议矩阵

每组运行三次、每次 30 秒并取三次中位数：

```text
Cornell 1600x900：AS baseline / optimized
小型静态 glTF 1600x900：AS baseline / optimized
小型动画 glTF 1600x900：AS baseline / optimized + --animate-model
代表性大静态 glTF 1600x900：AS baseline / optimized（若可用）
```

每次记录：

- GPU Total、AS、Path Trace p50/p95 和有效样本数。
- AS mode、TLAS update enabled、BLAS 总数/压缩数/跳过原因。
- original/final result bytes。
- original/final allocation bytes、saved bytes 和 ratio。
- retained update scratch bytes。
- local VRAM usage/budget、GPU 名称、PIX 状态。
- 启动是否发生 validation/error；动画场景 history reset 和 extent change 是否异常。

### 12.2 硬正确性门槛

以下必须全部满足：

1. baseline 和 optimized Debug GPU Validation 均为 0 条消息。
2. static optimized 的 `tlas_update_enabled=false`、AS pass 稳态 p50/p95 为 0/0；animated optimized 为 true 且持续正确 update。
3. animated AS p95 不高于 1.5 ms，并且不比 baseline 回退超过 3% 或 0.05 ms（取较宽者，避免 0 附近百分比失真）。
4. 每个 compacted BLAS 的 reported size 合法，且 final allocation 严格小于 original allocation。
5. 总数守恒：compacted + 各 skip 原因 = BLAS count；bytes 聚合与逐项相符。
6. TLAS instance desc 全部引用 final BLAS；画面、F1 数据、材质和动画正确。
7. Phase B fence 前不释放 source；Phase B 后 build-only 资源可释放，静态 retained update scratch 为 0。
8. local VRAM usage/budget 小于 70%，连续运行不持续增长。

### 12.3 默认采纳门槛

只有以下额外条件也满足，才能把 AS mode 默认改为 optimized：

1. 至少一个代表性大 BLAS 实际执行 compact copy，且总 final allocation 比 original 至少减少 5%。
2. Cornell、静态 glTF 和动画 glTF 的 GPU Total p95 均不得回退超过 3%。
3. 大模型静态 Path Trace p95 不得出现超过 3% 的无法解释回退。
4. baseline/optimized 画面对比通过。
5. Release 三次数据、驱动版本、电源/插电状态和资产信息完整记录。

如果没有可代表的大 BLAS、compacted count 为 0、画面对比未做或任一门槛失败：保留 `baseline` 默认和 optimized A/B 路径，记录结果，交给 Codex review；不要为了“完成 8D”修改门槛。

## 12.4 本次实现状态（2026-08-09）

已实现 baseline/optimized AS mode、Phase A/CPU decision/Phase B 初始化、BLAS postbuild
compaction query、真实 committed allocation 比较、最终 BLAS GPU VA 重建 TLAS、静态/动态
TLAS scratch 策略和稳定 benchmark JSON 遥测。RTX 4060 Laptop 的 Cornell、NonIndexedMultiNode
静态和动画小场景 Release 1600×900 三次 A/B 均显示 `compacted_blas_count=0`、真实 allocation
节省为 0；仓库没有达到采纳门槛所需的代表性大 BLAS，因此默认保持 baseline。

四项 Debug + GPU-Based Validation（Cornell baseline/optimized、静态 glTF、动画 glTF）均为
exit code 0 且 `D3D12 Debug InfoQueue：0 条消息`。F1 全视图、resize、最小化/恢复、截图
diff 和代表性大模型实测尚未完成，不能把 8D 或阶段 8 标记为完成。

首轮 Codex review 修复补充：allocation query 现在拒绝 `UINT64_MAX` 失败哨兵；动态 TLAS 的 `3,328 B` 是逻辑 required size，对应当前 committed buffer 的真实 allocation 为 `65,536 B`，两者已在 JSON 中分字段报告。高层 README、主方案和阶段状态已同步，下一代码工作包为 8E-1。

## 13. Review 高风险清单

Codex review 会重点检查：

1. 是否把逻辑 compacted bytes 错当成 committed allocation 节省。
2. query buffer 的状态、offset、元素大小和 primitive 映射是否正确。
3. 是否只有使用 `ALLOW_COMPACTION` build 的 BLAS 执行 compact copy。
4. compact source 和 query/readback 是否活到对应 fence 完成。
5. compact destination 是否在 TLAS build 前正确同步。
6. instance desc 是否在 compaction 决策后用 final GPU VA 重新生成。
7. mixed compacted/original BLAS 是否保持 primitive index 顺序。
8. static/dynamic TLAS prebuild、initial build 和 update flags 是否兼容。
9. static scratch 是否真正释放，dynamic scratch 是否覆盖 update 最大需求。
10. 两次初始化 fence 是否与首帧 fence 单调、不冲突。
11. 是否误把初始化 wait 放进每帧、resize 或 hot reload。
12. JSON 是否保留旧字段和单行 stdout 契约，统计是否可由逐项记录复算。
13. 是否把缺少大模型实测或人工画面对比伪装成已通过。

## 14. 官方参考

- Microsoft [`D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS`](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ne-d3d12-d3d12_raytracing_acceleration_structure_build_flags)
- Microsoft [`ID3D12GraphicsCommandList4::EmitRaytracingAccelerationStructurePostbuildInfo`](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12graphicscommandlist4-emitraytracingaccelerationstructurepostbuildinfo)
- Microsoft [`ID3D12GraphicsCommandList4::CopyRaytracingAccelerationStructure`](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12graphicscommandlist4-copyraytracingaccelerationstructure)
- Microsoft [`ID3D12Device::GetResourceAllocationInfo`](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12device-getresourceallocationinfo)
- Microsoft [`D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_COMPACTED_SIZE_DESC`](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ns-d3d12-d3d12_raytracing_acceleration_structure_postbuild_info_compacted_size_desc)

## 15. Luna 交付格式

最终回复必须包含：

1. commit 范围和每个 commit 的主题。
2. 修改文件清单与两阶段初始化/资源所有权说明。
3. 自动检查的实际命令和结果。
4. Debug GPU Validation 的实际命令、退出结果和 InfoQueue 条数。
5. Release A/B 的全部原始 JSON 或可复核表格，以及三次中位数。
6. 每个场景的 BLAS count、compacted/skip count、original/final allocation 与 savings。
7. 是否有代表性大 BLAS；若没有，明确列为未完成项。
8. 画面对比采用自动 diff 还是人工检查；未做则明确写未做。
9. 默认 AS mode 是否改变及其门槛依据。
10. 已知风险、未完成项和建议 Codex review 的高风险位置。

不要只回复“已实现”或“测试通过”，也不要估算没有实际运行的数据。
