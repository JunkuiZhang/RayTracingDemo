use rand::prelude::ThreadRng;

use crate::{
    data::ScatterInfo,
    entity::Ray,
    some_math::{Color, Vector3},
};

use super::{DiffuseLight, Material};

impl DiffuseLight {
    pub fn new(color: Color) -> Self {
        DiffuseLight { color }
    }
}

impl Material for DiffuseLight {
    fn scatter(&self, ray_in: &Ray, hit_normal: &Vector3, rng: &mut ThreadRng) -> ScatterInfo {
        ScatterInfo {
            scatter_dir: *hit_normal,
            color: self.color,
            pdf: 0.0,
        }
    }

    fn emit(&self) -> Color {
        self.color
    }

    fn get_color(&self) -> Color {
        self.color
    }

    fn is_light(&self) -> bool {
        true
    }

    fn get_fresnel(&self, ray_in_dir: &Vector3, hit_normal: &Vector3) -> f64 {
        1.0
    }
}
