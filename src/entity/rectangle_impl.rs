use std::{f64::{INFINITY, NEG_INFINITY}, sync::Arc};

use crate::{data::HitInfo, material::Material, some_math::{order_numbers, Point, Vector3}, systems::transform::rotate_around_y};

use super::{obj_traits::Hittable, Ray, Rectangle};

impl Rectangle {
    pub fn new(points: [Point; 2], angle_rotate_y: Option<f64>, material: Arc<dyn Material>) -> Self {
        Rectangle {
            points,
            angle_rotate_y,
            material
        }
    }
}

impl Hittable for Rectangle {
    fn ray_intersect(&self, ray_in: &Ray) -> Option<HitInfo> {
        let mut ray_origin = ray_in.origin;
        let mut ray_direction = ray_in.direction;
        if let Some(angle) = self.angle_rotate_y {
            ray_origin = rotate_around_y(ray_origin, -angle);
            ray_direction = rotate_around_y(ray_direction, -angle);
        }
        let mut hit_normal = Vector3::default();
        let mut t_min = NEG_INFINITY;
        let mut t_max = INFINITY;
        for n in 0..3 {
            let t_0 = (self.points[0].data[n] - ray_origin.data[n]) / ray_direction.data[n];
            let t_1 = (self.points[1].data[n] - ray_origin.data[n]) / ray_direction.data[n];
            let (t0, t1, indicator) = order_numbers(t_0, t_1);
            if t0 > t_min {
                t_min = t0;
                if indicator {
                    hit_normal = Vector3::unit_vec_from_axis(n).unwrap();
                } else {
                    hit_normal = Vector3::unit_vec_from_axis(n).unwrap() * (-1.0);
                }
            }
            if t1 < t_max {
                t_max = t1;
            }
        }
        if t_min > t_max || t_min <= 0.0 {
            return None;
        }
        let hit_point = ray_in.at(t_min);
        let normal;
        if let Some(angle) = self.angle_rotate_y {
            normal = rotate_around_y(hit_normal, angle);
        } else {
            normal = hit_normal;
        }
        return Some(HitInfo {
            hit_point,
            t: t_min,
            normal,
        });
    }
}
