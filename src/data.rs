use std::sync::Arc;

use crate::{
    material::Material,
    settings::{WINDOW_HEIGHT, WINDOW_WIDTH},
    some_math::{clamp, Color, Point, Vector3},
};

#[derive(Debug, Clone, Copy)]
pub struct PixelContainer {
    pub data: [[f64; (WINDOW_WIDTH * 3) as usize]; WINDOW_HEIGHT as usize],
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

impl PixelContainer {
    pub fn new() -> Self {
        PixelContainer {
            data: [[0.0; (WINDOW_WIDTH * 3) as usize]; WINDOW_HEIGHT as usize],
        }
    }

    pub fn to_pixels(&self) -> Vec<u8> {
        let mut res = Vec::with_capacity((WINDOW_HEIGHT * WINDOW_WIDTH * 3) as usize);
        for num in self.data.concat().iter() {
            res.push((clamp((*num).sqrt(), 0.0, 1.0) * 255.0) as u8);
        }
        return res;
    }
}
