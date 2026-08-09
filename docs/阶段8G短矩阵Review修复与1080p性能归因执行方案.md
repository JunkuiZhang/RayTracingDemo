# 阶段 8G：短矩阵 Review 修复与 1080p 性能归因执行方案

## 1. 基线与任务结论

- 方案日期：2026-08-09。
- 目标硬件：NVIDIA GeForce RTX 4060 Laptop GPU。
- Luna 交付提交：`bf0aba3 docs(stage8): record post-review short acceptance`。
- Review 基线：`3b87a52..bf0aba3`。
- 执行者：Luna；完成后由 Codex review。
- 本包性质：修复验收基础设施、补齐可复核 Debug 证据，并归因 1080p 的轻微性能门槛失败；不是渲染算法优化包。
- 长时限制：**禁止运行 1800 秒、600 秒或其他生命周期长测**。

`bf0aba3` 没有修改 renderer 或脚本，只提交了结果文档。Luna 正确执行了带 `-SkipLongRun` 的短矩阵，诚实保留了 runner exit 1 和 `summary.passed=false`，没有改阈值、挑数据或把阶段 8 提前标记完成。以下证据经 Codex 直接复核，可继续采用：

- run `output/stage8g/20260809-184649-8143bd8e/` 的 `git_head` 为 `3b87a52`，`long_run=null`；
- 20 个 benchmark 的 `benchmark_seconds` 都是 30，单次最大墙钟约 32.7 秒；
- 12 个 case 中只有 fixed 1920×1080 case 被 runner 判失败，所有 20 个 raw run 的逐次 validation 均通过；
- 54/54 capture 成功、实际 SPP 均为 128、名称和 SHA-256 完整；
- 23/23 diff 成功，22 个 exact 对全部为零，À-Trous 对 `max_abs=1`、`RMSE=0.0070904784`；
- forced dynamic measurement 为 `6407 valid + 6 stale = 6413 Total`，3 次 measurement switch 与 generation create/switch/retire、extent/history 对账，最终 retired 为 0、high-watermark 为 2、idle wait 为 0；
- Debug capture 文件 SHA-256 与文档一致；抽查 fixed 1.0/0.67、forced dynamic final/history-rejection 没有发现新的结构化黑边、漏光或历史错位。低 scale 更锯齿/更噪属于预期，但不替代真实交互观察。

本包必须处理下列 review findings。

## 2. Review findings

### R8G-P1：`Get-Median` 对三次运行取成最大值

`scripts/stage8_acceptance.ps1` 当前使用：

```powershell
$middle = [int]($numbers.Count / 2)
```

PowerShell 将 `1.5` 转为 `[int]` 时使用银行家舍入，`3 / 2` 得到索引 2，而不是中位索引 1。因此三次运行的所谓 median 实际取了最大值。Codex 从 raw JSON 重算得到：

| case | raw p95（排序后，ms） | runner 值 | 正确中位数 |
|---|---|---:|---:|
| fixed 1280×720 | 3.47 / 3.48 / 3.48 | 3.48 | 3.48 |
| fixed 1600×900 | 5.46 / 5.47 / 5.51 | 5.51 | 5.47 |
| fixed 1920×1080 | 7.63 / 7.67 / 7.69 | 7.69 | 7.67 |
| default dynamic | 7.43 / 7.48 / 7.64 | 7.64 | 7.48 |

正确的 fixed 1080p 中位数 `7.67 ms` 仍略高于 `7.6632 ms`，所以现有 run 的最终状态仍是 FAIL；但 summary 和所有引用 `7.69/7.64/5.51` 的文档必须修正。该 bug 在其他数据分布下也可能错误决定 case 状态，不能只改文档数字。

### R8G-P2：五条 Debug Validation 没有保存可复核 raw 日志

`bf0aba3` 文档声称四条 Debug benchmark 和一条 Debug capture 均 exit 0、InfoQueue 0，并给出墙钟时间；但 run 目录只保留了 `debug-gltf-as-capture.png`，没有这五条命令的 stdout、stderr、参数、退出码或 summary。截图哈希可复核，另外四条 Debug Validation 结论目前只能依赖执行者转述。

下一包必须用独立脚本把 Debug 命令和 raw 证据写到唯一的 `output/stage8g/<run-id>/`。在该证据生成前，文档中的“五条 Debug PASS”应降为 `UNVERIFIED RAW EVIDENCE`，不能作为阶段完成依据。

### R8G-P2：runner 环境快照不完整

正式 summary 的 GPU 名称可从 benchmark JSON 回填，但 `driver_version` 和 `power_source` 为 `N/A`。Codex 当前只读复核得到：

- CIM driver：`32.0.16.1062`；
- `nvidia-smi` driver：`610.62`；
- 活动电源方案：平衡；
- `Win32_Battery.BatteryStatus=2`：AC。

Luna 文档写了独立 R0 快照，但没有持久化 raw 环境文件。runner 应增加可靠 fallback 和来源字段，不能要求后续 reviewer 相信未保存的控制台输出。

### R8G-P3：文档派生值和提交顺序需校正

- `docs/阶段8G总体验收记录.md` 的 raw 三次值是正确的，但 900p、1080p 和 default dynamic 的 median 派生值错误。
- 提交列表把 `3b87a52` 写在其父提交 `ba76cb4` 前面；应按真实时间或拓扑顺序排列。
- 现有 54/23、dynamic counter、显存和 capture/diff 结论可保留，不需要重跑完整截图矩阵。

## 3. 范围与硬性禁令

### 3.1 必须完成

1. 修复 PowerShell median 并增加无需 GPU 的自测试。
2. 让 runner 可靠记录 GPU driver、活动电源方案和 AC/battery 状态及其数据来源。
3. 增加只执行 1080p 复核的受限 suite，避免重跑 54 张 capture 和无关 case。
4. 增加可复现的 Debug Validation 记录脚本，并实际保存五条短验证的 raw 证据。
5. 做当前候选与阶段 8E-2 基线 `2031abf` 的同机、交错顺序、固定 1080p 配对测试。
6. 根据真实结果校正文档；阶段 8 仍不得直接标记完成。

### 3.2 禁止事项

- 禁止运行 1800/600 秒测试；任何 acceptance runner 调用仍必须显式 `-SkipLongRun`。
- 禁止重跑完整 54 capture/23 diff 矩阵；现有证据已经通过 review。
- 禁止修改 renderer、Rust 渲染逻辑、HLSL、动态分辨率公式或默认 mode。
- 禁止修改 `7.6632 ms`、`16.67 ms`、3%、70% 或图像阈值来制造通过。
- 禁止下载模型、安装 PIX、改变系统电源计划、结束用户进程或清理用户文件。
- 禁止用历史最好值替换本轮失败，禁止删失败 run，禁止只报告候选或基线中的一方。
- 禁止提交 `output/`、reference 源码副本、构建产物、PNG、JSON、日志、外部资产或本机绝对路径。

## 4. 工作包 H1：修复 runner 统计与环境快照

### 4.1 正确 median

把中间索引改为明确的向下取整，例如：

```powershell
$middle = [int][Math]::Floor($numbers.Count / 2.0)
```

不能依赖隐式或显式 `[int]` 对 `.5` 的转换。空集合返回 `$null`，偶数集合仍取中间两项平均值。

为 `scripts/stage8_acceptance.ps1` 增加 `-SelfTest`，该模式必须在解析 executable、创建 output 目录或启动 GPU 进程之前退出。至少覆盖：

| 输入 | 期望 |
|---|---:|
| 空数组 | `$null` |
| `3` | 3 |
| `3,1,2` | 2 |
| `4,1,3,2` | 2.5 |
| `7,1,6,2,5,3,4` | 4 |

自测试失败应非零退出并指出 case；成功只输出一行稳定结果。不得引入 Pester 或网络依赖。

### 4.2 环境快照 fallback

环境采集按以下顺序：

1. GPU 名称优先使用 benchmark JSON 中实际选中的 adapter；预检可用 `Win32_VideoController`。
2. Windows driver version 优先 `Get-CimInstance Win32_VideoController`；失败时使用 `nvidia-smi --query-gpu=driver_version --format=csv,noheader`，并分别记录 `driver_version` 与 `driver_version_source`。
3. 活动电源方案继续使用 `powercfg /getactivescheme`，增加 `active_power_scheme_source`。
4. AC 状态优先 `GetSystemPowerStatus` 或等价的只读 Win32 API；失败时 fallback 到 `Win32_Battery.BatteryStatus`。记录 `power_source` 和 `power_source_probe`；无法判断才写 `N/A`。
5. 查询失败应记录短错误字段或 source=`unavailable`，不能吞掉所有异常后只留下无法解释的 N/A。

不得把 `nvidia-smi 610.62` 和 Windows CIM `32.0.16.1062` 当成矛盾；它们是两种版本表示。报告中注明实际采用的来源。

### 4.3 受限 suite

为 runner 增加：

```text
-Suite Full|Recheck1080
```

- 默认必须是 `Full`，保持现有完整矩阵行为不变。
- `Recheck1080` 只执行：forced-dynamic smoke 1 次、fixed 1920×1080 三次、default dynamic 1920×1080 三次。
- `Recheck1080` 不执行 720p/900p、fixed scale、fixture、capture/diff 或长测；summary 必须写 `suite="Recheck1080"`。
- 即使 suite 已不含长测，正式命令仍要带 `-SkipLongRun -SkipCaptures`，形成双重保护。
- 不接受自由文本 case filter，避免任意挑 case 后仍产生看似正式的 summary。

`Full` 和 `Recheck1080` 共用同一组 per-run/case validator，不能复制一套较弱的判断。

### 4.4 H1 检查和提交

执行：

```powershell
.\scripts\stage8_acceptance.ps1 -SelfTest
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
git diff --check
```

用 PowerShell AST parse 检查所有修改/新增 `.ps1`。H1 单独提交：

```text
fix(stage8-validation): compute medians and environment reliably
```

## 5. 工作包 H2：可复现 Debug Validation 证据

新增 `scripts/stage8_debug_validation.ps1`，只负责证据采集，不修改或自动操作窗口。参数至少包括：

```text
-Exe <Debug ray_tracing_demo.exe>
-OutputRoot <默认 output/stage8g>
-Seconds <固定允许 1..5，默认 1>
```

脚本必须：

1. 创建唯一 run-id 目录，保存环境快照和当前 commit。
2. 对每条命令保存完整 args、stdout、stderr、exit code、elapsed seconds 和最后一行 JSON parse 结果。
3. 验证 benchmark stdout 恰好一行 JSON、exit 0、GPU 名称存在、有效样本大于 0。
4. 验证 stderr 包含最终 `D3D12 Debug InfoQueue：0 条消息`，且不包含 Device Removed、panic、resource state、AS/readback/descriptor/fence 错误关键字。
5. capture case 验证 PNG 存在、320×180、8 SPP、view/mode 正确、bytes>0，并记录 SHA-256。
6. 最终写一份 `summary.json`；任一 case 失败则脚本非零退出，但仍保留所有已生成证据。

执行与上一轮相同的五条短验证：

1. Cornell fixed：baseline command + baseline À-Trous + baseline AS；
2. Cornell forced dynamic：optimized command + baseline À-Trous + baseline AS；
3. static `NonIndexedMultiNode.gltf` + optimized AS；
4. animated `NonIndexedMultiNode.gltf` + optimized AS；
5. glTF optimized AS、`object-material-id`、320×180、8 SPP capture。

每个 benchmark 的 `--benchmark-seconds` 仍为 1；Debug GPU Validation 初始化墙钟可能约 90–100 秒，这不允许扩大 benchmark 时长。不得发送 F1/F2、resize、最小化或 hot-reload 输入。

H2 脚本可与 H1 同一代码提交，也可单独提交：

```text
test(stage8): record reproducible debug validation evidence
```

## 6. 工作包 H3：1080p 同轮配对性能归因

### 6.1 为什么不能直接优化

修正后的 fixed 1080p 中位数为 `7.67 ms`，只比 `7.6632 ms` 高 `0.0068 ms`；同时同轮 default dynamic 的正确中位数是 `7.48 ms`。GPU timestamp 以 0.01 ms 粒度报告，现有失败太接近量化边界，不能据此改 shader 或 renderer。必须先用同机同轮的 reference/candidate 配对测试判断是否存在真实代码回退。

### 6.2 reference 构建

- reference commit 固定为 `2031abf`（阶段 8E-2 review 修复完成）。
- candidate 固定为包含 H1/H2 修复的当前 HEAD；H1/H2 只改 PowerShell，不应改变 GPU 路径。
- 不得切换或重置主工作树。
- 使用 `git archive 2031abf` 导出到唯一的 `output/stage8g/<run-id>/reference-src/`，在该副本中用独立 target 目录构建 Release。
- archive、reference 源码和 target 都在已忽略 output 目录，不提交、不覆盖既有目录，也不在本包删除其他 run。

若 reference 无法离线构建，保存真实错误并停止 H3；不得改 reference 源码让它编译。

### 6.3 配对记录脚本

新增 `scripts/stage8_paired_benchmark.ps1`，参数至少包括：

```text
-CandidateExe
-ReferenceExe
-OutputRoot
-Runs 3
-Seconds 30
```

固定 workload：Cornell、1920×1080、fixed scale 1.0、baseline À-Trous、optimized command recording、baseline AS。脚本按下列交错顺序运行，减小温度和后台漂移偏差：

```text
candidate-1, reference-1, reference-2, candidate-2, candidate-3, reference-3
```

要求：

- 两侧每次都保留 args/stdout/stderr/exit/elapsed/raw JSON。
- 校验 output/render 都为 1920×1080、resolution=fixed、valid samples>0、p95 有限且不小于 p50、Total p95<=16.67 ms。
- 使用修复后的正确 median 分别计算 candidate/reference 三次 Total p95 中位数。
- 报告 `candidate_vs_reference = (candidate_median / reference_median - 1) * 100%`。
- 保存同一次环境快照；运行期间若断电、output resize 或进程异常，整组标 invalid，不得只补一侧。
- 不自动重跑，不自动删除，不根据中途结果改变顺序。

### 6.4 同时执行 corrected runner 复核

另执行：

```powershell
.\scripts\stage8_acceptance.ps1 `
  -Configuration Release `
  -Suite Recheck1080 `
  -Runs 3 `
  -Seconds 30 `
  -SkipLongRun `
  -SkipCaptures
```

该命令不重跑 54/23，也不运行任何长测。保留 runner 的历史绝对门槛结果和 H3 的同轮 paired regression 结果，两者不得互相覆盖。

### 6.5 H3 决策表

| 条件 | Luna 必须记录的状态 | 后续动作 |
|---|---|---|
| candidate median `<=7.6632 ms` | `HISTORICAL GATE PASS` | 仍等待其他阶段债务，不标阶段完成 |
| candidate `>7.6632`，但 paired regression `<=3%` | `ABSOLUTE GATE FAIL / NO PAIRED REGRESSION` | 保持失败与环境方差证据，交 Codex决定是否修订验收定义 |
| paired regression `>3%` | `REAL REGRESSION` | 停止，不改渲染代码；交 Codex安排 commit 归因/修复 |
| 任一侧 invalid | `INCONCLUSIVE` | 保存证据并停止，不挑剩余 run |

Luna 无权自行把第二种情况改成 PASS，也无权自行进入 commit bisect 或性能优化。

## 7. 工作包 H4：文档修正

更新：

- `docs/阶段8G总体验收记录.md`
- `docs/阶段8G修复后短矩阵复验执行方案.md`
- `README.md`
- `docs/实时DXR渲染器实施方案.md`
- `docs/阶段8性能优化执行计划.md`

必须：

1. 把旧 runner median bug 和正确重算值写清楚，不改 raw 三次值。
2. 旧 run 的 fixed 1080p 状态仍是 `7.67 > 7.6632` 的 FAIL；default dynamic median 修正为 7.48 ms。
3. 只有 H2 新 summary 能把“五条 Debug Validation”恢复为可复核 PASS；否则保持未验证。
4. 记录 corrected `Recheck1080` 和 paired A/B 的 run-id、原始三次值、正确 median、百分比和决策表状态。
5. 环境数据注明来源，不能把 CIM/NVIDIA 两种 driver 表示混为一列。
6. 54/23 证据继续引用原 run，不重复生成或声称再次执行。
7. 1800/600 秒、真实 F1/F2/resize/最小化/恢复/hot-reload、PBR/大模型和 PIX UI 继续列为未执行/blocked。
8. 阶段 8 仍不得标记完成。

文档提交：

```text
docs(stage8): record corrected validation and paired attribution
```

## 8. 最终检查与交付

提交前执行：

```powershell
.\scripts\stage8_acceptance.ps1 -SelfTest
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
git diff --check
git status --short
```

对两个新增脚本和 acceptance runner 做 PowerShell AST parse。只 stage 本任务文件，保留用户无关修改。

最终回复必须列出：

- 每个 commit hash/message；
- corrected runner、Debug summary 和 paired summary 的 run-id；
- 每条实际命令、exit code、墙钟时间；
- raw 三次值、正确 median、absolute gate、paired regression；
- 54/23 没有重跑、1800/600 没有执行；
- 真实 F1/F2/GUI、PBR/大模型、PIX 仍未完成；
- 工作树状态。

完成后交回 Codex review。不要自行开始下一阶段。
