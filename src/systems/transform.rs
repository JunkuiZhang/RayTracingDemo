use crate::some_math::{Matrix3, Vector3};

pub fn rotate_around_y(vector: Vector3, angle: f64) -> Vector3 {
    // angle in radians
    // look at y-minus, anti-clockwise
    let trans_matrix = Matrix3::new([
        Vector3::new([angle.cos(), 0.0, -angle.sin()]),
        Vector3::new([0.0, 1.0, 0.0]),
        Vector3::new([angle.sin(), 0.0, angle.cos()]),
    ]);
    // println!("=============================");
    // println!("{:?} * {:?} = {:?}", trans_matrix, vector, trans_matrix * vector);
    return trans_matrix * vector;
}

pub fn rotate_vec_given_normal(vec: &Vector3, normal: &Vector3) -> Vector3 {
    let (data_z, data_y) = get_rotate_angles(normal);
    return reverse_rotate_y_given_sincos(data_y)
        * (reverse_rotate_z_given_sincos(data_z) * (*vec));
}

fn reverse_rotate_z_given_sincos(data: (f64, f64)) -> Matrix3 {
    // data: orgin sin cos
    // look at z_minus, anti_clockwise
    let (sin, cos) = data;
    return Matrix3::new([
        Vector3::new([cos, -sin, 0.0]),
        Vector3::new([-sin, cos, 0.0]),
        Vector3::new([0.0, 0.0, 1.0]),
    ]);
}

fn reverse_rotate_y_given_sincos(data: (f64, f64)) -> Matrix3 {
    // data: orgin sin cos
    // look at y_minus, anti_clockwise
    let (sin, cos) = data;
    return Matrix3::new([
        Vector3::new([cos, 0.0, sin]),
        Vector3::new([0.0, 1.0, 0.0]),
        Vector3::new([-sin, 0.0, cos]),
    ]);
}

fn get_rotate_angles(target_vec: &Vector3) -> ((f64, f64), (f64, f64)) {
    // theta to y-plus
    // phi to x-plus
    let cos_theta = *target_vec * Vector3::new([0.0, 1.0, 0.0]);
    let sin_theta = (1.0 - cos_theta.powi(2)).sqrt();
    let sin_phi;
    let xz_vec = Vector3::new([target_vec.x(), 0.0, target_vec.z()]).normalize();

    let cos_phi = xz_vec * Vector3::new([1.0, 0.0, 0.0]);
    let temp = (1.0 - cos_phi.powi(2)).sqrt();
    if target_vec.z() > 0.0 {
        sin_phi = temp;
    } else {
        sin_phi = -temp;
    }
    return ((sin_theta, cos_theta), (sin_phi, cos_phi));
}
