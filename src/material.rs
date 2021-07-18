use rand::prelude::ThreadRng;

use crate::{
    data::ScatterInfo,
    entity::Ray,
    some_math::{Color, Point, Vector3},
};

mod diffuse_light_impl;
mod diffuse_mat_impl;
mod metal_impl;

#[derive(Debug, Clone, Copy)]
pub struct DiffuseMat {
    pub diffuse_color: Color,
}

#[derive(Debug, Clone, Copy)]
pub struct DiffuseLight {
    pub color: Color,
    // pub area: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Metal {
    pub color: Color,
}

pub trait Material {
    fn scatter(&self, ray_in: &Ray, hit_normal: &Vector3, rng: &mut ThreadRng) -> ScatterInfo;
    fn emit(&self) -> Option<Color>;
    fn naive_render(&self) -> Color;
    fn is_light(&self) -> bool;
}

pub trait Light {
    fn get_pdf_mul(&self) -> f64;
    fn get_light_color(&self) -> Color;
}
