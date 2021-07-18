use std::{f64::INFINITY, sync::Arc};

use rand::prelude::ThreadRng;

use crate::{
    data::HitInfo,
    entity::{
        obj_traits::{Hittable, HittableLight},
        Ray,
    },
    material::Material,
    some_math::{Color, Point, Vector3},
};

pub fn shade(
    ray_in: &Ray,
    objects: &Arc<Vec<Arc<dyn Hittable + Send + Sync>>>,
    lights: &Arc<Vec<Arc<dyn HittableLight + Send + Sync>>>,
    depth: i32,
    rng: &mut ThreadRng,
    dismiss_light: bool,
) -> Color {
    if depth < 0 {
        return Color::BLACK;
    }
    if let Some(info) = ray_hit(ray_in, objects, dismiss_light) {
        // return info.material.naive_render();
        return shade_point(
            ray_in,
            &info.hit_point,
            &info.material,
            &info.normal,
            objects,
            lights,
            depth - 1,
            rng,
            true,
        );
    }
    return Color::BLACK;
}

fn ray_hit(
    ray_in: &Ray,
    objects: &Arc<Vec<Arc<dyn Hittable + Send + Sync>>>,
    dismiss_light: bool,
) -> Option<HitInfo> {
    let mut t = INFINITY;
    let mut hit_info = None;
    for obj in objects.iter() {
        if dismiss_light && obj.is_light() {
            continue;
        }
        if let Some(info) = obj.ray_intersect(ray_in) {
            if info.t < t {
                t = info.t;
                hit_info = Some(info);
            }
        }
    }
    return hit_info;
}

fn shade_point(
    ray_in: &Ray,
    point: &Point,
    point_material: &Arc<dyn Material>,
    point_normal: &Vector3,
    objects: &Arc<Vec<Arc<dyn Hittable + Send + Sync>>>,
    lights: &Arc<Vec<Arc<dyn HittableLight + Send + Sync>>>,
    depth: i32,
    rng: &mut ThreadRng,
    dismiss_light: bool,
) -> Color {
    if let Some(light_color) = point_material.emit() {
        return light_color;
    } else {
        // naive ray tracing
        let mut shade_color = Color::BLACK;
        // direct shading
        for light in lights.iter() {
            let pdf = light.get_pdf_mul();
            let (sample_point, sample_normal) = light.sample_on_light(rng);
            let sample_point_to_point = *point - sample_point;
            if sample_point_to_point * (*point_normal) >= 0.0
                || sample_point_to_point * sample_normal <= 0.0
            {
                continue;
            }
            let lq = sample_point_to_point.length_square();
            let unit_sptp = sample_point_to_point.normalize();
            let temp_ray = Ray::new(sample_point, unit_sptp);
            if let Some(thing) = ray_hit(&temp_ray, objects, true) {
                if (thing.hit_point - sample_point).length_square() < lq {
                    continue;
                }
            }
            let cos_theta = (-1.0) * unit_sptp * (*point_normal);
            let cos_theta_prime = unit_sptp * sample_normal;

            shade_color += light.get_light_color() * cos_theta * cos_theta_prime / lq * pdf;
        }
        // indirect shading
        let scatter_info = point_material.scatter(ray_in, point_normal, rng);
        let scatter_ray = Ray::new(*point, scatter_info.scatter_dir);
        shade_color += scatter_info.color.naive_mul(shade(
            &scatter_ray,
            objects,
            lights,
            depth,
            rng,
            dismiss_light,
        )) / scatter_info.pdf;
        return shade_color;
    }
}
