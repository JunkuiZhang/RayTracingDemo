# 阶段 11 稳定平面准生产收口与 Auto 默认切换执行方案

> 完成状态（2026-08-20）：WP-A 与 WP-B 均已完成。Streamline 生命周期修复、干净 1080p Gate、
> requested/active 分离、默认 auto、显式回退和 8-case Smoke 均已通过；最终证据见
> [`阶段11RTXPT稳定平面准生产验收记录.md`](阶段11RTXPT稳定平面准生产验收记录.md)。本文件后续
> 仅作为实施契约保留，不应再次交给 agent 重复执行。下一步是 11G-A Frame Generation。

## 1. 目标与当前结论

本轮只完成两件事：

1. 收口 stable-plane 的 1080p 准生产 Gate，修复 RR benchmark 的退出生命周期阻塞；
2. Gate 通过后，引入 `--path-space-mode auto|legacy|stable-planes`，让 NRD/RR 默认消费
   stable planes，而 SVGF 继续走 legacy。

本轮明确不实现 Frame Generation、不删除 legacy、不引入 ReSTIR、不调整曝光/色调映射，也不再
修改已经通过画质观察的 stable-plane 着色算法。Frame Generation 的 11G-A 必须等本方案完成并经
Codex review 后再开始。

当前基线为 `9e255b1`，工作树干净。已知事实：

- 修复后的 Smoke 已通过；
- 修复后的 Quality 自动项已通过，nested transmission 不再是阻断项；
- 用户已经连续观察并确认 Cornell 场景的灯边缘、墙缝、物体接地和静止画面明显稳定；
- `20260819-163119Z-9e255b1` 的 1080p Gate 中，NRD Native PASS，RTX 4060 Laptop 上
  `Total p95 = 14.87 ms`，低于 `16.67 ms`；
- 同一 Gate 的 RR Quality 在完成初始化和最终 resize 后未退出，60 秒 timeout，stdout 没有报告，
  因而整轮 Gate 为 FAIL；这不是已经证明的性能失败，而是尚未定位的退出生命周期失败；
- 默认路径仍是 `legacy`，不得在 RR Gate 失败时提前切换。

失败证据保留在：

```text
output/stage11-stable-planes/20260819-163119Z-9e255b1/summary.json
```

## 2. 总体顺序与停止点

严格按以下顺序实施：

```text
WP-A1 有界复现与定位 RR 退出阻塞
  -> WP-A2 修复 Streamline/Renderer 生命周期
  -> WP-A3 重跑 RR 单项与完整 1080p Gate
  -> STOP：Codex review Gate 证据
  -> WP-B1 requested/active 模型与纯解析器
  -> WP-B2 接入初始化和运行时 F3 切换
  -> WP-B3 JSON、runner、测试和回退验证
  -> STOP：Codex review 默认切换
  -> 才允许进入阶段 11G-A Frame Generation
```

若 WP-A 的完整 Gate 未通过，必须停在 WP-A，不能先提交 `auto` 默认切换。

## 3. WP-A：修复 RR benchmark 退出生命周期

### 3.1 先区分“渲染没结束”和“销毁时卡住”

当前 stderr 最后两条有效记录是：

```text
resize_diagnostic state=completed output=1894x1020 render=1263x680 generation=2 ...
resize_diagnostic state=completed output=1920x1080 render=1280x720 generation=3 ...
```

`windows_app.rs` 会先生成 benchmark JSON，随后把 `renderer` 置空；`Dx12Renderer::drop` 会等待
GPU、释放 Streamline viewport/proxy swapchain，然后调用 bridge shutdown；只有 Drop 完成并退出
event loop 后，应用才向 stdout 打印保存在内存中的 JSON。因此“stdout 为空”不能直接证明 benchmark
采样没有结束，也可能是 Drop/Streamline shutdown 阻塞。

最多进行两次独立 RR 复现，每次仍只用 `--benchmark-seconds 1`，单次 timeout 不超过 45 秒。不要
连续循环几十次。仅在无法从现有代码确定边界时增加结构化 stderr phase marker，至少覆盖：

1. warm-up complete；
2. benchmark sample complete；
3. benchmark JSON materialized；
4. renderer drop begin；
5. GPU shutdown wait complete；
6. active/retired Streamline viewport free begin/end；
7. proxy swapchain release begin/end；
8. Streamline bridge shutdown begin/end；
9. renderer drop end；
10. stdout report emitted/flushed。

marker 必须是低频生命周期日志，不能每帧输出。若只是定位用，根因确认后移除；若留下，使用固定
字段名，便于 runner 保存原始 stderr。

### 3.2 生命周期修复契约

根据 phase marker 修复实际阻塞点，不允许预先硬编码猜测。最终生命周期必须满足：

- 正常 benchmark、capture、窗口关闭和错误退出都只调用一次 Streamline shutdown；
- 一次性的应用退出允许等待 GPU；steady-state、resize、F2/F3 切换仍不得引入 GPU idle wait；
- 所有 active/retired Streamline viewport 都在 bridge shutdown 前完成 feature resource 释放；
- active/retired feature resources 在 bridge shutdown 前释放；升级后的 proxy swapchain 与普通
  swapchain/device 均保持存活到 `slShutdown()` 完成，再释放 proxy 拥有的接口引用；
- 不允许 `mem::forget` 新的 DX12/Streamline 对象来绕开析构；
- 不允许 `TerminateProcess`、`process::exit`、忽略 timeout 或“stdout 已写出即 PASS”作为修复；
- shutdown 返回非 OK 必须保留状态码和 bridge error；
- 构造中途失败仍由 RAII guard 平衡已成功的 SDK 初始化；
- runner timeout 仍必须判 FAIL，并留下唯一、确定的 process record。

优先让“显式 shutdown orchestration”拥有清晰次序，避免依赖 Rust 字段的隐式逆序 Drop。若需要重构，
将一次性 shutdown 状态建模为可测试的状态机或小型 helper，不要把整个 renderer 改成大量
`Option<T>`/`ManuallyDrop<T>`。

### 3.3 WP-A 测试与准入

先执行相关单元测试和 runner SelfTest，再执行以下有界 GPU 测试：

```powershell
# 必须直接从当前 PowerShell 调用脚本，不再额外包一层 pwsh.exe -File。
.\scripts\stage11_stable_planes_acceptance.ps1 -SelfTest

# 单项诊断可直接执行 RR exe；benchmark 仍为 1 秒，外层 timeout <= 45 秒。
.\target\release\ray_tracing_demo.exe `
  --output-size 1920x1080 `
  --scene cornell `
  --path-space-mode stable-planes `
  --denoiser dlss-rr `
  --upscaler dlss-quality `
  --benchmark-seconds 1 `
  --atrous-mode baseline `
  --command-recording-mode optimized `
  --acceleration-structure-mode baseline

.\scripts\stage11_stable_planes_acceptance.ps1 `
  -Suite Gate `
  -NrdExe .\target\stage11-fixed-nrd\release\ray_tracing_demo.exe `
  -RrExe .\target\release\ray_tracing_demo.exe `
  -TimeoutSeconds 45
```

Gate 完成定义：

- 两个进程均自行以 exit code 0 退出，不 timeout；
- stdout 各自恰好一条 JSON；
- GPU 名称为 RTX 4060 Laptop；
- NRD Native 与 RR Quality 的 `Total p95 <= 16.67 ms`；
- `gpu_idle_wait_count == 0`（benchmark 正式区间）；
- stable allocation、counter schema、真实 child pass、VRAM measurement 均有效；
- `interior_overflow_events == 0`、`invalid_medium_exit_events == 0`；
- 不运行 30/600/1800 秒 benchmark。

建议提交：

```text
fix(streamline): make benchmark shutdown deterministic
docs(stage11): record repaired stable-plane production gate
```

生命周期代码与证据文档分成两个提交。证据提交必须更新
`docs/阶段11RTXPT稳定平面准生产验收记录.md`，保留旧 FAIL run，并追加新 run-id、命令、exe hash、
NRD/RR p50/p95、counter、VRAM、退出码和仍为 `PENDING_MANUAL` 的项目。

## 4. WP-B：引入 requested/active 分离的 Auto 模式

### 4.1 类型模型

不要让 `Auto` 混进每帧 shader 分支。推荐分成两个概念：

```rust
enum PathSpaceMode { Auto, Legacy, StablePlanes }       // 用户请求
enum ActivePathSpace { Legacy, StablePlanes }           // 已解析运行态
```

提供无副作用纯函数：

```rust
fn resolve_path_space(
    requested: PathSpaceMode,
    denoiser: DenoiserBackend,
) -> ActivePathSpace
```

解析矩阵固定为：

| requested | SVGF | NRD REBLUR | DLSS RR |
|---|---|---|---|
| `auto` | `legacy` | `stable-planes` | `stable-planes` |
| `legacy` | `legacy` | `legacy` | `legacy` |
| `stable-planes` | `stable-planes` | `stable-planes` | `stable-planes` |

CLI 默认值改为 `auto`，帮助文字同时说明解析规则和 `legacy` 回退开关。显式模式永远覆盖自动策略。

### 4.2 Renderer 状态与资源代际

`Dx12Renderer` 分别保存：

- `requested_path_space`：CLI 请求，运行期间保持不变；
- `active_path_space`：根据当前 denoiser 解析出的实际模式。

所有资源分配、shader dispatch、NRD/RR adapter、consumer 名称和 counter 读取只看
`active_path_space`；日志和 JSON 另行报告 requested。不得在不同函数里重复写
`requested == Auto && denoiser ...` 条件。

需要统一检查下列创建入口，不能只修初始 generation：

- 初始 renderer 创建；
- resize / render extent recreation；
- F2 upscaler 切换；
- F3 denoiser 切换；
- shader hot reload 后的 generation 行为。

F3 是本轮最关键的运行时契约：

- `auto + SVGF -> NRD/RR` 时，destination generation 必须一次性创建 stable-plane 资源；
- `auto + NRD/RR -> SVGF` 时，destination generation 不分配 stable-plane 资源；
- denoiser 与 active path-space 在同一个 generation transaction 中提交，只创建/切换一个 generation、
  只请求一次 history reset；
- 新 generation 创建失败时，旧 denoiser、旧 active path-space、旧 generation 和旧 viewport 全部保持
  可用，不能留下半切换状态；
- retired generation 按 fence 回收，切换 steady-state 不等待 GPU；
- 显式 `legacy` 或 `stable-planes` 下按 F3 时 active path-space 不变化，但 denoiser generation 仍按
  现有规则切换。

### 4.3 JSON、capture 与可观测性

benchmark JSON 保持对象名 `path_space`，字段语义改为：

```json
{
  "path_space": {
    "requested": "auto",
    "active": "stable-planes",
    "consumer": "rr-stable-planes"
  }
}
```

同时满足：

- `plane_count`、`allocated_bytes`、`counters` 由 active 决定；
- active legacy 时 counter 为 `null`、allocation 为 0；
- capture metadata 也分别保存 requested/active，不能继续用单个字符串冒充两者；
- 窗口标题至少能看出 active 路径，日志中的切换记录要包含 requested、old active、new active；
- schema 如需升级，只升级一次并同步测试/runner，不能静默改变字段含义。

### 4.4 Runner 适配

保留现有显式 `--path-space-mode stable-planes` 的历史验收 case，再增加最小自动策略 case：

- 默认参数 + SVGF：requested `auto`、active `legacy`；
- 默认 path-space + NRD：requested `auto`、active `stable-planes`；
- 默认 path-space + RR：requested `auto`、active `stable-planes`；
- 显式 legacy + NRD/RR：requested/active 都是 `legacy`；
- 显式 stable-planes + SVGF：requested/active 都是 `stable-planes`，consumer 为 diagnostic-only。

runner 不能只依据 consumer 猜 requested；必须分别验证两个字段。

### 4.5 WP-B 单元与集成测试

最低覆盖：

1. CLI 默认 `auto`，三个合法值均可解析，非法值拒绝；
2. resolver 的完整 3×3 矩阵；
3. 初始 generation 的 stable allocation 与 active 一致；
4. F3 `SVGF -> NRD/RR -> SVGF` 下 active、generation create/switch、history reset 的精确增量；
5. 显式 override 下 F3 不改变 active；
6. generation 创建失败的事务回滚；
7. benchmark/capture requested 与 active 不混淆；
8. active legacy 时不访问 stable resource/counter；
9. default feature、`nrd`、`streamline-rr`、组合 feature 均编译和测试。

最终有界命令：

```powershell
cargo test --all-targets --locked
cargo test --all-targets --features nrd --locked
cargo test --all-targets --features streamline-rr --locked
cargo test --all-targets --features nrd,streamline-rr --locked
.\scripts\stage11_stable_planes_acceptance.ps1 -SelfTest
git diff --check
git status --short
```

只在上述测试通过后跑一次 Smoke 和一次 Gate；不要跑长测。默认 auto 的 GPU case 必须省略
`--path-space-mode`，否则没有真正验证默认解析。

建议提交拆分：

```text
refactor(path-space): separate requested and active modes
feat(path-space): default reconstructed paths to stable planes
test(stage11): gate automatic path-space resolution
docs(stage11): close stable-plane production acceptance
```

不要求机械地凑满四个提交，但不得把生命周期修复、类型重构、默认行为、runner 和最终证据全部塞进
一个提交。每个提交都必须可编译，提交信息要描述真实边界。

## 5. Review 阻断项

出现任一项即退回：

1. RR timeout 被 runner 忽略、强杀后记 PASS，或报告先写出就跳过 SDK shutdown；
2. 为解决退出卡顿而在每帧、resize 或 F2/F3 引入 `wait_for_gpu`；
3. `Auto` 直接参与每帧大量分支，requested/active 没有单一解析源；
4. F3 一次操作创建两个 generation 或 reset 两次历史；
5. JSON 把 resolved value 同时写入 requested/active；
6. SVGF 默认仍分配/执行 stable planes；
7. NRD/RR 默认仍落到 legacy；
8. 显式 legacy 回退失效，或本轮删除 legacy；
9. 夹带 FG、ReSTIR、曝光、材质或 stable-plane shader 画质改动；
10. 运行 30/600/1800 秒测试，或覆盖/删除旧失败证据。

## 6. 完成定义与后续

本方案完成的唯一标准是：RR 能自行退出、完整 1080p Gate PASS、默认 `auto` 正确解析、F3 的 active
路径随 denoiser 进行单 generation 事务切换、显式回退可用、证据和工作树干净。

上述条件已完成并经 Codex review，阶段 11 stable-plane 准生产收口完成。下一段执行既有
`docs/阶段11RR收口与FrameGeneration分段执行方案.md` 的 11G-A：锁定 Frame Generation SDK、部署
清单和最小 C ABI；仍然不把 11G-A 到 11G-E 合成一个大提交。

## 7. 给 Luna 的提示词

```text
请在 C:\zjk\projects\RayTracingDemo 中严格执行
docs/阶段11稳定平面准生产收口与Auto默认切换执行方案.md。

基线从当前 HEAD 开始，先确认 git status，不覆盖任何用户改动。严格按 WP-A -> STOP -> WP-B 的
顺序：先定位并修复 1080p RR benchmark 在 renderer/Streamline 退出生命周期中的 timeout，保留
output/stage11-stable-planes/20260819-163119Z-9e255b1 的失败证据；完整 Gate 没有 PASS 前不得切
auto 默认。不能用强杀、process::exit、提前打印 JSON、忽略 timeout 或泄漏对象绕开正常 shutdown。
诊断最多两次独立 RR 复现，每次 benchmark 1 秒、外层 timeout <= 45 秒，不跑 30/600/1800 秒长测。

WP-A Gate 通过后，实现 requested PathSpaceMode 与 ActivePathSpace 分离，默认 auto 的唯一解析规则是
SVGF -> legacy、NRD/RR -> stable-planes，显式 legacy/stable-planes 永远覆盖。所有 generation 创建
入口都使用 active；F3 的 denoiser 和 active path-space 变化必须在一次 generation transaction 中完成，
只 reset 一次，失败完整回滚，steady-state 不等待 GPU。benchmark/capture JSON 分别报告 requested
与 active。

按方案补齐 resolver、CLI、generation、F3、JSON/capture、runner 测试；执行四组 cargo test、SelfTest、
一次短 Smoke 和一次短 Gate。直接从当前 PowerShell 调用 acceptance 脚本，不要额外包一层
pwsh.exe -File。不要改 stable-plane shader 画质、曝光/色调映射，不实现 FG/ReSTIR，不删除 legacy。

分成有意义的多个 commit，至少把 Streamline 生命周期修复、requested/active 重构、默认切换、测试/
证据分开；每个提交可编译。最终回复列出 commit、改动、完整命令结果、新旧 Gate run-id、JSON/PNG
证据路径、尚为 PENDING_MANUAL 的项目和 git status。不要宣称未执行的人工观察为 PASS。
```
