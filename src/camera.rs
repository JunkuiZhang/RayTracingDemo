use image::imageops::colorops;
use rand::{prelude::ThreadRng, Rng};

use crate::{
    settings::{ASPECT_RATIO, CAMERA_HEIGHT, FOV, SAMPLES_PER_PIXEL},
    some_math::{Point, Vector3},
};

#[derive(Debug, Clone, Copy, Default)]
pub struct Camera {
    position: Point,
    lookat: Vector3,
    updir: Vector3,
    u: Vector3,
    v: Vector3,
    w: Vector3,
    upper_left_point: Point,
    view_width: f64,
    view_height: f64,
}

impl Camera {
    pub fn new(position: Point, lookat: Vector3, updir: Vector3) -> Self {
        let u = lookat.cross_product(updir);
        let v = updir;
        let w = u.cross_product(v);
        let view_height = CAMERA_HEIGHT;
        let view_width = CAMERA_HEIGHT * ASPECT_RATIO;
        let view_length = (view_height / 2.0) / (FOV.to_radians() / 2.0).tan();
        let upper_left_point =
            position + lookat * view_length - view_width / 2.0 * u + view_height / 2.0 * v;
        Camera {
            position,
            lookat,
            updir,
            u,
            v,
            w,
            view_height,
            view_width,
            upper_left_point,
        }
    }

    pub fn generate_ray(
        &self,
        col_num: usize,
        row_num: usize,
        rng: &mut ThreadRng,
    ) -> [Vector3; SAMPLES_PER_PIXEL] {
        let mut res = [Vector3::default(); SAMPLES_PER_PIXEL];
        for n in 0..SAMPLES_PER_PIXEL {
            res[n] = self.upper_left_point
                + ((col_num as f64 + rng.gen_range(0.0..1.0)) * self.u
                    - (row_num as f64 + rng.gen_range(0.0..1.0)) * self.v);
        }
        return res;
    }
}
