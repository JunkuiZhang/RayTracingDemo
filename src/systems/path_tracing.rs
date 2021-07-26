use std::{f64::INFINITY, sync::Arc};

use rand::prelude::ThreadRng;

use crate::{
    data::{GBInfo, HitInfo},
    entity::{
        obj_traits::{Hittable, HittableLight},
        Ray,
    },
    material::Material,
    settings::RAY_DEPTH,
    some_math::{Color, Point, Vector3},
};

pub fn shade(
    ray_in: &Ray,
    objects: &Vec<Arc<dyn Hittable + Send + Sync>>,
    lights: &Vec<Arc<dyn HittableLight + Send + Sync>>,
    depth: i32,
    rng: &mut ThreadRng,
    dismiss_light: bool,
    gb_indicator: bool,
    gbuffer_data: &mut GBInfo,
) -> Color {
    if depth < 0 {
        return Color::BLACK;
    }
    let mut dl = dismiss_light;
    if depth == RAY_DEPTH {
        dl = false;
    }
    if let Some(info) = ray_hit(ray_in, objects, dl) {
        if gb_indicator {
            *gbuffer_data = GBInfo {
                distance: (ray_in.at(info.t) - ray_in.origin).length(),
                normal: info.normal,
                hit_point: info.hit_point,
            }
        }
        return shade_point(
            ray_in,
            &info.hit_point,
            &info.material,
            &info.normal,
            objects,
            lights,
            depth - 1,
            rng,
            dl,
        );
    }
    return Color::BLACK;
}

fn ray_hit(
    ray_in: &Ray,
    // objects: &Arc<Vec<Arc<dyn Hittable + Send + Sync>>>,
    objects: &Vec<Arc<dyn Hittable + Send + Sync>>,
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
    // objects: &Arc<Vec<Arc<dyn Hittable + Send + Sync>>>,
    objects: &Vec<Arc<dyn Hittable + Send + Sync>>,
    lights: &Vec<Arc<dyn HittableLight + Send + Sync>>,
    depth: i32,
    rng: &mut ThreadRng,
    dismiss_light: bool,
) -> Color {
    let mut shade_color = point_material.emit();
    if point_material.is_light() {
        return shade_color;
    }
    // naive ray tracing
    let fresnel_factor = point_material.get_fresnel(&ray_in.direction, point_normal);
    let albedo = point_material.get_color();
    // direct shading
    for light in lights.iter() {
        let pdf_mul = light.get_pdf_mul();
        let (sample_point, sample_normal) = light.sample_on_light(rng);
        let sample_point_to_point = *point - sample_point;
        if sample_point_to_point * (*point_normal) >= 0.0
            || sample_point_to_point * sample_normal <= 0.0
        {
            continue;
        }
        let length_square = sample_point_to_point.length_square();
        let unit_sptp = sample_point_to_point.normalize();
        let temp_ray = Ray::new(sample_point, unit_sptp);
        if let Some(thing) = ray_hit(&temp_ray, objects, true) {
            if (thing.hit_point - sample_point).length_square() < length_square {
                continue;
            }
        }
        let cos_theta = (unit_sptp * (*point_normal)).abs();
        let cos_theta_prime = (unit_sptp * sample_normal).abs();
        shade_color += albedo.naive_mul(light.get_light_color()) * cos_theta * cos_theta_prime
            / length_square
            * pdf_mul
            * fresnel_factor
            * 0.5;
    }
    // indirect shading
    let scatter_info = point_material.scatter(ray_in, point_normal, rng);
    let scatter_ray = Ray::new(*point, scatter_info.scatter_dir);
    shade_color += albedo.naive_mul(shade(
        &scatter_ray,
        objects,
        lights,
        depth,
        rng,
        dismiss_light,
        false,
        &mut GBInfo::default(),
    )) * ((*point_normal) * scatter_info.scatter_dir).abs()
        / scatter_info.pdf
        * fresnel_factor
        * 0.5;
    // )) / scatter_info.pdf;
    return shade_color;
}
