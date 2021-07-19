use std::sync::Arc;

use rand::{prelude::ThreadRng, Rng};

use crate::{
    data::HitInfo,
    material::{Light, Material},
    some_math::{point_in_2d, Point, Vector3},
};

use super::{
    obj_traits::{Hittable, HittableLight},
    Panel, Ray,
};

impl Panel {
    pub fn new(
        points: [Point; 2],
        normal: Vector3,
        material: Arc<dyn Material + Send + Sync>,
    ) -> Self {
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
                material: self.material.clone(),
            });
        } else {
            return None;
        }
    }

    fn is_light(&self) -> bool {
        self.material.is_light()
    }
}

impl Light for Panel {
    fn get_pdf_mul(&self) -> f64 {
        let axis = self.normal.get_axis();
        let mut res = 1.0;
        for n in 0..3 {
            if n == axis {
                continue;
            }
            res *= self.points[1].data[n] - self.points[0].data[n];
        }
        return res;
    }

    fn get_light_color(&self) -> crate::some_math::Color {
        self.material.emit()
    }
}

impl HittableLight for Panel {
    fn sample_on_light(&self, rng: &mut ThreadRng) -> (Point, Vector3) {
        let axis = self.normal.get_axis();
        let mut data = [0.0; 3];
        data[axis] = self.points[0].data[axis];
        for i in 0..3 {
            if i == axis {
                continue;
            }
            data[i] = rng.gen_range(self.points[0].data[i]..self.points[1].data[i]);
        }
        return (Point::new(data), self.normal);
    }
}
