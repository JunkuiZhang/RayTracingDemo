# 阶段 11 RTXPT 稳定平面准生产验收记录

## 结论

阶段 11 的 NRD 与 RR stable-plane 路径已经在 RTX 4060 Laptop 上通过有界 Smoke、修复后的
Quality 自动项和 1920×1080 准生产 Gate。两条 Gate 进程均自行以 exit code 0 退出，未 timeout，
并形成真实 child GPU timings、异步 counter readback、显存预算与 `gpu_idle_wait_count == 0` 证据。
首次 Smoke 的 `E_INVALIDARG`/timeout、Quality 的 nested 黑块和首次 RR Gate shutdown timeout 仍在
下文保留，未被覆盖。

requested/active 分离的 `auto` 默认切换也已经完成：SVGF 默认解析为 `legacy`，NRD/RR 默认解析
为 `stable-planes`，显式覆盖继续有效。stable-plane 准生产工作包已完成；但尚不能宣称阶段 11
整体完成，因为 Frame Generation 尚未开始，Lifecycle 人工动态画质、GPU Validation 和 PIX UI
capture 仍为 `PENDING_MANUAL`。

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
| `ce6f741` | 在稳定平面队列饱和时保留高能透射 continuation，修复 nested 介质黑块。 |
| `235f6b4` | 增加 nested transmission retention 的自动验收。 |
| `7e67fb6` | 让 acceptance runner 的 process record、timeout 和退出证据确定化。 |
| `9e255b1` | 保留无可分叉平面时的反射 continuation，并完成修复后 Quality 自动验收。 |
| `1691ac3` | 按 Streamline 2.12 生命周期要求，让升级后的 DXGI 代理交换链存活到 `slShutdown()` 完成。 |
| `c5b688f` | 分离 requested/active 类型，默认解析 NRD/RR 到 stable planes，并让 F3 单 generation 事务提交。 |
| `629a6ee` | runner 分别验证 requested/active，增加默认、显式回退和 SVGF 诊断矩阵。 |
| `8494fe9` | 修正显式 stable + SVGF 诊断 pass 的 profiler active mask。 |
| `9ae3745` | 锁定 F3 只有一次 generation 创建、一次 history reset 且成功后才提交 active。 |

未实现：Frame Generation、ReSTIR。透明物体已由 stable-plane 路径空间处理，本轮没有另建
TransparencyLayer 后处理管线。

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

## Quality 首轮证据与阻塞项（历史）

Quality run-id 为 `20260819-152615Z-1c60f33`，原始汇总：

`output/stage11-stable-planes/20260819-152615Z-1c60f33/summary.json`

10 个 1280×720 capture 均在约 28 秒的总时间内完成；六组全图及每组七个 ROI 的
`image_diff` 均实际执行成功，自动汇总正确保持 `PENDING_MANUAL`。相邻 SPP 全图数据如下：

| pair | changed RGB | RGB diff > 2 | max | mean max-channel | RMSE |
|---|---:|---:|---:|---:|---:|
| NRD stable 127→128 | 47,223 | 1,346 | 248 | 0.1169 | 2.1280 |
| RR stable 127→128 | 185,063 | 5,012 | 65 | 0.2705 | 1.2097 |
| legacy 127→128 | 43,293 | 1,706 | 216 | 0.1263 | 2.2662 |

Cornell 的 127/128 静态图没有可见的大范围跳变；NRD/legacy 的高 max 主要集中在灯边缘等
少量高动态范围像素，RR 则是范围更广但幅度较低的变化。由于尚未执行真实连续窗口观察，这些
指标只保留为 characterization，不建立回退阈值。

本轮不能接受 Quality：nested NRD 和 nested RR 的内层液体都呈近乎纯黑。补充的 1 秒 nested
benchmark 没有介质硬错误，但暴露出大量分支拒绝：

| consumer | 内部尺寸 | frames | queue overflow | TIR | interior overflow | invalid exit | Total p95 |
|---|---:|---:|---:|---:|---:|---:|---:|
| NRD stable | 1280×720 | 141 | 19,470,489 | 3,236,675 | 0 | 0 | 7.53 ms |
| RR stable | 853×480 | 160 | 9,819,681 | 1,632,368 | 0 | 0 | 5.42 ms |

根因位于 `stage11_stable_plane_build.hlsl` 的单调 `tail` FIFO：每次玻璃界面先入队反射、再入队
透射，已消费槽位不能复用。嵌套界面很快令 `tail == 6`，随后高能透射分支也被拒绝，造成黑色
内部。单纯增大数组或按吞吐量排序都不是修复；NVIDIA RTXPT 的稳定平面实现会把分叉存入可用
plane，并让当前路径沿固定 lobe 继续，而且源码明确说明吞吐量排序会在分支切换处产生降噪接缝。
后续已按《阶段11稳定平面透射分支保活修复方案》修复并重跑 Quality；本段只保留首次失败证据，
不再代表当前状态。

## 修复后 Quality 与 1080p Gate

修复后的 Quality 自动项 run-id 为 `20260819-162502Z-9e255b1`，自动证据完整，汇总保持
`PENDING_MANUAL`，没有把主观画质伪造成 PASS。首次 1080p Gate
`20260819-163119Z-9e255b1` 中 NRD PASS，但 RR 在退出阶段 timeout；该失败记录继续保留。

根因不是渲染或性能：`Dx12Renderer::drop` 先释放 Streamline 升级出的 DXGI 代理交换链，再调用
`slShutdown()`，违反本项目所带 Streamline 2.12 文档“在销毁 DXGI/D3D12 组件前 shutdown”的
要求。直接调用、重定向调用和 phase-marker 诊断确认 RR 阻塞发生在 `slShutdown()` 内。`1691ac3`
调整为先释放 active/retired feature resources，再 `slShutdown()`，最后释放代理交换链；没有增加
steady-state GPU wait，也没有用强杀、提前输出或泄漏规避正常销毁。

修复后正式 Gate run-id 为 `20260819-172115Z-1691ac3`：

`output/stage11-stable-planes/20260819-172115Z-1691ac3/summary.json`

命令：

```powershell
.\scripts\stage11_stable_planes_acceptance.ps1 `
  -Suite Gate `
  -NrdExe .\target\stage11-fixed-nrd\release\ray_tracing_demo.exe `
  -RrExe .\target\release\ray_tracing_demo.exe `
  -TimeoutSeconds 45
```

两个可执行文件均由干净 commit `1691ac3` 构建，summary 记录 `git.dirty=false`：

| consumer | exe SHA-256 | 输出/内部尺寸 | frames | Total p50/p95 | stable allocation | peak VRAM |
|---|---|---:|---:|---:|---:|---:|
| NRD stable | `3e71021f43678b5dc97d7a6aa4cef64d128e459c1a3f900ba189111a8c96f3f3` | 1920×1080 / 1920×1080 | 68 | 14.01 / 14.81 ms | 572,313,648 B | 2,470,621,184 B |
| RR stable + DLSS Quality | `f731365251f2f55cfdc2cdee3eff073c0fa9de7e93e50031cd7ea69e9327dce5` | 1920×1080 / 1280×720 | 94 | 9.64 / 10.21 ms | 254,361,648 B | 1,201,954,816 B |

两者 `Total p95 <= 16.67 ms`、`gpu_idle_wait_count == 0`、exit code 0、无 timeout；
`branch_queue_overflow_events`、`interior_overflow_events`、`invalid_medium_exit_events` 均为 0。
`plane_overflow_pixels` 分别为 119,943 和 73,655，属于稳定平面容量 characterization，不是介质
栈或分支队列硬错误。完整 Smoke `20260819-172103Z-1691ac3` 也在相同干净构建上 PASS。

## Auto 默认切换与最终 Gate

`c5b688f` 将用户请求建模为 `PathSpaceMode::{Auto, Legacy, StablePlanes}`，将 generation 实际
状态建模为不含 Auto 的 `ActivePathSpace`。唯一纯解析器固定采用以下策略：

| requested | SVGF | NRD | RR |
|---|---|---|---|
| auto | legacy | stable-planes | stable-planes |
| legacy | legacy | legacy | legacy |
| stable-planes | stable-planes | stable-planes | stable-planes |

资源分配、dispatch、counter 与 consumer 只读取 active；benchmark/capture schema 升级到 2，分别
报告 requested/active。F3 先构造目标 Streamline viewport 和唯一的新 generation，成功后才同时
提交 denoiser 与 active，随后只请求一次 history reset；创建失败不会推进 viewport ID 或改变旧
renderer 状态。显式 stable + SVGF 保持 `diagnostic-only`，仍真实记录 Build/Fill timings。

扩展 Smoke 首轮 `20260819-173456Z-629a6ee` 正确拦截了 SVGF diagnostic pass active-mask 漏报，
overall 为 FAIL；该记录没有删除。修复后的干净 runtime commit `8494fe9` run-id 为
`20260819-173839Z-8494fe9`：

`output/stage11-stable-planes/20260819-173839Z-8494fe9/summary.json`

8 个 case 的 capture 与 1 秒 benchmark 全部 PASS：历史显式 stable NRD/RR、默认 SVGF/NRD/RR、
显式 legacy NRD/RR、显式 stable SVGF。三个 default case 的命令中没有
`--path-space-mode`，JSON 分别得到：

| denoiser | requested | active | consumer | stable allocation |
|---|---|---|---|---:|
| SVGF | auto | legacy | legacy | 0 B |
| NRD | auto | stable-planes | nrd-stable-planes | 15,897,648 B |
| RR | auto | stable-planes | rr-stable-planes | 7,054,608 B |

显式 legacy 的 NRD/RR 均报告 requested/active 为 legacy、allocation 0；显式 stable 的 SVGF
报告 `diagnostic-only`、allocation 15,897,648 B，并具有真实 stable child timing 与 counter。

最终 1080p Gate run-id 为 `20260819-173924Z-8494fe9`：

`output/stage11-stable-planes/20260819-173924Z-8494fe9/summary.json`

| consumer | exe SHA-256 | Total p50/p95 | frames | exit/timeout | GPU idle wait |
|---|---|---:|---:|---|---:|
| NRD stable | `f4d2907ff68de66bec5d654cd8d30f159a2ae27b50a02922bb3697988d412f59` | 13.90 / 14.23 ms | 69 | 0 / false | 0 |
| RR stable + DLSS Quality | `f8c93d1ac11a1fa8e23a8cb14685fa862327a5438888eb8f9f67c12adda66be7` | 9.62 / 9.86 ms | 101 | 0 / false | 0 |

summary 记录 RTX 4060 Laptop、`git.dirty=false`；两个 p95 均低于 16.67 ms，正式进程均自行退出。

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
| `cargo test --all-targets --locked` | PASS，172 tests + image_diff 6 tests |
| `cargo test --all-targets --features nrd --locked` | PASS，173 tests + image_diff 6 tests |
| `cargo test --all-targets --features streamline-rr --locked` | PASS，182 tests + image_diff 6 tests |
| `cargo test --all-targets --features nrd,streamline-rr --locked` | PASS，183 tests + image_diff 6 tests |
| `cargo build --locked` | PASS |
| `cargo build --release --locked` | PASS |
| 隔离 `cargo build --release --features nrd --locked` | PASS |
| 隔离 `cargo build --release --features streamline-rr --locked` | PASS |
| `.\scripts\stage11_stable_planes_acceptance.ps1 -SelfTest` | PASS，21 项拒绝/契约检查 |
| 最终 `-Suite Smoke` | PASS，8 个 requested/active case 的 capture 与 1 秒 benchmark 均通过 |
| 干净 `8494fe9` 构建的 `-Suite Gate` | PASS，NRD/RR 1 秒 benchmark 均自行退出，约 11 s |
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
不是 1080p 峰值准入数据。Quality 已实际完成 64/127/128 SPP 配对和 ROI `image_diff`，脚本只
验证证据完整性并把主观画质保持为 `PENDING_MANUAL`，没有再因仅写出 ROI 坐标而判 PASS；人工
review 随后因 nested 内层液体黑块将本轮标记为阻塞。

## 验收表

| 项目 | 状态 | 说明 |
|---|---|---|
| profiler child timings 类型和聚合 pass 保留 | PASS | 已有 NRD/RR 真机 p50/p95 |
| stable-plane counter CPU/HLSL 契约 | PASS | 已有正式测量区间 readback，硬错误为 0 |
| nested dielectric fixture 与 scene 互斥 | PASS（自动）/ PENDING_MANUAL（最终画质） | fixture 已隔离，透射主链保活与硬错误 counter 已通过 |
| runner SelfTest、证据目录和 bounded timeout | PASS | SelfTest 21 项；8-case Smoke PASS |
| NRD stable Smoke | PASS | capture + 1 秒 benchmark |
| RR stable Smoke | PASS | capture + 1 秒 benchmark |
| Quality 64/127/128 SPP 与全图/ROI diff | PENDING_MANUAL | nested continuation 已修复，自动证据完整；主观项不伪造 PASS |
| Lifecycle 15 秒项目 | SKIPPED / PENDING_MANUAL | 未执行；需真实窗口和人工观察 |
| 1080p Gate | PASS | 最新 NRD 14.23 ms、RR 9.86 ms p95，均无 timeout/idle wait |
| RTX 4060 Laptop GPU Validation | PENDING_MANUAL | Smoke/Gate 已通过；Debug Layer/GPU Validation 尚未执行 |
| 默认 stable/auto 切换 | PASS | 默认 3-case、显式 legacy 回退与 SVGF diagnostic 均有自动证据 |

## 待人工与外部验收

以下均没有伪造为 PASS：Lifecycle 的移动后停止、resize、最小化/恢复、shader hot reload，修复后
nested dielectric TIR 的最终人工复核，公开 PBR/大模型、PIX UI capture 和 Debug GPU Validation。
静止 Cornell 场景的灯边缘、墙缝、接地和玻璃稳定性此前已经由用户连续观察确认改善；该观察不
替代尚未执行的通用场景验收。

> 2026-09-10 后续修正：DLSS 4.5 Preset F 的人工复验确认 stable RR 仍需输出空间
> `RrPrimaryVisibility`/`RrBoundaryResolve`。它们现已从迁移期 legacy patch 改为所有 RR producer
> 共用的轮廓/虚拟表面稳定契约；本记录中的旧 pass mask 与历史数值只代表当时提交，不应继续作为
> 当前 active-pass 预期。
