use std::{f64::INFINITY, sync::Arc};

use rand::rngs::StdRng;

use crate::{
    data::{GBInfo, HitInfo},
    entity::{
        obj_traits::{Hittable, HittableLight},
        Ray,
    },
    some_math::{Color, Point, Vector3},
};

const RAY_EPSILON: f64 = 1e-4;

#[derive(Clone, Copy)]
struct EmissionContext {
    origin: Point,
    bsdf_pdf: f64,
    is_delta: bool,
}

pub fn shade(
    ray_in: &Ray,
    objects: &Vec<Arc<dyn Hittable + Send + Sync>>,
    lights: &Vec<Arc<dyn HittableLight + Send + Sync>>,
    depth: i32,
    rng: &mut StdRng,
    gb_indicator: bool,
    gbuffer_data: &mut GBInfo,
) -> Color {
    shade_recursive(
        ray_in,
        objects,
        lights,
        depth,
        rng,
        gb_indicator,
        gbuffer_data,
        None,
    )
}

fn shade_recursive(
    ray_in: &Ray,
    objects: &Vec<Arc<dyn Hittable + Send + Sync>>,
    lights: &Vec<Arc<dyn HittableLight + Send + Sync>>,
    depth: i32,
    rng: &mut StdRng,
    gb_indicator: bool,
    gbuffer_data: &mut GBInfo,
    emission_context: Option<EmissionContext>,
) -> Color {
    if depth <= 0 {
        return Color::BLACK;
    }

    let Some(info) = ray_hit(ray_in, objects) else {
        return Color::BLACK;
    };
    if gb_indicator {
        *gbuffer_data = GBInfo {
            distance: (info.hit_point - ray_in.origin).length(),
            normal: info.normal,
            hit_point: info.hit_point,
            hit_obj_id: info.obj_id,
            albedo: info.material.denoise_albedo(),
        };
    }
    if info.material.is_light() {
        return weighted_emission(&info, ray_in, lights, emission_context);
    }

    shade_point(ray_in, &info, objects, lights, depth - 1, rng)
}

fn ray_hit(ray_in: &Ray, objects: &Vec<Arc<dyn Hittable + Send + Sync>>) -> Option<HitInfo> {
    let mut closest_distance = INFINITY;
    let mut hit_info = None;
    for object in objects {
        if let Some(info) = object.ray_intersect(ray_in) {
            if info.t > RAY_EPSILON && info.t < closest_distance {
                closest_distance = info.t;
                hit_info = Some(info);
            }
        }
    }
    hit_info
}

fn shade_point(
    ray_in: &Ray,
    info: &HitInfo,
    objects: &Vec<Arc<dyn Hittable + Send + Sync>>,
    lights: &Vec<Arc<dyn HittableLight + Send + Sync>>,
    depth: i32,
    rng: &mut StdRng,
) -> Color {
    let direct_light = estimate_direct_light(info, objects, lights, rng);
    let scatter_info = info.material.scatter(ray_in, &info.normal, rng);
    let scatter_direction = scatter_info.scatter_dir.normalize();
    let scatter_origin = offset_ray_origin(info.hit_point, info.normal, scatter_direction);
    let scatter_ray = Ray::new(scatter_origin, scatter_direction);
    let is_delta = info.material.is_delta();
    let emission_context = EmissionContext {
        origin: info.hit_point,
        bsdf_pdf: scatter_info.pdf,
        is_delta,
    };
    let incoming_light = shade_recursive(
        &scatter_ray,
        objects,
        lights,
        depth,
        rng,
        false,
        &mut GBInfo::default(),
        Some(emission_context),
    );

    let indirect_light = if is_delta {
        // 理想镜面和玻璃是离散事件，吞吐量由材质颜色直接给出。
        scatter_info.color.naive_mul(incoming_light)
    } else {
        let cosine = (scatter_direction * info.normal).max(0.0);
        let pdf = info
            .material
            .scattering_pdf(&scatter_direction, &info.normal);
        if pdf <= 0.0 || cosine <= 0.0 {
            Color::BLACK
        } else {
            let brdf = info
                .material
                .evaluate_brdf(&scatter_direction, &info.normal);
            brdf.naive_mul(incoming_light) * cosine / pdf
        }
    };

    direct_light + indirect_light
}

fn estimate_direct_light(
    info: &HitInfo,
    objects: &Vec<Arc<dyn Hittable + Send + Sync>>,
    lights: &Vec<Arc<dyn HittableLight + Send + Sync>>,
    rng: &mut StdRng,
) -> Color {
    if info.material.is_delta() {
        return Color::BLACK;
    }

    let mut result = Color::BLACK;
    for light in lights {
        let (sample_point, sample_normal) = light.sample_on_light(rng);
        let to_light = sample_point - info.hit_point;
        let distance_square = to_light.length_square();
        if distance_square <= RAY_EPSILON {
            continue;
        }

        let light_direction = to_light.normalize();
        let surface_cosine = (light_direction * info.normal).max(0.0);
        let light_cosine = ((-1.0 * light_direction) * sample_normal).max(0.0);
        if surface_cosine <= 0.0 || light_cosine <= 0.0 {
            continue;
        }
        if is_occluded(info.hit_point, info.normal, sample_point, objects) {
            continue;
        }

        // 将均匀面积采样的 PDF 转换到立体角测度，再与 BSDF PDF 做幂启发式 MIS。
        let light_pdf = distance_square / (light.get_pdf_mul() * light_cosine);
        let bsdf_pdf = info.material.scattering_pdf(&light_direction, &info.normal);
        let mis_weight = power_heuristic(light_pdf, bsdf_pdf);
        let brdf = info.material.evaluate_brdf(&light_direction, &info.normal);
        result += brdf.naive_mul(light.get_light_color()) * surface_cosine * mis_weight / light_pdf;
    }
    result
}

fn is_occluded(
    point: Point,
    normal: Vector3,
    sample_point: Point,
    objects: &Vec<Arc<dyn Hittable + Send + Sync>>,
) -> bool {
    let initial_direction = (sample_point - point).normalize();
    let origin = offset_ray_origin(point, normal, initial_direction);
    let to_sample = sample_point - origin;
    let maximum_distance = to_sample.length();
    let shadow_ray = Ray::new(origin, to_sample / maximum_distance);
    if let Some(info) = ray_hit(&shadow_ray, objects) {
        info.t < maximum_distance - RAY_EPSILON
    } else {
        false
    }
}

fn weighted_emission(
    info: &HitInfo,
    ray_in: &Ray,
    lights: &Vec<Arc<dyn HittableLight + Send + Sync>>,
    context: Option<EmissionContext>,
) -> Color {
    let emission = info.material.emit();
    let Some(context) = context else {
        // 相机直接看到光源时不存在另一种采样策略，权重为一。
        return emission;
    };
    if context.is_delta {
        return emission;
    }

    let Some(light) = lights.iter().find(|light| light.get_id() == info.obj_id) else {
        return emission;
    };
    let distance_square = (info.hit_point - context.origin).length_square();
    let light_cosine = ((-1.0 * ray_in.direction) * info.normal).max(0.0);
    if light_cosine <= 0.0 {
        return Color::BLACK;
    }
    let light_pdf = distance_square / (light.get_pdf_mul() * light_cosine);
    emission * power_heuristic(context.bsdf_pdf, light_pdf)
}

fn power_heuristic(primary_pdf: f64, secondary_pdf: f64) -> f64 {
    let primary_square = primary_pdf * primary_pdf;
    let secondary_square = secondary_pdf * secondary_pdf;
    primary_square / (primary_square + secondary_square).max(1e-12)
}

fn offset_ray_origin(point: Point, normal: Vector3, direction: Vector3) -> Point {
    // 沿射线所在的一侧偏移起点，避免浮点误差让二次射线再次击中当前表面。
    if direction * normal >= 0.0 {
        point + RAY_EPSILON * normal
    } else {
        point - RAY_EPSILON * normal
    }
}
