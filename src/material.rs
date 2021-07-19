use rand::prelude::ThreadRng;

use crate::{
    data::ScatterInfo,
    entity::Ray,
    some_math::{Color, Vector3},
};

mod diffuse_light_impl;
mod diffuse_mat_impl;
mod glass_impl;
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
    fuzz: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Glass {
    color: Color,
    // index of refraction
    eta: f64,
}

pub trait Material {
    fn scatter(&self, ray_in: &Ray, hit_normal: &Vector3, rng: &mut ThreadRng) -> ScatterInfo;
    fn emit(&self) -> Color;
    fn get_color(&self) -> Color;
    fn is_light(&self) -> bool;
    fn get_fresnel(&self, ray_in_dir: &Vector3, hit_normal: &Vector3) -> f64;
}

pub trait Light {
    fn get_pdf_mul(&self) -> f64;
    fn get_light_color(&self) -> Color;
}
