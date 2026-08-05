use rand::rngs::StdRng;

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
    fn scatter(&self, _ray_in: &Ray, hit_normal: &Vector3, _rng: &mut StdRng) -> ScatterInfo {
        ScatterInfo {
            scatter_dir: *hit_normal,
            color: self.color,
            pdf: 0.0,
        }
    }

    fn emit(&self) -> Color {
        self.color
    }

    fn is_light(&self) -> bool {
        true
    }
}
