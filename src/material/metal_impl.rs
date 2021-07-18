use rand::prelude::ThreadRng;

use crate::{
    data::ScatterInfo,
    entity::Ray,
    some_math::{reflect, Color, Vector3},
};

use super::{Material, Metal};

impl Metal {
    pub fn new(color: Color) -> Self {
        Metal { color }
    }
}

impl Material for Metal {
    fn scatter(&self, ray_in: &Ray, hit_normal: &Vector3, rng: &mut ThreadRng) -> ScatterInfo {
        let dir = reflect(&ray_in.direction, hit_normal);
        return ScatterInfo {
            scatter_dir: dir,
            color: self.color,
            pdf: 0.0,
        };
    }

    fn emit(&self) -> Option<Color> {
        None
    }

    fn naive_render(&self) -> Color {
        self.color
    }

    fn is_light(&self) -> bool {
        false
    }
}
