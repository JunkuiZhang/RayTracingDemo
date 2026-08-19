# 阶段 11：RTXPT-style 路径空间重构验收记录

## 1. 结论

截至 2026-08-13，RTXPT-style stable planes 已达到“可显式启用的功能候选”，尚未达到
“默认启用并关闭工作包”的最终状态。

已经成立的结论：

- NRD/RR 在 `stable-planes` 模式中消费三平面数据，不再执行 legacy 的镜面虚拟面、
  RR primary visibility 或 boundary resolve 补丁；
- closed dielectric 使用显式嵌套介质和路径级吸收，玻璃不是 post-RR 屏幕空间着色；
- NRD 为每个有效平面使用独立 backend/history，随后从深层向 plane 0 合成；
- RR 每帧只 evaluate 一次，消费合并 noisy HDR 与确定性的主导 guide，
  `TransparencyLayer` 保持为空；
- 旧路径完整保留，可用一个 CLI 参数进行 A/B 与回滚。

暂不成立的结论：

- `stable-planes` 还不是默认值；
- 不能仅凭 320×180 的短截图宣称所有静态/动态伪影已经解决；
- Frame Generation 尚未开始，阶段 11 整体没有完成。

## 2. 实现与提交

| 提交 | 内容 |
|---|---|
| `61bd55a` | 固定 RTXPT 参考和执行边界 |
| `8de1ec4` | 稳定 branch ID、介质列表与 CPU/HLSL 公共契约 |
| `01eb41a` | 材质嵌套优先级、thin surface 与吸收参数 |
| `31e9ac0` | 修正扩展材质 GPU stride/binding |
| `2af6878` | stable-plane Build/Fill 和诊断资源 |
| `b8746f8` | NRD 三平面独立 history 与反向合成 |
| `11a41dc` | RR 三平面合并、主导 guide 与单次 evaluate |
| `6548f5a` | stable 消费者切断 legacy 专用补丁 |

本实现对照 NVIDIA RTXPT 的数据流与行为，但不引入 RTXPT、Donut 或 Falcor 运行时依赖。
本地只读对照固定在 `output/rtxpt-reference` 的
`f08d1c739071e0faad0c7c274d861124c511abab`。

## 3. 已实现的契约

### 3.1 路径与介质

- 每像素最多 3 个 stable plane，branch ID 按 delta lobe 祖先稳定编码；
- Build pass 最多探索 6 个候选分支、16 个步骤和 8 次 false-intersection skip；
- 两槽 priority medium list 支持嵌套闭合介质，退出玻璃后恢复外层 IOR；
- Beer-Lambert 吸收按封闭介质中的实际行进距离计算；
- Native 使用确定性 Owen-Sobol；DLSS/RR 只使用一份全局 jitter 契约。

### 3.2 重建消费者

- NRD：每个平面分别准备 diffuse/specular、normal/roughness/viewZ、motion、hit distance 和
  branch ID，再以三套 REBLUR history 降噪并反向合成；
- RR：把所有平面的 HDR radiance 合并成一次输入，直接发射光单独保存，normal、depth、motion
  与 diffuse/specular albedo 来自确定性的主导平面；
- SVGF：保持 legacy 主消费者；stable-planes 与 SVGF 联用只作为 Build/Fill 诊断，不冒充
  多平面重建。

### 3.3 显存

稳定平面分辨率相关资源为每内部像素 `276 B`：三份 64 B record、三份 4 B header，以及
noisy diffuse/specular、stable radiance 和主导 albedo 数组。实际分配与公式一致：

| 内部分辨率 | 像素数 | stable-plane 分配 |
|---|---:|---:|
| 213×120（DLSS Quality） | 25,560 | 7,054,560 B |
| 320×180（Native NRD） | 57,600 | 15,897,600 B |
| 1920×1080（Native 理论值） | 2,073,600 | 572,313,600 B，约 545.8 MiB |

最后一项是按真实每像素分配计算的预算值，不是一次 1080p 实测峰值。

## 4. 自动验证

以下矩阵已通过，未运行 600/1800 秒长测：

```text
cargo test --all-targets                         155 passed
cargo test --all-targets --features nrd          156 passed
cargo test --all-targets --features streamline-rr 165 passed
```

`streamline-rr,nrd` 组合的 `cargo check` 也已通过。全仓库 `cargo fmt --check` 仍会命中任务开始前
已有的无关格式差异，因此本工作包没有用全局格式化覆盖用户代码；所有本次变更另行通过
`git diff --check`。

## 5. RTX 4060 Laptop 短证据

| 后端 | 参数摘要 | 结果 |
|---|---|---|
| DLSS RR | 320×180 输出，DLSS Quality，stable planes，32 SPP | 成功退出；consumer=`rr-stable-planes`；内部 213×120；约 4.3 s |
| NRD REBLUR | 320×180 Native，stable planes，32 SPP | 成功退出；consumer=`nrd-stable-planes`；内部 320×180；约 1.5 s |

对应截图为 `output/stage11-p5-stable-rr.png` 和
`output/stage11-p5-stable-nrd.png`。这两项只证明真实 GPU 创建、调度、barrier、SDK submit 和
capture 路径可用；低分辨率/32 SPP 不能替代正常分辨率的画质验收。

## 6. 运行与回滚

候选 NRD：

```powershell
cargo run --release --features nrd -- --output-size 1280x720 --path-space-mode stable-planes --denoiser nrd-reblur
```

候选 RR：

```powershell
cargo run --release --features streamline-rr --locked -- --output-size 1280x720 --path-space-mode stable-planes --denoiser dlss-rr --upscaler dlss-quality
```

A/B 对照时只把 `stable-planes` 改成 `legacy`。默认值在最终验收前保持 `legacy`。

## 7. 最终准入清单

具体实现顺序、JSON/counter ABI、嵌套介质夹具和有界 runner 契约见
[`阶段11RTXPT稳定平面准生产验收执行方案.md`](阶段11RTXPT稳定平面准生产验收执行方案.md)。
该方案明确禁止 Luna 在证据 review 前自行切换默认路径。

以下项目完成前不得切换默认值：

1. 给 BuildStablePlanes、每层 FillStablePlanes、NRD layer/compose 和 RR merge 增加独立 GPU
   profiler 标记；benchmark JSON 提供 active-plane 平均数、plane/interior overflow、
   false-intersection rejection 与各 pass 时间；
2. 在 1280×720 的 NRD Native、RR Quality 下分别完成 64/128 SPP 固定截图，并对灯边缘、
   红绿墙接缝、左镜面/地板交界、右玻璃顶部/内部/接触区保存固定 ROI；
3. 补一次 1920×1080 短测，确认 RTX 4060 Laptop 的显存和 GPU Total p95 不超过项目门槛；
4. 人工观察静止收敛、相机运动/停止、resize、最小化/恢复和 hot reload，确认不保留错误历史；
5. 加入“玻璃容器 + 内部液体”夹具，验证 priority medium 的进入、退出、TIR、吸收和 reset；
6. 上述项目通过后再把 NRD/RR 的默认路径切到 stable planes；legacy 至少保留一个验收周期。

这些项目属于验收与可观测性收口，不要求重新设计 stable-plane 算法。若固定 ROI 或动态观察
暴露结构性错误，则停止默认切换并以 `legacy` 作为可靠回退。
