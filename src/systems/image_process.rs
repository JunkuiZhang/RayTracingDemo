use crate::{
    data::GBInfo,
    some_math::{Color, Vector3},
};

pub fn is_same_surface(gb0: &GBInfo, gb1: &GBInfo) -> bool {
    if gb0.hit_obj_id != gb1.hit_obj_id || gb0.normal * gb1.normal < 0.8 {
        return false;
    }

    // 点到中心切平面的距离可以区分前后表面，又不会排斥同一斜面上的邻居。
    let depth_scale = gb0.distance.abs().max(gb1.distance.abs()).max(1.0);
    let plane_distance = ((gb1.hit_point - gb0.hit_point) * gb0.normal).abs();
    plane_distance / depth_scale < 0.05
}

pub fn pixel_filter(gb0: &GBInfo, gb1: &GBInfo, c0: Color, c1: Color, sigma: f64) -> f64 {
    if gb0.hit_obj_id != gb1.hit_obj_id {
        return 0.0;
    }

    normal_filter(gb0.normal, gb1.normal) * depth_filter(gb0, gb1) * luminance_filter(c0, c1, sigma)
}

#[inline]
fn depth_filter(gb0: &GBInfo, gb1: &GBInfo) -> f64 {
    // 百分之一的相对深度作为衰减尺度，适配不同大小的场景。
    let depth_scale = gb0.distance.abs().max(gb1.distance.abs()).max(1.0);
    let plane_distance = ((gb1.hit_point - gb0.hit_point) * gb0.normal).abs();
    (-plane_distance / (0.01 * depth_scale)).exp()
}

#[inline]
fn normal_filter(n0: Vector3, n1: Vector3) -> f64 {
    // 点积差异为 0.1 时权重衰减到 e^-1，尖锐折角会自然被隔离。
    let cosine = (n0 * n1).clamp(0.0, 1.0);
    (-(1.0 - cosine) / 0.1).exp()
}

#[inline]
fn luminance_filter(c0: Color, c1: Color, sigma: f64) -> f64 {
    // 平坦区域的局部方差可能为零；设置下限可避免 0/0 产生 NaN 并污染整幅图像。
    let safe_sigma = sigma.max(1e-6);
    (-(luminance(c0) - luminance(c1)).abs() / safe_sigma).exp()
}

pub fn luminance(color: Color) -> f64 {
    // 在线性 RGB 空间中使用 Rec.709 亮度系数，避免把色相差误当成亮度噪声。
    0.2126 * color.x() + 0.7152 * color.y() + 0.0722 * color.z()
}
