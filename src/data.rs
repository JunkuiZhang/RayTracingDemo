use std::sync::Arc;

use crate::{
    material::Material,
    some_math::{Color, Point, Vector3},
};

mod gbuffer_impl;
mod pixel_data_impl;

pub struct PixelContainer {
    data: Vec<RowPixels>,
}

#[derive(Debug, Clone)]
pub struct RowPixels {
    data: Vec<f64>,
}

#[derive(Debug, Clone)]
pub struct ColPixels {
    data: Vec<f64>,
}

#[derive(Debug, Clone)]
pub struct GeometryBuffer {
    data: Vec<GeometryRowBuffer>,
}

#[derive(Debug, Clone)]
pub struct GeometryRowBuffer {
    data: Vec<GBInfo>,
}

#[derive(Debug, Clone)]
pub struct GeometryColBuffer {
    data: Vec<GBInfo>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GBInfo {
    pub distance: f64,
    pub normal: Vector3,
    pub hit_point: Point,
    pub hit_obj_id: usize,
}

#[derive(Clone)]
pub struct HitInfo {
    pub hit_point: Point,
    pub t: f64,
    pub normal: Vector3,
    pub material: Arc<dyn Material>,
}

#[derive(Debug, Clone, Copy)]
pub struct ScatterInfo {
    pub scatter_dir: Vector3,
    pub color: Color,
    pub pdf: f64,
}
