use crate::some_math::{Point, Vector3};

#[derive(Debug, Clone, Copy, Default)]
pub struct HitInfo {
    pub hit_point: Point,
    pub t: f64,
    pub normal: Vector3,
}
