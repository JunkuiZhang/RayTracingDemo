use std::sync::Arc;

use crate::{
    data::HitInfo,
    material::Material,
    some_math::{point_in_2d, Point, Vector3},
};

use super::{obj_traits::Hittable, Panel, Ray};

impl Panel {
    pub fn new(points: [Point; 2], normal: Vector3, material: Arc<dyn Material>) -> Self {
        Panel {
            points,
            normal,
            material,
        }
    }
}

impl Hittable for Panel {
    fn ray_intersect(&self, ray_in: &Ray) -> Option<HitInfo> {
        if ray_in.direction * self.normal > 0.0 {
            return None;
        }
        let axis = self.normal.get_axis();
        let t =
            (self.points[0].data[axis] - ray_in.origin.data[axis]) / ray_in.direction.data[axis];
        if t <= 0.0 {
            return None;
        }
        let hit_point = ray_in.at(t);
        if point_in_2d(hit_point, self.points, axis) {
            return Some(HitInfo {
                hit_point,
                t,
                normal: self.normal,
            });
        } else {
            return None;
        }
    }
}
