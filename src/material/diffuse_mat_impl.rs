use crate::some_math::Color;

use super::{DiffuseMat, Material};

impl DiffuseMat {
    pub fn new(color: Color) -> Self {
        DiffuseMat {
            diffuse_color: color,
        }
    }
}

impl Material for DiffuseMat {
    fn scatter(&self) {
        // TODO:
    }

    fn emit(&self) {}
}
