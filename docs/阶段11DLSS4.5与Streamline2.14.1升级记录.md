# 阶段 11：DLSS 4.5 与 Streamline 2.14.1 升级记录

日期：2026-09-10

目标硬件：NVIDIA GeForce RTX 4060 Laptop GPU

实现提交：

- `25bf5ae build(stage11): upgrade Streamline to 2.14.1`
- `debb138 feat(stage11): adopt DLSS 4.5 RR preset F`

结论：项目已从 Streamline 2.12.0 整体升级到 2.14.1，并显式选择本版本新增且作为默认值的
DLSS Ray Reconstruction `Preset F`。目标机日志确认实际加载 Streamline 2.14.1、
`nvngx_dlssd.dll` 310.9.1，并创建和执行了 853×480 到 1280×720 的 RR context；这不是只替换
DLL 或只改版本字符串。自动短矩阵和 Release 构建均通过。阶段 11 仍不标记完成：升级后的聚焦
FG 2x 证据、RR 人工画质复验和已知 NGX shutdown 卡住问题仍需分别验收。

## 1. 为什么整体升级

本次不采用“只复制最新 `nvngx_dlss*.dll`”的做法。Streamline 的头文件、导入库、interposer、
功能插件和 NGX 二进制属于同一套发布物；混用版本会让 ABI、资源 tag 和生命周期行为失去可复现性。
因此下载、锁定、构建和部署统一指向官方 `v2.14.1` release asset。

锁定信息位于 `third_party/streamline/version.lock.json`：

- tag：`v2.14.1`
- tag commit：`2122257e0fce486f91b385aa63b9a09b0a34b363`
- asset：`streamline-sdk-v2.14.1.zip`
- archive SHA-256：`92c4d954631a1710da86ca3fa8d5034f2b9503838c95fc4ae977ae149319781b`
- Streamline DLL file version：`2.14.1.0`
- DLSS SR、RR、FG NGX DLL file version：`310.9.1.0`

`scripts/fetch_streamline.ps1` 会验证固定下载地址、archive SHA-256、所需文件 SHA-256、生产 DLL
Authenticode 签名和许可文件。C++ bridge 还会在编译期检查 SDK 必须为 2.14.1，并验证
`sl::DLSSDPreset::ePresetF == 6`，避免旧 include path 静默混入新二进制。

## 2. Ray Reconstruction 选择

所有 RR quality mode 现在显式请求 `Preset F`。显式固定而不是依赖隐式 default，目的是让 capture、
回归对比和后续 SDK 升级都能明确回答“使用了哪个模型”。将来改变 preset 或改回 default，必须作为
画质变更重新检查镜面、玻璃、面积灯边缘、遮挡接缝以及相机运动历史。

这次升级不改变以下核心契约：

- RR 仍直接消费 1 SPP noisy HDR、depth、motion、材质和几何 guides；
- RR 不与 SVGF/NRD 或 DLSS Super Resolution 串联；
- `stable-planes` 仍是 NRD/RR 的默认路径空间；
- RTX 4060 Laptop 仍只使用单生成帧的 2x Frame Generation，不启用 Multi Frame Generation。

## 3. 自动验证

均在当前工作区、锁定依赖下执行；未运行 600/1800 秒长测。

| 验证 | 结果 |
| --- | --- |
| `cargo test --locked --no-default-features --quiet` | PASS：176 + 6 |
| `cargo test --locked --features streamline` | PASS：186 + 6 |
| `cargo test --locked --features streamline-rr --quiet` | PASS：192 + 6 |
| `cargo test --locked --features streamline-fg --quiet` | PASS：189 + 6 |
| `cargo test --locked --features streamline-rr,streamline-fg` | PASS：195 + 6 |
| `cargo build --release --locked --features streamline-rr,streamline-fg` | PASS |
| `scripts/stage11_rr_acceptance.ps1 -SelfTest` | PASS |
| `scripts/fetch_streamline.ps1` | PASS：lock、hash、签名和许可 |

仓库级 `cargo fmt --all -- --check` 仍会报告大量本次升级前已经存在的格式差异。本次没有执行全局
自动格式化，以免把无关代码混入 SDK 升级提交；这属于单独的格式债务，不是上述测试失败。

## 4. RTX 4060 Laptop 运行证据

使用 Release、1280×720 输出、DLSS Quality、DLSS RR 和 `stable-planes` 做了有界真机 smoke。
控制台报告该 adapter 的 DLSS、Reflex、PCL、RR 和 FG support query 均成功。`sl.log` 进一步确认：

- `Streamline v2.14.1 ... host SDK v2.14.1`；
- 从 `streamline-plugins/rr-fg` 加载 `nvngx_dlssd.dll version: 310.9.1`；
- DLAA、Quality、Balanced、Performance、Ultra Performance、Ultra Quality 均选择 `Preset_F`；
- 创建 `Created DLSSDContext feature (853,480)(optimal) -> (1280,720) for viewport 3`；
- 程序完成 RR 资源释放后进入 `Shutting down NGX`。

SDK 还打印一次 tagged-resource alignment warning。D3D12 应用资源本身使用默认 alignment，SDK 在克隆
资源时识别到 concrete 65536 alignment 并按其实现改写为 0；该 warning 已由 SDK 自行处理，不表示
应用传入了错误的自定义 alignment。

## 5. 尚未通过的门禁

### 5.1 NGX 退出

真机 smoke 在 `Shutting down NGX` 后没有正常退出，因此约 60 秒时被人工中止，没有产生可作为完整
benchmark gate 的最终单行 JSON。这与既有 NVIDIA NGX telemetry shutdown 卡住现象相同；不得通过
跳过 `slShutdown()` 或谎报正常退出掩盖。渲染路径已实际创建/执行 RR，但“完整进程正常退出”仍为 FAIL。

### 5.2 RR 人工画质

需要在静止和连续移动中人工复验 Preset F，重点观察：

- 面积灯边缘、Cornell 三面接缝和细小遮挡边界；
- 理想镜面、玻璃内部像、物体与地板接触区；
- 相机停止后是否稳定收敛，运动中是否出现拖影、抽动或水波纹；
- 与升级前固定机位 capture 的曝光、细节和残余噪声是否合理。

自动单元测试不能替代这项画质判断。

### 5.3 Frame Generation 重新验收

11G-C 的真实 inputs/options/lifecycle 已实现，但依赖升级后仍需一次窗口聚焦的 2x 真机证据。只有日志
同时满足 `status=0` 且 `numFramesActuallyPresented>=2`，才能证明当前 2.14.1 组合实际生成中间帧。
窗口失焦得到 `actual_presented=1` 只证明驱动正确暂停，不能计作通过。

## 6. 复验命令

先验证并构建完整 RR + FG feature set：

```powershell
.\scripts\fetch_streamline.ps1
cargo test --locked --features streamline-rr,streamline-fg
cargo build --release --locked --features streamline-rr,streamline-fg
```

RR 人工画质复验：

```powershell
$env:RAY_TRACING_STREAMLINE_LOG = '1'
.\target\release\ray_tracing_demo.exe --output-size 1280x720 --denoiser dlss-rr --upscaler dlss-quality --path-space-mode stable-planes
```

FG 聚焦复验：

```powershell
$env:RAY_TRACING_STREAMLINE_LOG = '1'
.\target\release\ray_tracing_demo.exe --output-size 1280x720 --denoiser dlss-rr --upscaler dlss-quality --path-space-mode stable-planes --frame-generation on
```

保持窗口前台聚焦，等待 warm-up 后检查控制台/`target/release/sl.log`。退出若再次停在 NGX shutdown，
单独记录为退出问题；不要用它覆盖运行期间已经取得的 RR/FG 数值，也不要反过来把运行成功当作退出通过。

## 7. 下一步

先完成 5.2 和 5.3 两项短人工门禁。若 Preset F 画质无明显回退且聚焦 FG 证明
`numFramesActuallyPresented>=2`，阶段 11 下一代码包进入 11G-D：建立 application/base FPS、
display FPS、generated/dropped frame 和 Reflex latency 的统一实时/JSON 统计。之后再做 11G-E 的
resize、最小化/恢复、开关往返、Debug Layer 和短稳定性验收；在这些完成前不进入阶段 12。
