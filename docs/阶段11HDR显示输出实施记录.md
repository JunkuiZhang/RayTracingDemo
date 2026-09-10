# 阶段 11 HDR 显示输出实施记录

日期：2026-09-11
目标机器：NVIDIA GeForce RTX 4060 Laptop GPU，Windows HDR 主显示器

## 1. 结论

实时后端已经增加显式、可校准的 HDR 显示路径。`--hdr` 使用
`DXGI_FORMAT_R16G16B16A16_FLOAT` flip-model 交换链和
`DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709` 线性 scRGB；不传参数时仍使用原来的
RGBA8 SDR 路径。

选择 scRGB 而不是直接输出 HDR10/PQ，是因为这是 Microsoft 针对普通 Win32 HDR 应用推荐的
通用路径：应用保留线性浮点内容，桌面合成器负责映射到显示器。scRGB 的绝对亮度标尺是
`1.0 = 80 nit`。

## 2. 用户接口

```text
--hdr
--hdr-paper-white-nits <80..500>   默认 200
--hdr-peak-nits <300..10000>       默认读取当前显示器的 MaxLuminance
```

亮度调节参数必须与 `--hdr` 同时使用，峰值不能低于 paper white。启动 HDR 时程序会：

1. 检查交换链能否以 scRGB Present；
2. 通过 `IDXGIOutput6::GetDesc1` 检查窗口所在输出是否处于 Windows HDR 状态；
3. 设置交换链色彩空间；
4. 在 resize 和 Frame Generation 交换链重建后恢复该色彩空间；
5. 不满足条件时明确报错，不静默退回 SDR，以免用户误把 SDR 当成 HDR 验收。

推荐启动命令：

```powershell
cargo build --release --features streamline-rr,streamline-fg --locked
.\target\release\ray_tracing_demo.exe --output-size 1280x720 --denoiser dlss-rr --upscaler dlss-quality --path-space-mode stable-planes --hdr
```

若显示器 EDID/驱动上报的峰值不符合实际认证值，应按显示器规格显式覆盖，例如：

```powershell
.\target\release\ray_tracing_demo.exe --output-size 1280x720 --denoiser dlss-rr --upscaler dlss-quality --path-space-mode stable-planes --hdr --hdr-paper-white-nits 200 --hdr-peak-nits 1000
```

## 3. 颜色管线

RR/DLSS/NRD 之前的 noisy radiance 和重建结果继续保持线性 HDR，不提前裁剪或 gamma 编码。
Tone Map 的 HDR 分支先把场景线性白映射到配置的 paper white；paper white 以下保持线性，
其上使用一阶连续、渐近显示峰值的高光 shoulder，再按 `80 nit/scRGB unit` 写入 FP16 输出。
这使日常材质不会因开启 HDR 改变相对亮度，同时为面积灯和镜面高光保留真实高光余量。

SDR 分支继续使用原 ACES 近似和 gamma，因而不改变默认画面。调试视图仍是工程诊断用途，不作为
HDR 母版画质验收。

Streamline Frame Generation 的 color/hudless 格式现在来自实际显示格式，不再硬编码 RGBA8。
HDR capture 从 FP16 scRGB readback 生成 SDR PNG 预览，并在 JSON 中标记
`capture_encoding=sdr-png-preview`；它不是包含 HDR 元数据的母版文件。

## 4. 自动验证

以下均通过：

- `cargo test --all-targets --locked`：186 项（image_diff 6 + 主程序 180）；
- `cargo test --all-targets --features streamline-rr --locked`：204 项（6 + 198）；
- `cargo test --all-targets --features streamline-rr,streamline-fg --locked`：207 项（6 + 201）；
- `cargo check --all-targets --features streamline-rr,streamline-fg --locked`；
- Release `streamline-rr,streamline-fg` 构建；
- SDR RR 1280×720、1 秒 benchmark：正常退出，JSON 为 RGBA8/SDR；
- HDR RR 1280×720、1 秒 benchmark：正常退出，交换链为 FP16 scRGB；
- HDR RR 1280×720、8 SPP capture：至少一次正常退出并生成可查看的 SDR PNG 预览；最终
  tone-map 复验也生成了有效预览，但随后命中下述间歇性 NGX 退出卡住。

目标主显示器的实际启动诊断为：Windows HDR active、`BitsPerColor=10`，驱动
`MaxLuminance=4000 nit`。这是驱动/EDID 上报值，不代表已独立测量过屏幕峰值；画质验收可先用
显示器认证峰值覆盖。

HDR + Frame Generation 组合能建立 FP16 proxy swap chain 并持续渲染；RR/FG 有界命令结束时仍可能
命中项目升级记录中已经存在的 NVIDIA NGX telemetry shutdown 卡住问题。因此这里仅将其记录为
“HDR/FG 运行路径已进入”，不把被终止的进程冒充为完整退出验收通过。

## 5. 已知边界与下一步

- 当前在启动时绑定窗口所在显示器；把运行中的窗口拖到另一台 SDR/HDR 显示器后，不会自动重建
  色彩空间。正式多显示器支持需要监听输出拓扑/窗口位置变化并事务性重建交换链。
- Windows HDR 校准、显示器 OSD、驱动 EDID 和屏幕实际峰值会共同影响观感。引擎提供校准参数，
  但不能替代 Windows HDR Calibration 或仪器测量。
- PNG 只承担 SDR 诊断预览；若以后需要 HDR 截图，应增加线性 EXR 或带正确色彩元数据的 HDR
  图像格式，不能把当前 PNG 改名充当 HDR 文件。
- HDR 不改变阶段 11 的主线门禁：FG display/generated/dropped/latency 统计和生命周期验收仍是
  下一项工作。

## 6. 设计依据

- [Microsoft：Use DirectX with Advanced Color on high/standard dynamic range displays](https://learn.microsoft.com/en-us/windows/win32/direct3darticles/high-dynamic-range)
- [Microsoft：IDXGISwapChain3::CheckColorSpaceSupport](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_4/nf-dxgi1_4-idxgiswapchain3-checkcolorspacesupport)
- [Microsoft：IDXGIOutput6::GetDesc1](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_6/nf-dxgi1_6-idxgioutput6-getdesc1)
