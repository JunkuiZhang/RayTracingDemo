use std::f64::consts::PI;

use rand::{prelude::ThreadRng, Rng};

use crate::some_math::{Matrix3, Vector3};

pub fn rotate_around_y(vector: Vector3, angle: f64) -> Vector3 {
    // angle in radians
    // look at y-minus, anti-clockwise
    matrix_rotate_around_y(angle.sin(), angle.cos()) * vector
}

pub fn rotate_vec_given_normal(vec: &Vector3, normal: &Vector3) -> Vector3 {
    // let (data_z, data_y) = get_rotate_angles(normal);
    // return reverse_rotate_y_given_sincos(data_y)
    // * (reverse_rotate_z_given_sincos(data_z) * (*vec));
    let (sin_theta, cos_theta, sin_phi, cos_phi) = agnle_rotate_y_to_normal(normal);
    let trans_rotate_z = matrix_rotate_around_z(-sin_theta, cos_theta);
    let trans_rotate_y = matrix_rotate_around_y(-sin_phi, cos_phi);
    let res = trans_rotate_y * (trans_rotate_z * (*vec));
    // println!("{:?} around {:?} with ({}, {}, {}, {}) by {:?}, {:?} ==> {:?}", *vec, *normal, sin_theta, cos_theta, sin_phi, cos_phi,trans_rotate_y, trans_rotate_z, res);
    return res;
}

pub fn generate_unit_vec_sphere(rng: &mut ThreadRng) -> Vector3 {
    let theta = rng.gen_range(0.0..PI);
    let phi = rng.gen_range(0.0..(2.0 * PI));
    let trans_rotate_z = matrix_rotate_around_z(theta.sin(), theta.cos());
    let trans_rotate_y = matrix_rotate_around_y(phi.sin(), phi.cos());
    return trans_rotate_y * (trans_rotate_z * Vector3::new([0.0, 1.0, 0.0]));
}

fn agnle_rotate_y_to_normal(normal: &Vector3) -> (f64, f64, f64, f64) {
    // angle from normal to y-plus
    let cos_theta = *normal * Vector3::new([0.0, 1.0, 0.0]);
    let sin_theta = (1.0 - cos_theta * cos_theta).sqrt();
    // angle from projection of noraml to xz-plane, between x-plus
    let temp_vec = Vector3::new([normal.x(), 0.0, normal.z()]);
    if temp_vec.length_square() > 1e-3 {
        let xz = temp_vec.normalize();
        let cos_phi = xz * Vector3::new([1.0, 0.0, 0.0]);
        let mut sin_phi = (1.0 - cos_phi * cos_phi).sqrt();
        if normal.z() < 0.0 {
            sin_phi = -sin_phi;
        }
        return (sin_theta, cos_theta, sin_phi, cos_phi);
    } else {
        return (sin_theta, cos_theta, 0.0, 1.0);
    }
}

fn matrix_rotate_around_z(sin_theta: f64, cos_theta: f64) -> Matrix3 {
    // agnle in radians
    return Matrix3::new([
        Vector3::new([cos_theta, sin_theta, 0.0]),
        Vector3::new([-sin_theta, cos_theta, 0.0]),
        Vector3::new([0.0, 0.0, 1.0]),
    ]);
}

fn matrix_rotate_around_y(sin_phi: f64, cos_phi: f64) -> Matrix3 {
    // angle in radians
    return Matrix3::new([
        Vector3::new([cos_phi, 0.0, -sin_phi]),
        Vector3::new([0.0, 1.0, 0.0]),
        Vector3::new([sin_phi, 0.0, cos_phi]),
    ]);
}
