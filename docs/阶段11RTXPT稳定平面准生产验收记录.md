# 阶段 11 RTXPT 稳定平面准生产验收记录

## 结论

修复后，阶段 11 的 NRD 与 RR stable-plane 路径已在 RTX 4060 Laptop 上通过有界 Smoke：
两条路径均生成 PNG 和单行 benchmark JSON，真实 child GPU timings、异步 counter readback、
显存预算与 `gpu_idle_wait_count == 0` 均形成证据。首次 Smoke 的 `E_INVALIDARG`/timeout 记录仍在
下文保留，未被覆盖。

当前仍不是“准生产通过”：Quality、Lifecycle、1080p Gate、GPU Validation 和人工动态画质尚未
执行，默认仍是 `legacy`。所以可以继续进入下一项验收，但在这些项目完成前不能切换默认路径。

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
| `5352b72` | 补齐首次证据的最终 metadata。 |
| `e4a8981` | 把 stable build UAV 表修正为 6 项并移动后续 feature 表，消除 `u5` 越界/描述符重叠。 |
| `64cbb14` | 令直方图按最终平面数每像素只记一次，并隔离 benchmark 正式区间与旧世代 counter。 |
| `ae3981c` | 移除 nested fixture 中遗留玻璃，确保内外介质闭合、相互包含且与金属盒分离。 |
| `5ec0c97` | runner 区分 legacy/stable 契约，并真正执行全图/ROI image diff，Quality 不再假阳性 PASS。 |

未实现、未切换：`--path-space-mode auto` 默认切换、Frame Generation、ReSTIR、透明层重构。

## 修复后短时 Smoke 证据

Runner commit 为 `5ec0c97`，run-id 为 `20260819-121437Z-5ec0c97`。原始汇总：

`output/stage11-stable-planes/20260819-121437Z-5ec0c97/summary.json`

命令：

```powershell
.\scripts\stage11_stable_planes_acceptance.ps1 `
  -Suite Smoke `
  -NrdExe .\target\stage11-fixed-nrd\release\ray_tracing_demo.exe `
  -RrExe .\target\release\ray_tracing_demo.exe `
  -TimeoutSeconds 30
```

| case | capture | benchmark | 关键契约 |
|---|---|---|---|
| Cornell stable NRD，输出/内部 320×180 | PASS，8 SPP PNG | PASS，238 个完成帧，Total p95 0.86 ms | consumer/pass active mask 正确；GPU idle wait 0 |
| Cornell stable RR，输出 320×180、DLSS 内部 213×120 | PASS，8 SPP PNG | PASS，238 个完成帧，Total p95 0.73 ms | RR fused pass 正确；GPU idle wait 0 |

两条路径的 `plane_count_histogram` 之和分别等于正式区间 `pixels_traced`；`last.pixels_traced`
分别等于各自内部渲染像素数。`plane_overflow_pixels`、`interior_overflow_events` 和
`invalid_medium_exit_events` 均为 0。`branch_queue_overflow_events` 非零，按执行方案属于首轮必须
报告但不预设为零的 characterization 项，不能隐去，也暂不作为 Smoke 失败条件。

## 首次 Smoke 证据（历史，修复前）

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
当时没有重跑 Smoke；后来的修复后 run 使用新 run-id 单独留存。

## 环境与构建

Runner 的 WMI GPU 查询因访问被拒绝而记录 `gpu_name=N/A`；独立的本机 `nvidia-smi` 采集为：

```text
NVIDIA GeForce RTX 4060 Laptop GPU, 610.62
```

Runner environment 证据仍保留：驱动来源 `nvidia-smi`、驱动 `610.62`、电源来源
`GetSystemPowerStatus.ACLineStatus=AC`、电源方案来源 `powercfg /getactivescheme`（平衡）。

首次 run 的隔离 Release 构建：

- NRD：`target/stage11-nrd/release/ray_tracing_demo.exe`
- Streamline-RR：`target/stage11-rr/release/ray_tracing_demo.exe`
- image_diff：`target/release/image_diff.exe`

Smoke summary 中记录了三个 exe 的 SHA-256。它们是本机生成物，未加入 Git。

修复后 run 的 NRD executable 位于
`target/stage11-fixed-nrd/release/ray_tracing_demo.exe`，RR 与 `image_diff` 位于
`target/release/`；新 summary 独立记录对应 SHA-256。这些仍是本机生成物，未加入 Git。

## 自动化检查

| 命令 | 结果 |
|---|---|
| `cargo test --all-targets --locked` | PASS，160 tests + image_diff 6 tests |
| `cargo test --all-targets --features nrd --locked` | PASS，161 tests + image_diff 6 tests |
| `cargo test --all-targets --features streamline-rr --locked` | PASS，170 tests + image_diff 6 tests |
| `cargo test --all-targets --features nrd,streamline-rr --locked` | PASS，171 tests + image_diff 6 tests |
| `cargo build --locked` | PASS |
| `cargo build --release --locked` | PASS |
| 隔离 `cargo build --release --features nrd --locked` | PASS |
| 隔离 `cargo build --release --features streamline-rr --locked` | PASS |
| `.\scripts\stage11_stable_planes_acceptance.ps1 -SelfTest` | PASS，15 项拒绝/契约检查 |
| 修复后 `-Suite Smoke` | PASS，NRD/RR capture 与 1 秒 benchmark 均通过，总用时约 12.5 s |
| `git diff --check` | PASS |
| `cargo clippy --all-targets --all-features -- -D warnings` | FAIL，既有 `resource.rs:create_texture_2d_array` too-many-arguments 和既有 stable NRD `needless_range_loop`；本轮未用全局 allow 掩盖 |
| `cargo fmt --all -- --check` | FAIL，仓库已有历史格式差异，且此前阶段提交中的 Rust 片段仍未全局重排；本轮遵守不全局格式化约束 |

## Profiler、counter、VRAM 与画质

修复后已有 RTX 4060 Laptop 真机、小分辨率 Smoke 证据：

| consumer | 内部尺寸 | Total p50/p95 | Build p50/p95 | Fill[0/1/2] p95 | stable allocation |
|---|---:|---:|---:|---:|---:|
| NRD stable | 320×180 | 0.84 / 0.86 ms | 0.04 / 0.05 ms | 0.34 / 0.09 / 0.09 ms | 15,897,648 B |
| RR stable | 213×120 | 0.71 / 0.73 ms | 0.02 / 0.02 ms | 0.20 / 0.08 / 0.08 ms | 7,054,608 B |

local VRAM measurement 状态为 `available`，预算为 7,537,164,288 B；这是 Smoke 环境信息，
不是 1080p 峰值准入数据。Smoke PNG 已生成，但 64/127/128 SPP 配对与 ROI image diff 属于
Quality，尚未执行。修复后的 runner 会实际调用 `image_diff`，只验证证据完整性，并把主观画质
保持为 `PENDING_MANUAL`，不会再因仅写出 ROI 坐标而判 PASS。

## 验收表

| 项目 | 状态 | 说明 |
|---|---|---|
| profiler child timings 类型和聚合 pass 保留 | PASS | 已有 NRD/RR 真机 p50/p95 |
| stable-plane counter CPU/HLSL 契约 | PASS | 已有正式测量区间 readback，硬错误为 0 |
| nested dielectric fixture 与 scene 互斥 | PASS（单测） | fixture 已隔离；nested GPU Quality 尚未跑 |
| runner SelfTest、证据目录和 bounded timeout | PASS | SelfTest 15 项；修复后 Smoke PASS |
| NRD stable Smoke | PASS | capture + 1 秒 benchmark |
| RR stable Smoke | PASS | capture + 1 秒 benchmark |
| Quality 64/127/128 SPP 与全图/ROI diff | NOT RUN / PENDING_MANUAL | runner 已修复，尚未执行 |
| Lifecycle 15 秒项目 | SKIPPED / PENDING_MANUAL | 未执行；需真实窗口和人工观察 |
| 1080p Gate | SKIPPED | 未执行 |
| RTX 4060 Laptop GPU Validation | BLOCKED | Smoke 已通过；Debug Layer/GPU Validation 尚未执行 |
| 默认 stable/auto 切换 | NOT DONE | 明确留给 Codex review 和用户动态验收 |

## 待人工与外部验收

以下均没有伪造为 PASS：静止收敛、移动后停止、resize、最小化/恢复、shader hot reload、玻璃
水波纹/接缝/灯边缘主观画质、nested dielectric TIR 画面、公开 PBR/大模型、PIX UI capture、
Debug GPU Validation 和 1080p gate。
