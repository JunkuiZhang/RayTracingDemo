# 阶段 11 HDR 显示输出实施记录

日期：2026-09-12
目标机器：NVIDIA GeForce RTX 4060 Laptop GPU，Windows HDR 主显示器

## 1. 最终结论与纠错

实时后端提供显式、可校准的 HDR 显示路径。`--hdr` 使用
`DXGI_FORMAT_R10G10B10A2_UNORM` flip-model 交换链、
`DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020` 色彩空间和 HDR10 mastering metadata；
不传参数时仍使用原来的 RGBA8 SDR 路径。

第一版使用 FP16 scRGB。该格式对普通 Win32 HDR 应用很方便，但 NVIDIA Streamline 2.14.1 的
DLSS Frame Generation 集成契约明确规定：HDR 交换链应使用 UINT10/RGB10、BT.2100/HDR10，
不支持 FP16/scRGB。RTX 4060 Laptop 上的 A/B 也精确复现了此限制：

- HDR + scRGB：FG `status=0`，但 120 次 warmup 后 `numFramesActuallyPresented` 始终为 1；
- SDR + RGBA8：同一机器和配置在第 3 次 warmup 达到 `numFramesActuallyPresented=2`；
- iFlip 在两组中均为 0，因此它不是这次失败的区分变量。

因此最终实现统一改成 RGB10 HDR10，而不是为 FG 单独维护第二套 HDR 路径。这样运行时 F5
开关 FG 不需要重建成另一种交换链格式，生命周期更直接，也不会出现“HDR 能显示但 FG 静默不插帧”。

## 2. 用户接口

```text
--hdr
--hdr-paper-white-nits <80..500>   默认 200
--hdr-peak-nits <300..10000>       默认读取当前显示器的 MaxLuminance
```

亮度参数必须与 `--hdr` 同时使用，峰值不能低于 paper white。启动 HDR 时程序会：

1. 检查交换链能否以 BT.2100/PQ Present；
2. 通过 `IDXGIOutput6::GetDesc1` 检查窗口所在输出是否处于 Windows HDR 状态；
3. 设置交换链为 HDR10 色彩空间，并提交 BT.2020/D65、峰值、MaxCLL 和 MaxFALL metadata；
4. 在 resize 和 Frame Generation proxy 交换链重建后重新提交色彩空间和 metadata；
5. 不满足条件时明确报错，不静默退回 SDR。

推荐启动命令：

```powershell
cargo build --release --features streamline-rr,streamline-fg --locked
.\target\release\ray_tracing_demo.exe --output-size 1280x720 --denoiser dlss-rr --upscaler dlss-quality --path-space-mode stable-planes --hdr
```

若显示器/驱动上报的峰值不符合实际认证值，可显式覆盖，例如：

```powershell
.\target\release\ray_tracing_demo.exe --output-size 1280x720 --denoiser dlss-rr --upscaler dlss-quality --path-space-mode stable-planes --hdr --hdr-paper-white-nits 200 --hdr-peak-nits 1000
```

## 3. 颜色管线

RR/DLSS/NRD 之前的 noisy radiance 和重建结果继续保持线性 HDR，不提前裁剪或 gamma 编码。
最终 tone map 的 HDR 分支执行以下唯一一次显示编码：

1. 把场景线性白映射到配置的 paper white；
2. paper white 以下保持线性，其上用一阶连续 shoulder 渐近到显示峰值；
3. 把线性 Rec.709 转换为线性 BT.2020；
4. 把绝对 nit 值按 SMPTE ST.2084/PQ 编码到 RGB10。

SDR 分支继续使用原 ACES 近似和 gamma，默认画面不变。工程调试视图跳过曝光和高光 shoulder，
但在 HDR 模式仍按 paper white 做 Rec.709 → BT.2020 → PQ 编码，避免把线性调试色直接写入 PQ
交换链。

Streamline Frame Generation 的 color/hudless tag 使用实际显示格式。HDR capture 则从打包的
RGB10 readback 解码 PQ、转换 BT.2020 → Rec.709，并按 paper white 生成 SDR PNG 预览；JSON
标记 `display.mode=hdr10` 和 `capture_encoding=sdr-png-preview`。该 PNG 不是 HDR 母版。

## 4. 验证结果

自动验证均通过：

- `cargo check --all-targets --features streamline-rr,streamline-fg --locked`；
- `cargo test --all-targets --locked`：186 项（image_diff 6 + 主程序 180）；
- `cargo test --all-targets --features streamline-rr,streamline-fg --locked`：207 项（6 + 201）；
- `cargo build --release --features streamline-rr,streamline-fg --locked`；
- RTX 4060 Laptop 有界启动确认：`mode=hdr10`、`R10G10B10A2_UNORM`、
  `HDR10-BT.2100-PQ`、`BitsPerColor=10`；RR 路径成功初始化；
- 同一 HDR10 启动中，FG proxy 成功创建、`fg_loaded=1`、能力查询 `status=0`。

随后在 RTX 4060 Laptop 的前台聚焦窗口完成了最终 HDR+FG 计数门禁，关键日志为：

```text
frame_generation_state status=0 actual_presented=2 max_generated=1 vsync_supported=1 sync_interval=0 focused=1 warmup=4 requested=on state=on-proxy
frame_generation_confirmed status=0 num_frames_actually_presented=2 vsync_supported=1 sync_interval=0
```

这证明当前 HDR10 交换链真实插入了一个中间帧，而不只是 proxy/feature 加载成功。复现命令为：

```powershell
$env:RAY_TRACING_STREAMLINE_LOG = '1'
.\target\release\ray_tracing_demo.exe --output-size 1280x720 --denoiser dlss-rr --upscaler dlss-quality --path-space-mode stable-planes --frame-generation on --hdr --hdr-paper-white-nits 200 --hdr-peak-nits 1000
```

通过标准仍是日志同时出现 `focused=1`、`status=0`、`numFramesActuallyPresented>=2`，而不是仅凭
`fg_loaded=1` 判定成功。本次证据已经满足该标准；`iFlip=0` 只是日志中的显示链诊断，不推翻 SDK
实际返回的两帧计数。

目标主显示器实际启动诊断为 Windows HDR active、`BitsPerColor=10`，驱动上报
`MaxLuminance=4000 nit`。这不代表屏幕峰值已被仪器验证；画质验收可先按显示器认证峰值覆盖。
目标机仍可能在 `slShutdown()` 的 NVIDIA NGX telemetry shutdown 内卡住，因此“渲染/FG 已执行”
与“进程正常退出”必须分开记录。

## 5. 已知边界与下一步

- 当前只在启动时绑定窗口所在显示器；拖到另一台 SDR/HDR 显示器后，不会自动检测并事务性重建；
- Windows HDR Calibration、显示器 OSD、驱动 EDID 和实际峰值都会影响观感；校准参数不能替代测量；
- PNG 只承担 SDR 诊断预览；真正 HDR 截图应增加线性 EXR 或带正确元数据的 HDR 图像格式；
- 11G-D 的 base/display/generated/dropped/latency 统计已经实现；下一步是 11G-E 生命周期、
  Debug Layer 与 FrameView pacing 验收。

## 6. 设计依据

- 项目内 Streamline 2.14.1 指南：
  `external/streamline-v2.14.1/docs/ProgrammingGuideDLSS_G.md` 的 HDR Swap Chain Format 一节；
- [NVIDIA：Streamline DLSS Frame Generation FAQ](https://developer.nvidia.com/rtx/streamline/get-started)；
- [Microsoft：Use DirectX with Advanced Color on high/standard dynamic range displays](https://learn.microsoft.com/en-us/windows/win32/direct3darticles/high-dynamic-range)；
- [Microsoft：IDXGISwapChain4::SetHDRMetaData](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_5/nf-dxgi1_5-idxgiswapchain4-sethdrmetadata)。
