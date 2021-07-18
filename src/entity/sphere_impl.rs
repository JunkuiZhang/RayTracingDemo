use std::sync::Arc;

use crate::{data::HitInfo, material::Material, some_math::Point};

use super::{obj_traits::Hittable, Ray, Sphere};

impl Sphere {
    pub fn new(center: Point, radius: f64, material: Arc<dyn Material + Send + Sync>) -> Self {
        Sphere {
            center,
            radius,
            material,
        }
    }
}

impl Hittable for Sphere {
    fn ray_intersect(&self, ray_in: &Ray) -> Option<HitInfo> {
        let oc = ray_in.origin - self.center;
        let a = ray_in.direction * ray_in.direction;
        let b = 2.0 * ray_in.direction * oc;
        let c = oc * oc - self.radius * self.radius;
        let indicator = b * b - 4.0 * a * c;
        if indicator < 0.0 {
            return None;
        }
        let t = (-b - indicator.sqrt()) / (2.0 * a);
        let hit_point = ray_in.at(t);
        let normal = (hit_point - self.center).normalize();
        return Some(HitInfo {
            hit_point,
            t,
            normal,
            material: self.material.clone(),
        });
    }

    fn is_light(&self) -> bool {
        self.material.is_light()
    }
}
