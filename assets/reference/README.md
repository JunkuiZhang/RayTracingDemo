# CPU 参考图片

本目录统一保存项目早期的实验图片，以及阶段 0 冻结的 CPU 参考结果。

## 阶段 0 基准

以下图片由固定 Cornell Box 场景、固定相机和随机种子 `0x5EED20210716CAFE` 生成：

- `00-1SPP-origin-img.png`：1 SPP 原始结果。
- `00-16SPP-origin-img.png`：16 SPP 原始结果。
- `00-500SPP-origin-img.png`：500 SPP 原始结果。

生成命令示例：

```powershell
cargo run --release -- --cpu-reference --samples 16 --skip-denoise --output-dir assets\reference
```

## 历史图片

其余 PNG 是项目早期保存的降噪过程和高采样结果。原来的 `00-1SPP-origin-img.png` 为避免覆盖，已改名为 `历史-00-1SPP-origin-img.png`。
