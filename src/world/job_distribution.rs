use std::sync::Arc;

use rand::prelude::ThreadRng;

use crate::{
    camera::Camera,
    data::{GBInfo, GeometryRowBuffer, RowPixels},
    entity::obj_traits::{Hittable, HittableLight},
    settings::{RAY_DEPTH, SAMPLES_PER_PIXEL, WINDOW_WIDTH},
    some_math::Color,
    systems::path_tracing::shade,
};

pub fn process_job_sequence(
    content: u32,
    camera: Arc<Camera>,
    objects: &Vec<Arc<dyn Hittable + Send + Sync>>,
    lights: &Vec<Arc<dyn HittableLight + Send + Sync>>,
    rng: &mut ThreadRng,
) -> (u32, RowPixels, GeometryRowBuffer) {
    let mut pixel_res = RowPixels::new();
    let mut gbuffer_res = GeometryRowBuffer::new_empty();
    for col_num in 0..WINDOW_WIDTH {
        let ray_list = camera.generate_rays(col_num, content, rng);
        let mut gbuffer_data = GBInfo::default();
        let mut pixel_color = shade(
            &ray_list[0],
            objects,
            lights,
            RAY_DEPTH,
            rng,
            false,
            true,
            &mut gbuffer_data,
        );
        if SAMPLES_PER_PIXEL > 1 {
            for index in 1..SAMPLES_PER_PIXEL {
                let ray = &ray_list[index];
                pixel_color += shade(
                    ray,
                    objects,
                    lights,
                    RAY_DEPTH,
                    rng,
                    false,
                    false,
                    &mut GBInfo::default(),
                );
            }
            pixel_color /= SAMPLES_PER_PIXEL as f64;
        }
        pixel_res.set_color(col_num as usize, pixel_color.data);
        gbuffer_res.push_data(gbuffer_data);
    }
    return (content, pixel_res, gbuffer_res);
}
