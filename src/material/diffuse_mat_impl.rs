use std::f64::consts::PI;

use rand::{prelude::ThreadRng, Rng};

use crate::{
    data::ScatterInfo,
    entity::Ray,
    some_math::{Color, Matrix3, Vector3},
    systems::transform::{rotate_around_y, rotate_vec_given_normal},
};

use super::{DiffuseMat, Material};

impl DiffuseMat {
    pub fn new(color: Color) -> Self {
        DiffuseMat {
            diffuse_color: color,
        }
    }
}

impl Material for DiffuseMat {
    fn scatter(&self, ray_in: &Ray, hit_normal: &Vector3, rng: &mut ThreadRng) -> ScatterInfo {
        // impl cosine-weighted sampling
        let a: f64 = rng.gen_range(0.0..1.0);
        let b: f64 = rng.gen_range(0.0..1.0);
        let sin_theta = a.sqrt();
        let cos_theta = (1.0 - a).sqrt();
        let sin_phi = (2.0 * PI * b).sin();
        let cos_phi = (2.0 * PI * b).cos();
        let pdf = cos_theta / PI;
        let x = sin_theta * cos_phi;
        let z = sin_theta * sin_phi;
        let y = cos_theta;

        // let scatter_dir = generate_unit_vec_hemisphere(hit_normal, rng);
        // let pdf = 1.0 / (4.0 * PI);

        let temp_dir = Vector3::new([x, y, z]);
        let scatter_dir = rotate_vec_given_normal(&temp_dir, hit_normal);
        return ScatterInfo {
            scatter_dir,
            color: self.diffuse_color,
            pdf,
        };
    }

    fn emit(&self) -> Color {
        Color::BLACK
    }

    fn get_color(&self) -> Color {
        self.diffuse_color
    }

    fn is_light(&self) -> bool {
        false
    }

    fn get_fresnel(&self, ray_in_dir: &Vector3, hit_normal: &Vector3) -> f64 {
        let f0 = 0.4;
        return f0 + (1.0 - f0) * ((*ray_in_dir) * (*hit_normal)).abs().powi(5);
    }
}

fn generate_unit_vec_hemisphere(hit_normal: &Vector3, rng: &mut ThreadRng) -> Vector3 {
    let theta: f64 = rng.gen_range(0.0..180.0f64).to_radians();
    let beta: f64 = rng.gen_range(0.0..360.0f64).to_radians();
    let trans_z = Matrix3::new([
        Vector3::new([theta.cos(), theta.sin(), 0.0]),
        Vector3::new([-theta.sin(), theta.cos(), 0.0]),
        Vector3::new([0.0, 0.0, 1.0]),
    ]);
    let temp_vec = rotate_around_y(trans_z * Vector3::new([0.0, 1.0, 0.0]), beta);
    return (temp_vec + *hit_normal).normalize();
}
