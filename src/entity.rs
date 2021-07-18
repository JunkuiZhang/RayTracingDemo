use std::sync::Arc;

use crate::{
    material::Material,
    some_math::{Point, Vector3},
};

pub mod obj_traits;
mod panel_impl;
mod ray_impl;
mod rectangle_impl;
mod sphere_impl;

#[derive(Clone)]
pub struct Sphere {
    pub center: Point,
    pub radius: f64,
    pub material: Arc<dyn Material + Send + Sync>,
}

#[derive(Clone)]
pub struct Panel {
    pub points: [Point; 2],
    pub normal: Vector3,
    pub material: Arc<dyn Material + Send + Sync>,
}

#[derive(Clone)]
pub struct Rectangle {
    pub points: [Point; 2],
    // agnle in radians
    pub angle_rotate_y: Option<f64>,
    pub trans_points: [Point; 2],
    pub material: Arc<dyn Material + Send + Sync>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Ray {
    pub origin: Point,
    pub direction: Vector3,
}
