use crate::some_math::Color;

use super::{DiffuseLight, Material};

impl DiffuseLight {
    pub fn new(color: Color) -> Self {
        DiffuseLight { color }
    }
}

impl Material for DiffuseLight {
    fn scatter(&self) {}

    fn emit(&self) {}
}
