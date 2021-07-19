use rand::prelude::ThreadRng;

use crate::{
    data::ScatterInfo,
    entity::Ray,
    some_math::{reflect, Color, Vector3},
    systems::transform::generate_unit_vec_sphere,
};

use super::{Material, Metal};

impl Metal {
    pub fn new(color: Color, fuzz: f64) -> Self {
        Metal { color, fuzz }
    }
}

impl Material for Metal {
    fn scatter(&self, ray_in: &Ray, hit_normal: &Vector3, rng: &mut ThreadRng) -> ScatterInfo {
        let dir = reflect(&ray_in.direction, hit_normal);
        let scatter_dir = dir + 0.7 * self.fuzz * generate_unit_vec_sphere(rng);
        return ScatterInfo {
            scatter_dir: scatter_dir,
            color: self.color,
            pdf: 1.0,
        };
    }

    fn emit(&self) -> Color {
        Color::BLACK
    }

    fn get_color(&self) -> Color {
        self.color
    }

    fn is_light(&self) -> bool {
        false
    }

    fn get_fresnel(&self, ray_in_dir: &Vector3, hit_normal: &Vector3) -> f64 {
        let f0 = 0.9;
        return f0 + (1.0 - f0) * (1.0 - ((*ray_in_dir) * (*hit_normal)).abs()).powi(5);
    }
}
