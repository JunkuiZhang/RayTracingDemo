use crate::some_math::Color;

mod diffuse_light_impl;
mod diffuse_mat_impl;

#[derive(Debug, Clone, Copy)]
pub struct DiffuseMat {
    pub diffuse_color: Color,
}

#[derive(Debug, Clone, Copy)]
pub struct DiffuseLight {
    pub color: Color,
}

pub trait Material {
    fn scatter(&self);
    fn emit(&self);
}
