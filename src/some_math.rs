mod matrix_impl;
mod vector_impl;

pub type Color = Vector3;
pub type Point = Vector3;

#[derive(Debug, Clone, Copy, Default)]
pub struct Vector3 {
    pub data: [f64; 3],
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Matrix3 {
    pub vectors: [Vector3; 3],
}

pub fn clamp(x: f64, min: f64, max: f64) -> f64 {
    if x < min {
        return min;
    }
    if x > max {
        return max;
    }
    return x;
}

pub fn point_in_2d(point: Point, square: [Point; 2], axis: usize) -> bool {
    for n in 0..3 {
        if n == axis {
            continue;
        }
        if !(point.data[n] > square[0].data[n] && point.data[n] < square[1].data[n]) {
            return false;
        }
    }
    return true;
}

pub fn order_numbers(x: f64, y: f64) -> (f64, f64, bool) {
    if x <= y {
        return (x, y, false);
    } else {
        return (y, x, true);
    }
}

pub fn reflect(vec: &Vector3, normal: &Vector3) -> Vector3 {
    *vec - 2.0 * ((*vec) * (*normal)) * (*normal)
}

pub fn refract(vec: &Vector3, normal: &Vector3, factor: f64) -> Vector3 {
    let cos_theta = ((-1.0) * (*vec) * (*normal)).min(1.0);
    let r_out_perp = factor * (*vec + cos_theta * (*normal));
    let r_out_para = (1.0 - r_out_perp.length_square()).abs().sqrt() * (*normal);
    return r_out_para + r_out_perp;
}
