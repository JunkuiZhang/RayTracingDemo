# 阶段 11：以 RTXPT 为蓝本的路径空间重构执行方案

> 当前状态（2026-08-13）：P0–P4 与 P5 的代码切断已经实现，NRD/RR 可通过
> `--path-space-mode stable-planes` 使用真正的三稳定平面消费者；旧专用补丁仅保留在
> `legacy` 回退中。三组自动测试与 RTX 4060 Laptop 上的 NRD/RR 短截图已经通过。
> 默认仍为 `legacy`，因为独立 GPU 标记/统计、720p/1080p 固定 ROI、动态生命周期和嵌套介质
> 夹具尚未完成最终验收。证据与剩余准入项见
> [`阶段11RTXPT路径空间重构验收记录.md`](阶段11RTXPT路径空间重构验收记录.md)。

## 1. 目标与边界

本工作包把当前“相机主表面 + 镜面/玻璃专用补丁”改造成统一的路径空间重建管线。
目标不是复制 RTXPT/Donut，也不是把 RTXPT 作为第三方库嵌入 Rust 渲染器，而是采用它已经
验证过的核心契约：

- 在理想反射、理想折射等 delta 事件处分解路径；
- 每像素最多保留 3 个可独立重投影的稳定平面；
- 先建立稳定平面及其 guide，再填充各平面的 noisy radiance；
- NRD 对每个稳定平面独立降噪，并从深层向主层合成；
- DLSS Ray Reconstruction（RR）接收所有稳定平面的合并 radiance，以及由这些平面混合出的
  单套可信 guide；
- 用带优先级的显式介质列表处理闭合玻璃和嵌套介质，不再依赖 front-face 猜测“空气/玻璃”；
- `stage11_rr_primary_visibility.hlsl` 和 `stage11_rr_boundary_resolve.hlsl` 不再承担路径空间着色，
  但保留为 RR 统一的输出空间边界契约：前者生成无 jitter 的输出分辨率可见性，后者只在真实轮廓
  与虚拟镜面/玻璃表面使用有界历史，非边界像素严格直通。2026-09-10 的 Preset F 人工复验确认，
  stable-plane radiance 正确并不能替代这个最终的显示边界稳定步骤。

这不是 Frame Generation 工作包。11G 的 proxy swap chain、FG 输入和统计保持独立，不能与
路径空间重构混在同一提交中。

## 2. 参考基线与本项目取舍

实现参考固定到本地只读快照 `output/rtxpt-reference` 的提交
`f08d1c739071e0faad0c7c274d861124c511abab`。该目录被 `.gitignore` 排除，只用于人工对照，
不参与构建或分发。

上游依据：

- [NVIDIA RTXPT](https://github.com/NVIDIA-RTX/RTXPT)：路径空间分层、guide 生成、NRD 多层和
  嵌套介质的参考实现；
- [NVIDIA NRD](https://github.com/NVIDIA-RTX/NRD)：REBLUR/RELAX 输入输出契约；
- [Rendering Perfect Reflections and Refractions in Path-Traced Games](https://developer.nvidia.com/blog/rendering-perfect-reflections-and-refractions-in-path-traced-games/)：
  delta 路径分解的背景与动机。

本项目保留 Rust + 原生 D3D12/DXR、现有 Streamline C ABI 和现有场景系统，不引入 Donut、
Falcor 或 RTXPT 的运行时依赖。实现应独立编写，只对照数据契约和算法行为。

### 2.1 已确认的 RTXPT 行为

RTXPT 不是简单维护 `PathLayer[3]` 数组。它使用：

1. stable-plane build pass：沿相机路径探索 delta 分支，分配稳定平面并保存可恢复的路径状态；
2. stable-plane fill pass：从保存状态继续追踪，把 noisy diffuse/specular radiance 写入对应平面；
3. denoiser preparation：把稳定平面转换成 NRD 或 RR 所需 guide；
4. final merge：NRD 逐层处理并反向合成，RR 则一次提交合并后的输入。

RTXPT 当前的 RR 路径没有把原始路径追踪玻璃写进 `TransparencyLayer`。它将各稳定平面的
radiance 合并为 RR 主输入，并根据平面可用性、吞吐量和主导平面混合 normal、roughness、
diffuse/specular albedo。`TransparencyLayer` 因此不应被当作“替应用降噪原始玻璃”的接口。

## 3. 运行时数据模型

### 3.1 稳定平面上限

固定 `STABLE_PLANE_COUNT = 3`：

- plane 0：相机路径的主稳定表面；
- plane 1/2：在最重要的理想反射或理想折射分支后找到的稳定表面；
- 未分配平面使用无效 branch ID，不能以零深度或零法线伪装成有效表面。

3 个平面是质量、显存和调度成本之间的明确边界。超过上限的高阶 delta 路径继续累积到最近
的合法平面，但不得覆盖其 guide。Cornell Box 的主反射和主透射可落到独立平面；更复杂场景
可通过诊断视图观察溢出，而不是静默产生错误历史。

### 3.2 稳定 branch ID

branch ID 从相机根 `1` 开始，每经过一个 delta 顶点追加 2 bit lobe ID：

```text
new_id = (old_id << 2) | lobe_id
```

`lobe_id` 范围为 0–3，delta 顶点最多 15 层。该 ID 同时表达路径祖先关系；不能用每帧分配的
队列序号替代，否则 reflection/refraction 分支交换时会污染历史。

预留值：

- `0xffffffff`：平面无效；
- `0xfffffffe`：仅 build pass 内部使用的待探索状态；
- `0`：仅 build pass 内部使用的刚启动状态。

### 3.3 StablePlaneRecord

每个有效平面至少保存：

- 恢复追踪所需的 ray origin/direction、累计 path length、顶点序号；
- 稳定 branch ID 和主导平面标记；
- throughput；
- 当前/前帧虚拟表面的 motion；
- world normal、roughness；
- diffuse/specular BSDF estimate；
- noisy diffuse/specular radiance 或其独立纹理地址；
- 介质列表、随机维度和继续追踪所需的有界状态。

第一版优先采用可读、显式对齐的 16-byte 字段组；只有在 RenderDoc/PIX 和短测确认正确后才做
FP16/八面体法线压缩。必须同时提供 Rust `size_of/offset` 测试和 HLSL 常量测试，禁止凭经验
猜 ABI。

### 3.4 显存预算

所有稳定平面资源按内部渲染分辨率分配，而不是输出分辨率。RTX 4060 Laptop 的验收上限：

- 1280×720 时 stable-plane 新增资源目标不超过 256 MiB；
- 1920×1080 时目标不超过 576 MiB；
- resize/dynamic-resolution 必须随 generation 重建并在 fence 后释放；
- inactive 的 SVGF/NRD/RR 专属资源应尽量不同时常驻。

若显式布局超过预算，应先压缩 record 或复用逐平面临时 NRD 输入，不能把上限改大来通过测试。

## 4. 嵌套介质与玻璃材质

### 4.1 材质参数

介质材质增加：

- `ior`：介质内部 IOR；
- `nested_priority`：0–15，0 在资产接口中表示最高优先级；
- `thin_surface`：薄片只计算一次界面事件，不进入介质列表；
- `absorption_coefficient`：Beer–Lambert 吸收系数，单位与场景 world unit 对齐，默认零；
- 当前 `base_color_factor` 不再在每个玻璃界面重复相乘。实体玻璃颜色由距离吸收产生，界面只
  负责 Fresnel/BSDF 能量。

glTF 没有相应扩展时使用保守默认值：普通不透明材质无介质状态；legacy dielectric 为闭合
实体、IOR 1.5、priority 1、零吸收。导入器不得把所有双面材质自动当作薄玻璃。

### 4.2 两槽 priority interior list

路径状态携带两个介质槽。每个槽用高 4 bit 保存 priority、低 28 bit 保存 material ID，并按
priority 从高到低排序。资产 priority 0 映射到内部 15；空槽编码为 0。

- 命中 priority 低于当前栈顶的嵌套表面时，该交点视为 false intersection，继续光线；
- 真正透射进入时插入材质，退出时按 material ID 删除；
- 入射 IOR 来自当前栈顶，出射 IOR 来自更新后的栈顶或空气 1.0；
- 两槽溢出、退出不存在的材质或拒绝命中过多都写诊断计数，并保守终止该分支，不能使用错误
  IOR 继续追踪；
- `thin_surface` 不修改列表。

两槽覆盖当前玻璃箱以及常见“玻璃 + 内部液体”演示。增加到四槽必须是单独的性能/ABI提交。

## 5. 两遍路径追踪

### 5.1 Pass A：BuildStablePlanes

每像素从 branch root 1 开始：

1. 追踪至首个非 delta 稳定表面、环境或 delta 顶点；
2. 在理想反射/折射处枚举有效 delta lobes，按确定规则生成 branch ID；
3. 主路径继续最主要的有效 lobe，其余分支写入有界探索队列；
4. 找到稳定表面时写 record、guide 基础值和该分支的恢复状态；
5. 继续探索直至 3 个平面已满、队列为空或达到深度/拒绝命中上限。

不能按瞬时 radiance 对 delta lobes 排序；权重相近处的排序交换会制造空间接缝。分配顺序应由
稳定 lobe 类别和 branch ID 决定，throughput 只用于主导平面选择及低能量裁剪。裁剪阈值必须
有滞回或固定下限，避免逐帧开关。

### 5.2 Pass B：FillStablePlanes

对 build pass 标记的平面恢复路径状态并继续普通路径追踪：

- NEE/MIS、Russian roulette 和现有 Owen-Sobol 序列继续复用；
- stable branch ID 参与样本维度散列，保证不同分支去相关且跨帧身份稳定；
- 环境和无法绑定到表面的稳定 emission 写入 `stable_radiance`；
- 每个平面分别累计 noisy diffuse/specular radiance；
- 任何 NaN/Inf、负 radiance 或非法 hit distance 在写 UAV 前钳制并增加诊断计数。

Pass A 只建立身份和 guide，不把其单次 radiance 当成最终样本；Pass B 才是可增加 SPP 的估计器。

## 6. NRD 适配

NRD 采用逐平面 REBLUR：

1. 对 plane 2、1、0 分别准备 viewZ、normal/roughness、motion、diffuse/specular radiance-hitT；
2. 无效平面写 NRD sky marker，不能沿用上一帧内容；
3. radiance 先按对应 BSDF estimate 解调并做有限亮度钳制；
4. 每个平面拥有独立 NRD history/instance；
5. 输出按 plane 2 → 1 → 0 反向调制、乘 throughput 并合成，最后加 `stable_radiance`。

禁止继续维持“primary + transmission”写死的两套资源名。资源和提交循环必须按
`STABLE_PLANE_COUNT` 索引，以免第三层成为没有 history 的特殊分支。

## 7. DLSS Ray Reconstruction 适配

RR 一帧只 evaluate 一次：

- noisy HDR = `stable_radiance + sum(valid_plane.noisy_radiance)`；
- 各平面权重由有效性、平均 throughput 和主导平面偏置构成，并归一化；
- normal、roughness、diffuse/specular albedo 使用同一组权重混合；
- depth/motion 使用主导平面的虚拟表面契约；
- specular motion 仅在命中距离可信且表面足够光滑时使用，否则回退主 motion；
- brightness clamp、pre-exposure 和 reset token 仍遵守现有 Streamline 生命周期；
- `TransparencyLayer` 保持空，不提交原始路径追踪玻璃。

混合权重必须是可调常量并有诊断视图。第一版使用固定、跨帧确定的权重，不引入基于单帧 noisy
radiance 的主层选择。

## 8. 迁移开关与回退

新增 `--path-space-mode legacy|stable-planes`：

- 开发期间默认 `legacy`，每个中间提交都必须能编译和运行；
- P5 画质/生命周期验收通过后，NRD/RR 默认切换为 `stable-planes`；
- SVGF 保留 legacy 路径，作为无专有 SDK 的基础回退；
- 选择 `stable-planes` 但缺少所需 backend/resource 时必须报明确错误，不能静默退回并在 JSON 中
  声称 active；
- 最终仍保留显式 `legacy` 对照至少一个阶段，随后再单独决定是否删除。

## 9. 分段提交

### P0：方案与固定参考

- 本文档；
- 记录 RTXPT 对照提交、非目标、显存和验收边界。

建议提交：`docs(stage11): plan RTXPT-style path-space migration`

### P1：公共契约与介质基础

- Rust/HLSL branch ID、稳定平面常量、两槽 interior list；
- 材质 priority/thin/absorption ABI 与默认值；
- ABI、排序、进入/退出、溢出和祖先关系单元测试；
- 不改变当前渲染输出。

建议拆成：

- `feat(path-space): define stable branch and medium contracts`
- `feat(material): add nested dielectric parameters`

### P2：stable-plane generation 与诊断

- 分辨率相关资源、状态转换和清理；
- BuildStablePlanes / FillStablePlanes 两个独立 GPU pass；
- branch ID、plane index、throughput、virtual depth/motion、overflow debug view；
- `legacy`/`stable-planes` CLI 和 benchmark JSON 字段。

建议至少拆成资源、build pass、fill pass 三个提交。

### P3：NRD 三平面

- 将写死的 primary/transmission 改为三平面数组；
- 三个独立 NRD history；
- 反向合成与 stable radiance；
- 保留 legacy NRD 对照。

建议拆成输入/资源与 submit/compose 两个提交。

### P4：RR 稳定平面输入

- 合并 noisy HDR；
- 平面权重和统一 guide；
- 主导 virtual depth/motion 与保守 specular motion；
- RR debug views 和 JSON 诊断。

建议拆成 guide preparation 与 Streamline/tag/lifecycle 两个提交。

### P5：删除专用补丁并切换默认

- 删除稳定平面已替代的玻璃 deterministic dual-branch 发布逻辑；
- 删除 post-RR glass transmission visibility/boundary 特判；
- 镜面虚拟平面仅在 legacy 对照路径保留；
- 验收通过后，NRD/RR 默认改为 stable planes；
- 更新 README、总实施方案和最终验收记录。

当前只完成了“稳定消费者不再执行旧补丁”的代码切断；默认切换要等第 10 节的最终验收通过，
不能因为短 smoke capture 成功而提前执行。

删除必须独立提交，便于回滚和 A/B 对照。

## 10. 验收标准

### 10.1 自动测试

- `cargo fmt --check`；
- `cargo test --all-targets`；
- `cargo test --all-targets --features nrd`；
- SDK 已配置时再运行 `--features streamline-rr`，未配置必须明确记为 PENDING；
- shader 编译、D3D12 Debug Layer 和 InfoQueue 无新增错误；
- resize、最小化/恢复、NRD/RR/feature-off 切换不泄漏、不使用旧 generation。

### 10.2 RTX 4060 Laptop 有界短测

不运行 600/1800 秒长测。本工作包每轮只使用：

- 1–3 秒 benchmark smoke test；
- 64/128 SPP 固定 capture；
- 必要时最多 15 秒真实窗口生命周期观察；
- 1280×720 与 DLSS Quality 为主要迭代档；
- 最终再补一次 1920×1080 短测，不能用它替代内部资源显存统计。

### 10.3 画质门槛

- 静止相机下，灯边缘、红/绿墙接缝、左侧镜面与地板交界、右侧玻璃顶面/内部/接触区应在
  history 收敛后稳定；
- 玻璃不再依赖 post-RR 屏幕空间采样生成颜色；
- 主反射、主透射拥有不同 branch ID 和不同 guide，不串 history；
- 相机运动、reset、resize 后不保留错误虚拟表面；
- 与 legacy 相比不能出现全屏亮度漂移、白边、黑洞、NaN 或明显能量重复；
- NRD、RR 和 feature-off 都有固定截图与 ROI 差分，人工动态观察仍单独记录，不能由静态指标
  冒充 PASS。

### 10.4 性能门槛

- DLSS Quality + RR/NRD 的 GPU Total p95 目标仍为 16.67 ms；
- BuildStablePlanes、FillStablePlanes、每层 denoise/merge 必须有独立 GPU 标记；
- benchmark JSON 报告 active plane 平均数、plane overflow、interior overflow、false-intersection
  rejection、stable-plane 显存和各 pass GPU 时间；
- 若 3 平面导致 p95 超标，先优化 inactive-plane 调度和资源压缩，不允许降低正确性或恢复
  guide 不匹配的混层。

## 11. Review 阻断项

出现以下任一项不得进入下一工作包：

1. 把 branch ID 替换成逐帧队列序号；
2. build/fill 使用不同 jitter、不同 camera constants 或不同材质身份；
3. 一个平面的 radiance 使用另一个平面的 depth/normal/motion；
4. NRD 多层共享同一 history instance；
5. RR 同一帧 evaluate 多次，或把原始玻璃误送 `TransparencyLayer`；
6. 介质退出仍硬编码回空气 1.0，忽略外层介质；
7. `base_color_factor` 在玻璃每个界面重复相乘造成厚度无关的变暗；
8. 平面/介质溢出静默覆盖有效数据；
9. 中间提交默认改变画质但没有 CLI 对照和 reset；
10. 将 Frame Generation、ReSTIR DI/GI 或材质系统大改夹带进本工作包。

## 12. 完成定义

只有 P0–P5 全部完成、短测和人工动态观察通过，才可声称“真正路径追踪玻璃已经采用
RTXPT-style stable planes”。仅完成 P1/P2 只能称为基础设施或实验路径，不能删除现有稳定
回退，也不能宣称阶段 11 已整体完成。Frame Generation 仍按 11G 独立推进。
