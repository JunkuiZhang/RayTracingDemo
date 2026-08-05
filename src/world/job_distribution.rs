use std::sync::Arc;

use rand::{rngs::StdRng, SeedableRng};

use crate::{
    camera::Camera,
    data::{GBInfo, RowColGBuffer, RowColPixels},
    entity::obj_traits::{Hittable, HittableLight},
    settings::{RAY_DEPTH, WINDOW_WIDTH},
    systems::path_tracing::shade,
};

pub fn process_job_sequence(
    content: u32,
    camera: Arc<Camera>,
    objects: &Vec<Arc<dyn Hittable + Send + Sync>>,
    lights: &Vec<Arc<dyn HittableLight + Send + Sync>>,
    samples_per_pixel: usize,
    seed: u64,
) -> (u32, RowColPixels, RowColGBuffer) {
    // 每行使用独立种子，使结果不受线程调度顺序影响。
    let row_seed = seed ^ (content as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut rng = StdRng::seed_from_u64(row_seed);
    let mut pixel_res = RowColPixels::new();
    let mut gbuffer_res = RowColGBuffer::new_empty();
    for col_num in 0..WINDOW_WIDTH {
        let ray_list = camera.generate_rays(col_num, content, samples_per_pixel, &mut rng);
        let mut gbuffer_data = GBInfo::default();
        let mut pixel_color = shade(
            &ray_list[0],
            objects,
            lights,
            RAY_DEPTH,
            &mut rng,
            true,
            &mut gbuffer_data,
        );
        if samples_per_pixel > 1 {
            for ray in ray_list.iter().skip(1) {
                pixel_color += shade(
                    ray,
                    objects,
                    lights,
                    RAY_DEPTH,
                    &mut rng,
                    false,
                    &mut GBInfo::default(),
                );
            }
            pixel_color /= samples_per_pixel as f64;
        }
        pixel_res.set_color(col_num as usize, pixel_color.data);
        gbuffer_res.push_data(gbuffer_data);
    }
    return (content, pixel_res, gbuffer_res);
}
