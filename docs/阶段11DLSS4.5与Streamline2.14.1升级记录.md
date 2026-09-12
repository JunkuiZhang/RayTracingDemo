# 阶段 11：DLSS 4.5 与 Streamline 2.14.1 升级记录

日期：2026-09-10

目标硬件：NVIDIA GeForce RTX 4060 Laptop GPU

实现提交：

- `25bf5ae build(stage11): upgrade Streamline to 2.14.1`
- `debb138 feat(stage11): adopt DLSS 4.5 RR preset F`
- `1bd4994 fix(streamline): scale depth separation for compact scenes`
- `586d246 fix(path-tracing): keep area-light emission one-sided`
- `bd93000 fix(scene): restore visible Cornell light surface`
- `7481df0 fix(stage11): stabilize stable-plane RR boundaries`
- `86be567 fix(scene): integrate area light into ceiling`
- `81cca50 fix(stage11): mix stable-plane RR guides`
- `98132c1 test(scene): generalize Cornell geometry checks`
- `2389229 fix(scene): unify ceiling reconstruction identity`
- `c522df1 fix(path-tracing): preserve delta-plane sample budget`

结论：项目已从 Streamline 2.12.0 整体升级到 2.14.1，并显式选择本版本新增且作为默认值的
DLSS Ray Reconstruction `Preset F`。目标机日志确认实际加载 Streamline 2.14.1、
`nvngx_dlssd.dll` 310.9.1，并创建和执行了 853×480 到 1280×720 的 RR context；这不是只替换
DLL 或只改版本字符串。自动短矩阵和 Release 构建均通过，升级后的聚焦 FG 2x 证据也已取得。
阶段 11 仍不标记完成：11G-E 生命周期/FrameView 验收、RR 人工画质记录和已知 NGX shutdown
卡住问题仍需分别收口。

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
| 第三轮修复后 `cargo test --all-targets --features streamline-rr --locked` | PASS：195 + 6 |
| 修复后 `cargo build --release --features streamline-rr,streamline-fg --locked` | PASS |

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

首次 Preset F 人工复验为 **FAIL**：静止/移动画面仍有边界抖动，并且面积灯左侧顶面出现错误的
分区亮度。相同现象在 FG off/on 下都存在，因此不能归因于 Frame Generation。

排查确认了两个应先修正的输入根因：

- bridge 此前没有暴露 `sl::Constants::minRelativeLinearDepthObjectSeparation`，因而继承 SDK 的
  `40.0` 默认值。本项目 near plane 为 `0.001`，该默认值会把约 4 cm 内的深度层视作未充分分离；
  Cornell 灯和顶面只相隔约 3.3 mm。ABI v9 现在显式提交 `1.0`，保留约 1 mm 的线性深度余量。
- NEE 把 Cornell 面积灯作为朝下的单面发光体，但 BSDF 命中路径曾用 face-forward normal 和
  `abs(dot(...))` 接受背面发光。现在 NEE 与 BSDF-hit MIS 都使用固定的室内朝向 `-Y` 发射半球；
  薄灯卡仍保持 `double_sided`，因为该材质位只负责 DXR 命中可见性，不能同时表达发射方向。

修复后首次人工复验又暴露了两个独立问题：把薄灯卡改为单面会受当前 DXR face classification
影响而显示成黑色；stable-plane RR 则直接把 RR output 送入 ToneMap，绕过了输出分辨率边界历史。
前者已通过拆分“可见双面”和“发光单面”修复；后者已把 `RrPrimaryVisibility` 与
`RrBoundaryResolve` 提升为所有 RR producer 共用的最终边界契约。该 resolve 只在半径 2 内的真实
轮廓或虚拟镜面/玻璃表面使用有界历史，其他像素精确直通，不是全屏时域模糊。

第二轮修复后的人工反馈确认普通边界已经稳定，但玻璃箱顶面仍有水波纹，面积灯左侧顶棚仍有错误
亮度。第三轮排查确认它们是两个独立根因：

- Cornell 面积灯原本是悬在完整顶棚下约 3.3 mm 的薄片，形成了非物理的窄遮挡腔。现在顶棚拆成
  四个互不重叠的面片，灯面与精确开口共面，使 NEE、BSDF 命中和实际可见几何使用同一个发光域；
- Stable Plane RR 原本合并全部平面的 noisy radiance，却只提交主导平面的 normal、roughness 和
  albedo。玻璃顶面的反射与透射因此使用了不匹配的单层 guide。现在按本地 RTXPT 参考使用稳定的
  throughput 权重、有效层均衡项和主导层偏置，使用同一组归一化权重混合 normal、roughness、
  diffuse/specular albedo；depth 和 motion 仍只取主导平面，避免破坏已稳定的轮廓契约。

第三轮代码的自动门禁已通过，动态画质仍需在目标机人工确认。重点检查玻璃顶面在相机停止后是否
稳定，以及灯左侧顶棚是否不再出现矩形暗区；若二者通过，同时还应确认上一轮已经稳定的外轮廓没有
回退。

第三轮人工复验表明上述两项仍可见。第四轮使用相同 Release/RR 命令分别抓取 final、normal 和
object/material-ID，进一步定位并修正：

- normal guide 在顶棚连续，但 ID 图确认四个开口面片被创建为四个独立 instance/stable surface，
  人工分片边界泄漏到重建结果。现在四个带孔面片属于同一个 indexed mesh、同一个 stable ID；
  修复后的 RTX 4060 Laptop capture 中，灯左侧矩形区域及其斜向亮度缝已经消失；
- Stable Plane fill 会把本地 depth 重新置零，同时把非 root 的镜面/玻璃分支误标为普通路径，导致
  已有的额外直射光采样策略在最需要的第一稳定表面失效。现在由稳定 branch ID 恢复 delta 路径类别，
  第一稳定表面使用有界 8 个灯光样本，后续 specular/transmission bounce 保留 4 个，普通全屏主表面
  仍为 1 个。相邻 `capture-after-spp=31/32` 的玻璃顶面 ROI MAE 从 `0.2703` 降至 `0.2509`，
  RGB 差值大于 2 的像素由 7 降至 5；这是方差下降证据，不替代连续动态肉眼验收。

上述修复后的 Preset F 仍需在静止和连续移动中人工复验，重点观察：

- 面积灯边缘、Cornell 三面接缝和细小遮挡边界；
- 理想镜面、玻璃内部像、物体与地板接触区；
- 相机停止后是否稳定收敛，运动中是否出现拖影、抽动或水波纹；
- 与升级前固定机位 capture 的曝光、细节和残余噪声是否合理。

自动单元测试不能替代这项画质判断。`stable-planes` 当前仍有意不启用旧的 post-RR boundary
history pass；若上述输入修复后仍有残余抖动，应先用 depth/normal/motion debug view 证明 guide
不连续，再决定是否实现 stable-aware 输出边界方案，不能直接用全屏时域滤波遮盖错误输入。

### 5.3 Frame Generation 重新验收

11G-C 的真实 inputs/options/lifecycle 已实现，依赖升级后的窗口聚焦 2x 真机门禁也已通过：日志取得
`status=0`、`actual_presented=2`、`max_generated=1`、`focused=1`、warmup 4，并输出
`frame_generation_confirmed`。窗口失焦时的 `actual_presented=1` 仍只表示驱动暂停，不能代替该证据。

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

聚焦 FG 的 `numFramesActuallyPresented>=2` 门禁已经通过，11G-D 也已建立 application/base FPS、
display FPS、generated/dropped frame 和 Reflex application-frame latency 的统一实时/JSON 统计。
下一代码包进入 11G-E：resize、最小化/恢复、F5 开关往返、Debug Layer、FrameView pacing 和短稳定性
验收；在这些完成前不进入阶段 12。
