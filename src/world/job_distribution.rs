use std::sync::Arc;

use rand::prelude::ThreadRng;

use crate::{
    camera::Camera,
    entity::obj_traits::{Hittable, HittableLight},
    settings::{RAY_DEPTH, SAMPLES_PER_PIXEL, WINDOW_WIDTH},
    some_math::Color,
    systems::path_tracing::shade,
};

pub fn process_job_sequence(
    content: u32,
    camera: &Arc<Camera>,
    objects: &Arc<Vec<Arc<dyn Hittable + Send + Sync>>>,
    lights: &Arc<Vec<Arc<dyn HittableLight + Send + Sync>>>,
    rng: &mut ThreadRng,
) -> (u32, [u8; (WINDOW_WIDTH * 3) as usize]) {
    let mut res = [0u8; (WINDOW_WIDTH * 3) as usize];
    for col_num in 0..WINDOW_WIDTH {
        let mut pixel_color = Color::BLACK;
        for ray in camera.generate_rays(col_num, content, rng).iter() {
            pixel_color += shade(ray, objects, lights, RAY_DEPTH, rng, false);
        }
        pixel_color /= SAMPLES_PER_PIXEL as f64;
        let temp_color = pixel_color.to_u8();
        res[(3 * col_num) as usize] = temp_color[0];
        res[(3 * col_num + 1) as usize] = temp_color[1];
        res[(3 * col_num + 2) as usize] = temp_color[2];
    }
    return (content, res);
}
