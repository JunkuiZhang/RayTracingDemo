use crate::some_math::{Matrix3, Vector3};

pub fn rotate_around_y(vector: Vector3, angle: f64) -> Vector3 {
    // angle in radians
    // look at y-minus, anti-clockwise
    let trans_matrix = Matrix3::new([
        Vector3::new([angle.cos(), 0.0, -angle.cos()]),
        Vector3::new([0.0, 1.0, 0.0]),
        Vector3::new([angle.sin(), 1.0, angle.cos()]),
    ]);
    return trans_matrix * vector;
}
