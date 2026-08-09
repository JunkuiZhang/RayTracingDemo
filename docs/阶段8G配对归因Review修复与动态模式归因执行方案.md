# 阶段 8G：配对归因 Review 修复与动态模式归因执行方案

## 1. 文档目的

本文审查 Luna 在 `3d55830` 之后提交的阶段 8G H1–H4 实现，并给出下一轮可直接执行的修复包。下一轮只修验收证据链并做短时动态模式归因，不修改 renderer、Rust 渲染路径或 HLSL。

审查提交范围：

```text
7a2a440 fix(stage8-validation): compute medians and environment reliably
69b3c30 test(stage8): record reproducible debug validation evidence
30073cd test(stage8): add paired 1080p attribution benchmark
6a9619e fix(stage8-validation): align paired evidence run root
aa2b0b7 fix(stage8-validation): preserve paired run count parameter
d6a868a docs(stage8): record corrected validation and paired attribution
```

目标硬件仍为 **NVIDIA GeForce RTX 4060 Laptop GPU**。

## 2. Review 结论

本轮不能整体无条件接受，但也没有发现 renderer 或画质路径回退。H1、H2 可以接受；H3 的本次数值有真实 raw 支撑，reference 也能由当前未提交的 archive 人工还原为 `2031abf`，但配对脚本生成的 summary 不是自认证证据，仍需修复后重跑一组短配对。

### 2.1 已通过的部分

- `Get-Median` 已改为显式 `Floor`，空、单值、奇数、偶数和七元素五组无 GPU 自测通过。
- `stage8_acceptance.ps1 -Suite Recheck1080` 只执行 forced-dynamic smoke、fixed 1080p 和 default dynamic 1080p，没有重跑 54 capture/23 diff，也没有运行长测。
- 环境快照在本机正确识别 RTX 4060 Laptop、驱动、平衡电源方案和 AC；字段保留了数据来源。
- Debug summary `debug-20260809-193534-bc7a77b5` 保存了五条命令的 args/stdout/stderr/exit/JSON。五条均 exit 0、InfoQueue 0，capture 为 320×180、8 SPP，SHA-256 与文档一致。
- corrected Recheck summary `20260809-195413-09b167f5` 诚实保存 `passed=false`：fixed 1080p 三次 p95 为 `7.25/7.06/7.07 ms`，正确 median `7.07 ms`；default dynamic 为 `7.76/7.93/7.98 ms`，median `7.93 ms`，超过 `7.6632 ms`。
- paired summary `paired-1080-20260809-194700` 保存了交错顺序和六份 raw：candidate 为 `7.30/7.47/7.42 ms`，median `7.42 ms`；reference 为 `6.94/7.34/7.67 ms`，median `7.34 ms`；差值 `+1.089918%`。
- 当前 archive `reference-2031abf.zip` 的 ZIP comment 为完整 commit `2031abfc1992661b7186b68e58b5ff5f1c3712de`，与 `git rev-parse 2031abf` 一致；archive SHA-256 为 `52D3039D988EA7331B4C72E72D387CA3FAB76EC92719459D14E5ADC0BDB0ACD5`。这只能作为本次 Codex 人工补验，不能替代脚本修复。
- PowerShell AST、median 自测、`cargo fmt --all -- --check`、`cargo test --all-targets`（101 + 4）、`cargo clippy --all-targets --all-features -- -D warnings` 和 `git diff --check 3d55830..HEAD` 均通过。

### 2.2 Review findings

#### R8G2-P1：paired summary 没有把测试二进制绑定到声称的 commit

`scripts/stage8_paired_benchmark.ps1` 当前只把 `CandidateExe`、`ReferenceExe` 路径和主工作树 `git_head` 写入 summary。它没有记录或验证：

- reference/candidate 的完整 commit 和 tree；
- archive comment 与目标 commit 是否一致；
- archive、`Cargo.lock` 和两个 EXE 的 SHA-256；
- reference EXE 是否确实由该 archive 构建；
- candidate EXE 是否是当前 commit 的新鲜构建。

因此，把同一个 EXE 或任意旧 EXE 作为两侧参数，也能生成看似正式的 `NO PAIRED REGRESSION`。本次 output 目录可以由 Codex 额外检查 archive，但 summary 本身无法独立证明文档声称的 `2031abf` attribution。H3 在修复前只能写成“本次 raw 数值可信、provenance 需加固”，不能写成通用可复现闭环。

#### R8G2-P2：`-Runs` 参数契约与实际序列冲突

脚本声明 `Runs` 允许 `1..10`，实际 sequence 永远硬编码三次 candidate 和三次 reference。传 `-Runs 1`、`-Runs 2` 或 `-Runs 4` 都不会执行请求的次数，最终只因计数不相等而变成 `INCONCLUSIVE`。`aa2b0b7` 修复了 PowerShell 大小写不敏感导致的变量碰撞，但没有修复这个公开参数契约。

本方案固定要求每侧恰好三次，因此应把参数限制为 3，或直接取消可变 Runs；不得假装支持 1..10。

#### R8G2-P2：三个 runner 的子进程等待没有上限

`stage8_acceptance.ps1`、`stage8_debug_validation.ps1` 和 `stage8_paired_benchmark.ps1` 都对 `Start-Process` 直接调用无限期 `WaitForExit()`。本次 Debug 的单条 1 秒 benchmark 因 GPU Validation 初始化实际耗时约 94–101 秒，虽然最终成功，但任何启动、驱动或窗口异常都可能让脚本永久挂住，不符合“短、受限、禁止超长测试”的执行要求。

runner 必须给自己启动的子进程设置与 case 类型相符的墙钟上限；超时只终止该子进程，记录 `timed_out=true` 并让整组失败，不得杀其他用户进程。

#### R8G2-P2：paired 环境只在开始时采样

当前 paired runner 只保存运行前环境，不能兑现“运行期间断电或电源方案变化则整组 invalid”。修复后必须保存 before/after 两份快照，至少比较 GPU、driver、活动电源方案和 AC/battery。任一关键字段不可用、前后不一致或不为 AC 时，paired 结论必须为 `INCONCLUSIVE`。

#### R8G2-P2：主实施方案保留了互相矛盾的状态段落

`docs/实时DXR渲染器实施方案.md` 中最新 H1–H4 段落已经写 Debug raw 完成，但紧接的“当前状态”仍写“五条 Debug 短验证缺少可复核 raw 日志、下一包补证据”。这两段不能同时为真。后续文档提交必须删除过期叙述，而不是在其前后继续叠加新状态。

## 3. 下一轮范围和禁令

### 3.1 本轮必须完成

1. 让 commit paired runner 自行导出、构建、哈希并验证 candidate/reference，生成自认证 schema v2 summary。
2. 修正固定三次的参数契约、增加前后环境一致性检查和子进程超时。
3. 给三个 runner 增加无需 GPU 的自测试/AST 可检查路径；本轮不重跑五条 Debug。
4. 用修复后的 runner 重跑 `candidate HEAD` 对 `reference 2031abf` 的固定 1080p 三次配对。
5. 新增同一 candidate EXE 的 default dynamic 对 fixed 1.0 交错短配对，判断 `7.93 ms` 是动态模式开销还是运行顺序/环境波动。
6. 根据新 raw 和 review findings 统一文档状态。

### 3.2 硬性禁止

- 禁止运行 1800 秒、600 秒或任何单条超过 30 秒 benchmark；禁止重跑完整 acceptance、54 capture/23 diff 和五条 Debug Validation。
- 禁止修改 Rust、renderer、HLSL、动态分辨率控制公式、默认模式或性能/图像门槛。
- 禁止用降低 `7.6632 ms`、`16.67 ms`、3% 等门槛制造通过。
- 禁止自动重跑失败 case、删除失败 run、挑最好值或在中途改变交错顺序。
- 禁止切换、reset 或污染主工作树；reference/candidate 源码必须导出到新的 ignored output run 目录。
- 禁止提交 `output/`、archive、target、EXE、JSON、PNG、stdout/stderr 或本机绝对路径。
- 禁止改变系统电源计划、结束不属于 runner 的进程、下载模型或安装 PIX。

## 4. 工作包 I1：修复自认证 commit paired runner

### 4.1 参数和固定工作负载

重构 `scripts/stage8_paired_benchmark.ps1`，建议接口：

```powershell
.\scripts\stage8_paired_benchmark.ps1 `
  -CandidateCommit HEAD `
  -ReferenceCommit 2031abf `
  -Runs 3 `
  -Seconds 30 `
  -OutputRoot output/stage8g
```

要求：

- `Runs` 只能为 3；传其他值必须在创建 run 目录和启动进程前由参数绑定或显式校验失败。
- `Seconds` 保持 `1..30`，本轮正式值为 30。
- workload 不变：Cornell、1920×1080、fixed scale 1.0、optimized command recording、baseline À-Trous、baseline AS。
- 顺序固定为 `candidate-1, reference-1, reference-2, candidate-2, candidate-3, reference-3`。

### 4.2 两侧从 commit 独立构建

runner 必须在唯一 run root 下对两侧执行相同流程：

1. 用 `git rev-parse --verify <rev>^{commit}` 解析完整 commit；用 `git rev-parse <commit>^{tree}` 保存 tree。
2. 用 `git archive --format=zip` 分别生成 candidate/reference archive，禁止复制当前工作树。
3. 读取 ZIP EOCD comment 并验证等于完整 commit；不接受仅从文件名猜 commit。
4. 展开到各自的 `candidate-src/`、`reference-src/`。
5. 在各自独立 target 目录执行 `cargo build --release --locked --offline`。构建 stdout、stderr、exit code、命令和墙钟时间都要保存；构建失败写 summary 后停止，不得修改 archive 源码。
6. 分别记录 commit、tree、archive SHA-256、archive comment、`Cargo.lock` SHA-256、EXE SHA-256、EXE bytes 和构建命令。
7. benchmark 启动前再次计算 EXE SHA-256，必须与 build manifest 一致。

summary 使用 `schema_version=2`，至少包含：

```text
provenance.candidate.commit/tree/archive_sha256/archive_comment/cargo_lock_sha256/exe_sha256
provenance.reference.commit/tree/archive_sha256/archive_comment/cargo_lock_sha256/exe_sha256
provenance.valid
```

两侧 archive 和 EXE 必须位于本次唯一 run root。任一字段缺失或校验不一致，整组 `INCONCLUSIVE`，非零退出。

### 4.3 环境连续性和有界等待

- 保存 `environment_before.json` 和 `environment_after.json`。
- 两份快照都要保留 source/error 字段，电源优先使用 `GetSystemPowerStatus`，再 fallback `Win32_Battery`。
- before/after 的 GPU 名、driver、活动电源方案、电源来源必须可用且一致；电源必须为 AC。
- 对 Release 30 秒 benchmark 使用不超过 90 秒的墙钟上限；实际超时写入 raw record 的 `timed_out=true`，终止该 child 及其 child tree，停止剩余序列并输出 `INCONCLUSIVE`。
- Debug runner 默认上限可设为 180 秒，以覆盖本次约 100 秒的 Debug 初始化；本轮只改代码和无 GPU 自测，不实际重跑 Debug。
- acceptance runner 的 timeout 应按 workload 派生，不能把未来合法 long-run 误限为 90 秒；但本轮不得执行 long-run。

### 4.4 无 GPU 自测试

`-SelfTest` 在检查 EXE、commit、output 之前退出，至少覆盖：

- 正确 median 的奇偶样本；
- sequence 恰好六项、两侧各三项、顺序固定；
- `Runs != 3` 被拒绝；
- ZIP comment 解析对一个小测试 ZIP 的正确/错误情况；
- provenance 缺字段、hash 不一致、before/after 电源变化都判 invalid；
- timeout record 不能被 validator 判为通过。

建议提交：

```text
fix(stage8-validation): make paired evidence self-authenticating
```

提交后再执行正式 paired run；不要把 output stage 进 Git。

## 5. 工作包 I2：同一 candidate 的 dynamic/fixed 模式归因

新增 `scripts/stage8_resolution_pair.ps1`。它不得再次构建，也不得接受任意未认证 EXE；必须读取 I1 本次 run 生成的 candidate build manifest，重新计算 EXE SHA-256 并与 manifest 比较。

建议接口：

```powershell
.\scripts\stage8_resolution_pair.ps1 `
  -CandidateManifest output/stage8g/<i1-run>/candidate-build-manifest.json `
  -Runs 3 `
  -Seconds 30 `
  -OutputRoot output/stage8g
```

### 5.1 两侧 workload

共同参数：

```text
--benchmark-seconds 30
--output-size 1920x1080
--command-recording-mode optimized
--atrous-mode baseline
--acceleration-structure-mode baseline
```

fixed 侧增加：

```text
--render-scale 1.0
```

dynamic 侧增加：

```text
--dynamic-resolution
```

固定交错顺序：

```text
dynamic-1, fixed-1, fixed-2, dynamic-2, dynamic-3, fixed-3
```

每侧只允许三次，不自动补跑。

### 5.2 validator

两侧共同要求：

- exit 0、非超时、stdout 恰好一行 JSON、stderr 无 panic/device removed；
- GPU 名、commit manifest 和 EXE hash 有效；
- output 为 1920×1080，valid samples > 0；
- Total p50/p95 有限、p95 >= p50 且每次 p95 <= 16.67 ms。

fixed 侧额外要求 `resolution_mode=fixed`，render 为 1920×1080。

dynamic 侧是“原生尺寸动态模式开销”对照，因此必须满足：

- `resolution_mode=dynamic`；
- measurement 期间 render min/max 和最终 render 都为 1920×1080；
- measurement `switch_count=0`、`downscale_count=0`、`upscale_count=0`；
- 没有 stale-generation sample。

若 dynamic 发生真实尺寸切换，本组对“native dynamic overhead”而言是 `INCONCLUSIVE`，不得把不同分辨率性能直接与 fixed 比较。

### 5.3 统计和决策

分别计算三次 Total p95 的正确 median：

```text
dynamic_vs_fixed_percent = (dynamic_median / fixed_median - 1) * 100
```

summary 必须分开保存两个轴，不能只用一个模糊 `passed`：

- `absolute_gate = PASS|FAIL|INCONCLUSIVE`，dynamic median 门槛为 `7.6632 ms`；
- `mode_regression = PASS|FAIL|INCONCLUSIVE`，`dynamic_vs_fixed_percent <= 3%` 为 PASS；
- `decision` 按下表生成。

| 条件 | decision | 退出码/后续 |
|---|---|---|
| dynamic `<=7.6632` 且差值 `<=3%` | `HISTORICAL DYNAMIC GATE PASS` | 0；仍不标阶段完成 |
| dynamic `<=7.6632` 但差值 `>3%` | `GATE PASS / DYNAMIC MODE OVERHEAD` | 非零；停止，交 Codex review |
| dynamic `>7.6632` 但差值 `<=3%` | `ABSOLUTE GATE FAIL / NO DYNAMIC-MODE REGRESSION` | 非零；保留环境方差证据 |
| dynamic `>7.6632` 且差值 `>3%` | `DYNAMIC-MODE REGRESSION` | 非零；停止，禁止自行改 renderer |
| 任一 run/provenance/环境无效或 dynamic 发生切换 | `INCONCLUSIVE` | 非零；不补跑、不挑值 |

同样保存 before/after 环境和每条 raw；同样使用 90 秒 child timeout。

建议提交：

```text
test(stage8): add bounded dynamic mode attribution
```

## 6. 工作包 I3：文档统一

更新以下文档：

- `README.md`
- `docs/实时DXR渲染器实施方案.md`
- `docs/阶段8性能优化执行计划.md`
- `docs/阶段8G总体验收记录.md`
- 必要时在旧执行方案末尾追加 superseded 链接，不要改写历史要求。

必须做到：

1. 删除“Debug raw 已完成”和“Debug raw 仍缺失”并存的矛盾段落。
2. 把旧 `paired-1080-20260809-194700` 标为“raw 数值有效，provenance 由 Codex 人工补验，但 schema v1 不自认证”。
3. 只有新的 schema v2 summary 全部校验通过，才写 reference/candidate commit attribution 已闭环。
4. 分别记录 commit pair 和 resolution-mode pair 的 run-id、六个 raw p95、median、百分比、两个 gate 和 exit code。
5. corrected Recheck 的 dynamic `7.93 > 7.6632` 仍保留 FAIL；新 mode pair 不能删除或覆盖这条历史证据。
6. 不声称重跑 54/23、五条 Debug 或长测。
7. 真实 F1/F2/resize/最小化/恢复/hot-reload、公开 PBR/大模型、PIX UI 和 1800/600 秒债务继续列为未完成；阶段 8 仍不得标记完成。

建议提交：

```text
docs(stage8): record authenticated paired attribution
```

## 7. 最终检查

不启动 GPU 的检查：

```powershell
.\scripts\stage8_acceptance.ps1 -SelfTest
.\scripts\stage8_paired_benchmark.ps1 -SelfTest
.\scripts\stage8_resolution_pair.ps1 -SelfTest
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
git diff --check
```

另用 PowerShell AST parser 检查三个改动 runner 和新脚本。正式 GPU 命令只有：

- commit paired：6 × 30 秒；
- resolution mode paired：6 × 30 秒。

不得再运行其他 GPU 矩阵。构建时间不属于 benchmark，但也必须有界并记录。

最终交付必须列出：

- 每个 commit hash/message；
- 两个新 run-id 和 summary 路径；
- candidate/reference 完整 commit、tree、archive/EXE SHA-256；
- 两组各六个 raw p95、正确 median、百分比、gate、decision 和脚本 exit code；
- before/after 环境一致性；
- 每条 benchmark 为 30 秒且未发生 timeout；
- 未执行的 54/23、Debug、1800/600、GUI/PBR/PIX 债务；
- `git status --short` 结果。

完成后停止，交回 Codex review；不要自行修改 renderer，也不要继续下一阶段。

## 8. 给 Luna 的提示词

```text
请在当前 RayTracingDemo 仓库严格执行 docs/阶段8G配对归因Review修复与动态模式归因执行方案.md。

先完整阅读该文档和它引用的现有 runner/阶段 8G 验收记录，再从当前 HEAD 开始工作。按 I1、I2、I3 顺序实现，可拆成 3 个 commit：
1) fix(stage8-validation): make paired evidence self-authenticating
2) test(stage8): add bounded dynamic mode attribution
3) docs(stage8): record authenticated paired attribution

硬性要求：
- 不修改 Rust、renderer、HLSL、动态控制器、默认模式或任何门槛。
- 禁止 1800/600 秒、完整 acceptance、54 capture/23 diff 和五条 Debug 重跑；任何 benchmark 单条最多 30 秒。
- paired runner 必须从 commit archive 对 candidate/reference 使用相同方法独立构建，验证完整 commit/tree、ZIP comment、archive/Cargo.lock/EXE SHA-256，并把 provenance 写进 schema v2 summary。
- Runs 只允许 3；固定交错顺序，不自动重跑、不挑值。
- 三个 runner 的 child wait 必须有界；timeout 只终止自己启动的 child tree，保存 timed_out 和失败 summary，不得结束用户进程。
- commit pair 和 dynamic/fixed pair 都要保存 before/after 环境，确认 RTX 4060 Laptop、driver、电源方案一致且全程 AC。
- dynamic/fixed pair 只有在 dynamic 全程保持 1920x1080 且 switch=0 时才可比较；否则 INCONCLUSIVE。
- 旧 Recheck dynamic 7.93 ms FAIL 必须保留；不要把 paired 结果写成阶段 8 完成。
- output、archive、target、EXE、JSON、日志一律不提交。

先提交 I1，再运行新的 commit paired；再提交 I2 并运行 dynamic/fixed paired；最后按真实结果提交 I3。执行 AST/self-test/fmt/test/clippy/diff check。若 build、provenance、环境或任一 run 无效，保留证据、非零退出并停止，不要自行修 renderer。

最终回复完整列出 commits、run-id、两组 raw/median/百分比/gate/exit code、provenance hash、环境前后状态、测试墙钟和未执行项，然后停止交回 Codex review。
```
