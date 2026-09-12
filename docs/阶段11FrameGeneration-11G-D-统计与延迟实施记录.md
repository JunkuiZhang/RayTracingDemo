# 阶段 11：Frame Generation 11G-D 统计与延迟实施记录

日期：2026-09-12

目标硬件：NVIDIA GeForce RTX 4060 Laptop GPU

状态：IMPLEMENTED；自动短门禁通过，下一步进入 11G-E 有界验收

## 1. 前置门禁

11G-C 与 HDR10 交换链已经在前台聚焦窗口取得真实生成帧证据：

```text
frame_generation_state status=0 actual_presented=2 max_generated=1 vsync_supported=1 sync_interval=0 focused=1 warmup=4 requested=on state=on-proxy
frame_generation_confirmed status=0 num_frames_actually_presented=2 vsync_supported=1 sync_interval=0
```

这满足 `status == 0` 且 `numFramesActuallyPresented >= 2` 的硬门槛。`actual_presented=2` 来自
`slDLSSGGetState`，表示上次查询以来实际呈现了一个 application frame 和一个 generated frame；不能用
options-on、proxy-loaded 或窗口观感代替该数值。

## 2. Present 统计契约

`PresentationCounters` 是唯一把 application Present 与 DLSS-G 实际呈现结果合并的统计层。每次成功
application `Present` 后先查询一次 `slDLSSGGetState`，然后恰好记一次：

```text
application_frames += 1
displayed_frames += numFramesActuallyPresented
generated_frames += max(numFramesActuallyPresented - 1, 0)
dropped_generated_frames += max(requested_generated_frames + 1 - numFramesActuallyPresented, 0)
```

没有编译 FG、proxy 未加载或 FG 未请求时，一次成功 Present 记为 application/displayed 各一帧。
generated frame 不取得 frame token，也不推进相机、动画、路径追踪、SPP、重建历史、frame index 或 GPU
profiler；Total GPU 时间继续表示 application frame 的渲染成本。

统计使用单调累计计数和窗口 baseline，避免用某一帧的 2x 状态乘以滚动 FPS。标题每约 500 ms 显示：

```text
Base 120 / Display 232 / FG 1.93x
```

其中 Base 和 Display 分别由同一个真实时间窗口计算，FG 倍率为
`displayed_frames / application_frames`。最小化、capture 等没有成功 Present 的 redraw 不会虚增 Base。

## 3. Benchmark JSON v3

benchmark schema 升为 v3，并增加稳定的 `frame_generation` 对象：

```json
{
  "compiled": true,
  "supported": true,
  "requested": "on",
  "active": "on",
  "lifecycle": "on-proxy",
  "support_result_raw": 0,
  "status_raw": 0,
  "requested_generated_frames": 1,
  "max_generated_frames": 1,
  "application_frames": 0,
  "displayed_frames": 0,
  "generated_frames": 0,
  "dropped_generated_frames": 0,
  "actual_presented_multiplier": 0.0,
  "base_fps": 0.0,
  "display_fps": 0.0,
  "estimated_vram_bytes": 0,
  "vsync_supported": true,
  "dynamic_mfg_supported": false,
  "reason": null
}
```

计数只覆盖 benchmark 正式 measurement interval，不包含 120 个 GPU 样本的预热。feature-off 构建仍
输出相同字段；`compiled=false`，不可取得的 SDK 字段为 `null`，并且不会加载 FG DLL。失败状态通过
raw status、lifecycle 和 `reason` 显式表达，不把请求态冒充为实际 active。

## 4. Reflex 延迟语义

Streamline bridge ABI 升为 v10。桥接层从 SDK 的 64 项 `ReflexReport` ring 中选择 frame ID 最大且
`presentEndTime != 0` 的完整 application-frame report，仅复制固定宽度字段，不把 SDK 数组布局暴露给
Rust ABI。

标题显示 SDK 原始 `simulationStart -> presentEnd`，例如 `Latency S→P 5.20 ms`。JSON 还可报告：

- input sample → present end；
- simulation start → present end；
- render submit start → present end；
- SDK 的 GPU active render 和 GPU frame duration。

时间戳按 SDK 定义的微秒换算，只接受非零且单调的区间。report 或单项数据不可用时为 `N/A`/`null`；
绝不根据 FPS 估算延迟。这些是 application-frame latency，不是扫描输出或显示器端到端延迟；11G-E 仍需
用 FrameView/Reflex 外部工具检查 display pacing 和外部延迟证据。benchmark 的 query failure 计数也以
正式 measurement baseline 为准，不混入预热阶段。

## 5. Capture 来源

capture schema 升为 v3，并明确记录：

```json
"capture_source": "application_display_output"
```

同时保存 FG requested/active 状态。当前截图读取的是应用渲染的 `display_output`，不是 Streamline proxy
生成的中间帧，因此可用于画质回归，但不能作为“FG 已实际插帧”的证据。

## 6. 自动验证

本包只执行构建、单元测试和静态差异检查，没有运行 30/600/1800 秒长测：

| 验证 | 结果 |
| --- | --- |
| `cargo test --all-targets --features streamline-rr,streamline-fg --locked` | PASS：主程序 204 + image_diff 6 |
| `cargo test --all-targets --locked` | PASS：主程序 182 + image_diff 6 |
| `cargo build --release --features streamline-rr,streamline-fg --locked` | PASS |
| `git diff --check` | PASS |

新增测试覆盖混合 base/generated/dropped 计数、零时长窗口、Present → state query → counter 的顺序、
Reflex 单调时间戳/单位、benchmark schema v3 和 capture provenance。仓库级
`cargo fmt --all -- --check` 仍会发现本包前已有的全局格式差异；未做全仓机械格式化，以免混入无关改动。

## 7. 下一步：11G-E

11G-E 应新增有界 FG runner，并分开验证：

1. feature-off、FG compiled-off、DLSS Quality + FG、RR Quality + FG、animated 和低分辨率 Debug；
2. steady-state application/display/generated/dropped 关系、status/support、marker/token/presentCommon 一致性；
3. F5 连续开关、resize、最小化/恢复和退出的显式生命周期边界；
4. Debug Layer/DRED 无新增错误，稳态 `gpu_idle_wait_count == 0`；
5. FrameView `MsBetweenDisplayChange` 的外部 pacing 证据。

每个自动 case 默认 1 秒、外层 timeout 不超过 60 秒。NVIDIA NGX telemetry shutdown hang 继续与运行期
FG 成功分开记录；不得跳过 `slShutdown()` 或伪造 clean exit。11G-E 通过后才判断阶段 11 是否完成。
