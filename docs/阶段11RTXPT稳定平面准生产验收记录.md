# 阶段 11 RTXPT 稳定平面准生产验收记录

## 结论

本轮没有通过准生产 Smoke，阶段 11 稳定平面路径保持显式候选状态，默认路径没有切换。
由于 Smoke 失败，Quality、Lifecycle 和 Gate 均未执行；本记录不能作为稳定平面生产准入或默认切换依据。

失败原因是实际 renderer/SDK 运行阻断，不是用阈值绕过：NRD Release 进程在创建
`stable-plane build` 管线时返回 `E_INVALIDARG (0x80070057)`；RR Release 进程在单进程
30 秒上限内没有产生 JSON，runner 将其标记为 timeout。按照方案要求，本轮没有修改渲染算法来
制造通过，也没有继续执行后续 suite。

## 基线与提交

起始基线为 `fbab8e7`，工作树起始时干净。实施提交如下：

| commit | 作用 |
|---|---|
| `9de1bae` | 为 stable-plane child GPU timings 增加类型化 pass、PIX 名称、active mask 与 JSON 字段，保留聚合 pass。 |
| `6e2c9bb` | 增加线程组归约计数器、固定 Frame Context readback、generation/fence 校验和异步 telemetry。 |
| `a01590b` | 增加 `SceneAsset::nested_dielectric_fixture()`、`--scene` 解析和 `--scene`/`--model` 互斥测试。 |
| `44a56cf` | 使无有效样本的 GPU pass 序列化为 `null`，并在 reset/resize/hot reload 时清理旧世代诊断累积。 |
| `6bc0ecd` | 新增有界 `stage11_stable_planes_acceptance.ps1` runner 与 SelfTest。 |
| `8864725` | 加强 runner timeout 后的子进程刷新、精确 child kill 和退出码记录。 |
| `bb029fc` | 本验收记录。 |

未实现、未切换：`--path-space-mode auto` 默认切换、Frame Generation、ReSTIR、透明层重构。

## 实际 Smoke 证据

Runner commit 为 `6bc0ecd`，run-id 为 `20260819-114804Z-6bc0ecd`。原始汇总：

`output/stage11-stable-planes/20260819-114804Z-6bc0ecd/summary.json`

命令：

```powershell
powershell -ExecutionPolicy Bypass -File scripts/stage11_stable_planes_acceptance.ps1 `
  -Suite Smoke `
  -NrdExe target\stage11-nrd\release\ray_tracing_demo.exe `
  -RrExe target\stage11-rr\release\ray_tracing_demo.exe `
  -TimeoutSeconds 30
```

| case | 结果 | 原始证据 |
|---|---|---|
| Cornell stable NRD 320×180 capture/8 SPP | FAIL，进程约 1.689 s 后启动失败；没有 JSON/PNG | `cornell_nrd_smoke/capture/capture.stderr.txt`、`capture.exit.json` |
| Cornell stable NRD 320×180 benchmark/1 s | FAIL，未产生 JSON | `cornell_nrd_smoke/benchmark/benchmark.stderr.txt`、`benchmark.exit.json` |
| Cornell stable RR 320×180 capture/8 SPP | FAIL，30.722 s timeout | `cornell_rr_smoke/capture/capture.stderr.txt`、`capture.exit.json` |
| Cornell stable RR 320×180 benchmark/1 s | FAIL，30.053 s timeout | `cornell_rr_smoke/benchmark/benchmark.stderr.txt`、`benchmark.exit.json` |

NRD stderr 的关键原始信息为：

```text
实时 DX12 渲染器启动失败：创建 DX12 后端：创建 stable-plane build 管线：参数错误。 (0x80070057) (0x80070057)
```

实际文件中的 HRESULT 也是 `0x80070057`，判断以原始 stderr 和 `summary.json` 为准。RR stderr 能确认窗口、RTX 4060 Laptop adapter 和 Streamline support 已
开始初始化，但没有形成可解析 benchmark/capture JSON。

Smoke 后发现旧 runner 对某些 timeout child 的 `Process` 对象刷新不充分；`8864725` 已加强
tree kill、精确 child kill 和退出码采集。该修复是在 Smoke 失败后完成的，未伪造或覆盖原始 run，
也没有重跑 Smoke。

## 环境与构建

Runner 的 WMI GPU 查询因访问被拒绝而记录 `gpu_name=N/A`；独立的本机 `nvidia-smi` 采集为：

```text
NVIDIA GeForce RTX 4060 Laptop GPU, 610.62
```

Runner environment 证据仍保留：驱动来源 `nvidia-smi`、驱动 `610.62`、电源来源
`GetSystemPowerStatus.ACLineStatus=AC`、电源方案来源 `powercfg /getactivescheme`（平衡）。

隔离 Release 构建：

- NRD：`target/stage11-nrd/release/ray_tracing_demo.exe`
- Streamline-RR：`target/stage11-rr/release/ray_tracing_demo.exe`
- image_diff：`target/release/image_diff.exe`

Smoke summary 中记录了三个 exe 的 SHA-256。它们是本机生成物，未加入 Git。

## 自动化检查

| 命令 | 结果 |
|---|---|
| `cargo test --all-targets` | PASS，159 tests + image_diff 6 tests |
| `cargo test --all-targets --features nrd` | PASS，160 tests + image_diff 6 tests |
| `cargo test --all-targets --features streamline-rr` | PASS，169 tests + image_diff 6 tests |
| `cargo check --features nrd,streamline-rr` | PASS |
| `cargo build --locked` | PASS |
| `cargo build --release --locked` | PASS |
| 隔离 `cargo build --release --features nrd --locked` | PASS |
| 隔离 `cargo build --release --features streamline-rr --locked` | PASS |
| `powershell -ExecutionPolicy Bypass -File scripts/stage11_stable_planes_acceptance.ps1 -SelfTest` | PASS，10 项拒绝/契约检查；PowerShell 5.1 与 7 均执行过 |
| `git diff --check` | PASS |
| `cargo clippy --all-targets --all-features -- -D warnings` | FAIL，既有 `resource.rs:create_texture_2d_array` too-many-arguments 和既有 stable NRD `needless_range_loop`；本轮未用全局 allow 掩盖 |
| `cargo fmt --all -- --check` | FAIL，仓库已有历史格式差异，且此前阶段提交中的 Rust 片段仍未全局重排；本轮遵守不全局格式化约束 |

## Profiler、counter、VRAM 与画质

Smoke 在 renderer 创建/进程超时阶段失败，没有 fence-complete benchmark JSON。因此：

- stable-plane Build/Fill、NRD stable child、RR stable merge 的真实 GPU p50/p95：`BLOCKED`；
- counter schema/均值/硬错误计数：`BLOCKED`，没有可信 readback 样本；
- local VRAM usage/budget：`BLOCKED`，没有可信 benchmark JSON；
- 1080p Total p95：`NOT RUN`；
- PNG/ROI diff：`NOT GENERATED`，Smoke 没有生成有效 PNG，Quality 未执行。

代码侧的计数器 schema、线程组归约、固定 readback slice、generation/extent 丢弃规则和 child pass
字段已有 CPU 单元测试，但它们不能替代 RTX 4060 Laptop 真机 JSON 证据。

## 验收表

| 项目 | 状态 | 说明 |
|---|---|---|
| profiler child timings 类型和聚合 pass 保留 | PASS（静态/单测） | 尚无 GPU p50/p95 |
| stable-plane counter CPU/HLSL 契约 | PASS（单测/静态） | 尚无真实 readback |
| nested dielectric fixture 与 scene 互斥 | PASS（159+ tests 覆盖） | 未进入 GPU Smoke |
| runner SelfTest、证据目录和 bounded timeout | PASS（SelfTest）；Smoke 证据 FAIL | Smoke run 使用旧 runner commit；后续已加强 cleanup |
| NRD stable Smoke | FAIL | stable-plane build pipeline `E_INVALIDARG` |
| RR stable Smoke | FAIL | 30 秒内无 JSON，timeout |
| Quality 64/127/128 SPP 与 ROI | SKIPPED | Smoke 失败后按方案停止 |
| Lifecycle 15 秒项目 | SKIPPED / PENDING_MANUAL | 未执行；需真实窗口和人工观察 |
| 1080p Gate | SKIPPED | 未执行 |
| RTX 4060 Laptop GPU Validation | BLOCKED | Smoke 尚未通过 |
| 默认 stable/auto 切换 | NOT DONE | 明确留给 Codex review 和用户动态验收 |

## 待人工与外部验收

以下均没有伪造为 PASS：静止收敛、移动后停止、resize、最小化/恢复、shader hot reload、玻璃
水波纹/接缝/灯边缘主观画质、nested dielectric TIR 画面、公开 PBR/大模型、PIX UI capture、
Debug GPU Validation 和 1080p gate。
