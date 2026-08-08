# 阶段 7 glTF 测试夹具

这些文件是仓库内的最小、离线可重复测试夹具，不依赖网络下载。它们覆盖 loader 的三角形、非索引网格、父子节点、同 mesh 多 node、缺失法线/UV、纹理 sampler/UV 校验，以及明确错误路径。

`TextureSampler/TextureSampler.gltf`、`TexCoord1.gltf` 和 `TextureNoUv.gltf` 均为项目内生成的 review fixture：嵌入一张最小 PNG 和几何 data URI，用于验证同 image 的不同 sampler、Repeat/Clamp/Mirror、nearest/linear、非零 `texCoord` 拒绝和缺失 `TEXCOORD_0` 拒绝。

最终手工验收使用的 Khronos Sample Assets 不随仓库提交大文件，来源为：

- [Khronos glTF Sample Assets](https://github.com/KhronosGroup/glTF-Sample-Assets)
- [glTF 2.0 Registry](https://registry.khronos.org/glTF/)

`Triangle/` 夹具的 buffer 是本地测试数据，采用 CC0 风格的最小几何，不代表 Khronos 官方资产授权。提交实际 Khronos 资产时必须在此补充具体目录、原始 URL、获取日期和该资产的 license。
