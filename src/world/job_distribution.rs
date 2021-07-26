use std::sync::Arc;

use rand::prelude::ThreadRng;

use crate::{
    camera::Camera,
    data::RowPixels,
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
) -> (u32, RowPixels) {
    let mut res = RowPixels::new();
    for col_num in 0..WINDOW_WIDTH {
        let mut pixel_color = Color::BLACK;
        for ray in camera.generate_rays(col_num, content, rng).iter() {
            pixel_color += shade(ray, objects, lights, RAY_DEPTH, rng, false);
        }
        pixel_color /= SAMPLES_PER_PIXEL as f64;
        res.set_color(col_num as usize, pixel_color.data);
    }
    return (content, res);
}
