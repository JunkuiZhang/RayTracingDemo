use crate::{
    data::GBInfo,
    some_math::{Color, Point, Vector3},
};

pub fn pixel_filter(gb0: &GBInfo, gb1: &GBInfo, c0: Color, c1: Color, sigma: f64) -> f64 {
    if gb0.hit_obj_id != gb1.hit_obj_id {
        return 0.0;
    } else {
        return normal_filter(2.0, gb0.normal, gb1.normal)
            * (depth_filter(
                0.5,
                gb0.distance,
                gb1.distance,
                gb0.hit_point,
                gb1.hit_point,
                gb0.normal,
            ) + luminance_filter(2.0, c0, c1, sigma))
            .exp();
    }
}

#[inline]
fn depth_filter(weight: f64, d0: f64, d1: f64, p0: Point, p1: Point, n0: Vector3) -> f64 {
    let res = -weight * (d0 - d1).abs() / (1.0 - ((p0 - p1).normalize() * n0).abs() + 1e-5);
    // println!("depth filter: {}", res);
    return res;
}

// fn distance_filter(weight: f64, p0: Point, p1: Point)

#[inline]
fn normal_filter(weight: f64, n0: Vector3, n1: Vector3) -> f64 {
    let res = (n0 * n1).max(0.0).powf(weight);
    // println!("normal: {}", res);
    return res;
}

#[inline]
fn luminance_filter(weight: f64, c0: Color, c1: Color, sigma: f64) -> f64 {
    let res = -weight * (c0 - c1).length() / sigma;
    // println!("luminance {}", res);
    return res;
}
