use std::sync::Arc;

use crate::{
    material::Material,
    some_math::{Color, Point, Vector3},
};

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
