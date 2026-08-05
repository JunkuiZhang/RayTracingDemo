use rand::rngs::StdRng;

use crate::{
    data::HitInfo,
    material::Light,
    some_math::{Point, Vector3},
};

use super::Ray;

pub trait Hittable {
    fn ray_intersect(&self, ray_in: &Ray) -> Option<HitInfo>;
    fn get_id(&self) -> usize;
}

pub trait HittableLight: Hittable + Light {
    fn sample_on_light(&self, rng: &mut StdRng) -> (Point, Vector3);
}
