use std::{
    f64::{INFINITY, NEG_INFINITY},
    sync::Arc,
};

use crate::{
    data::HitInfo,
    material::Material,
    some_math::{order_numbers, Point, Vector3},
    systems::transform::rotate_around_y,
};

use super::{obj_traits::Hittable, Ray, Rectangle};

impl Rectangle {
    pub fn new(
        points: [Point; 2],
        angle_rotate_y: Option<f64>,
        material: Arc<dyn Material + Send + Sync>,
        id: usize,
    ) -> Self {
        if let Some(angle) = angle_rotate_y {
            let mut trans_points = [Point::default(); 2];
            let x_width = points[1].data[0] - points[0].data[0];
            let z_width = points[1].data[2] - points[0].data[2];
            let x_center = points[0].data[0] + x_width / 2.0;
            let z_center = points[0].data[2] + z_width / 2.0;
            let move_trans = Vector3::new([-x_center, 0.0, -z_center]);
            let mut rotated_position = [Point::default(); 2];
            for n in 0..2 {
                rotated_position[n] = rotate_around_y(points[n] + move_trans, angle) - move_trans;
            }
            for n in 0..2 {
                trans_points[n] = rotate_around_y(rotated_position[n], -angle);
            }
            return Rectangle {
                points,
                angle_rotate_y,
                trans_points,
                material,
                id,
            };
        } else {
            return Rectangle {
                points,
                angle_rotate_y,
                trans_points: points.clone(),
                material,
                id,
            };
        }
    }
}

impl Hittable for Rectangle {
    fn ray_intersect(&self, ray_in: &Ray) -> Option<HitInfo> {
        let mut ray_origin = ray_in.origin;
        let mut ray_direction = ray_in.direction;
        let mut p1 = self.points[0];
        let mut p2 = self.points[1];
        if let Some(angle) = self.angle_rotate_y {
            ray_origin = rotate_around_y(ray_origin, -angle);
            ray_direction = rotate_around_y(ray_direction, -angle);
            p1 = self.trans_points[0];
            p2 = self.trans_points[1];
        }
        let mut hit_normal = Vector3::default();
        let mut t_min = NEG_INFINITY;
        let mut t_max = INFINITY;
        for n in 0..3 {
            let t_0 = (p1.data[n] - ray_origin.data[n]) / ray_direction.data[n];
            let t_1 = (p2.data[n] - ray_origin.data[n]) / ray_direction.data[n];
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
            material: self.material.clone(),
            obj_id: self.id,
        });
    }

    fn is_light(&self) -> bool {
        self.material.is_light()
    }
}
