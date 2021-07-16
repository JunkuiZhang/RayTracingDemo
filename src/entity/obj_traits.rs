use crate::data::HitInfo;

use super::Ray;

pub trait Hittable {
    fn ray_intersect(&self, ray_in: &Ray) -> Option<HitInfo>;
}
