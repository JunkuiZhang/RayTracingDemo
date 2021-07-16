use crate::some_math::{Point, Vector3};

use super::Ray;

impl Ray {
    pub fn new(origin: Point, direction: Vector3) -> Ray {
        Ray { origin, direction }
    }

    pub fn at(&self, t: f64) -> Point {
        self.origin + t * self.direction
    }
}
