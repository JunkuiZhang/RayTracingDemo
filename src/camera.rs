use rand::{prelude::ThreadRng, Rng};

use crate::{
    entity::Ray,
    settings::{ASPECT_RATIO, CAMERA_HEIGHT, FOV, SAMPLES_PER_PIXEL, WINDOW_HEIGHT, WINDOW_WIDTH},
    some_math::{Point, Vector3},
};

#[derive(Debug, Clone, Copy, Default)]
pub struct Camera {
    position: Point,
    lookat: Vector3,
    updir: Vector3,
    u: Vector3,
    v: Vector3,
    upper_left_point: Point,
    view_width: f64,
    view_height: f64,
}

impl Camera {
    pub fn new(position: Point, lookat: Vector3, updir: Vector3) -> Self {
        let u = lookat.cross_product(updir);
        let v = updir;
        let view_height = CAMERA_HEIGHT;
        let view_width = CAMERA_HEIGHT * ASPECT_RATIO;
        // let view_length = (view_height / 2.0) / (FOV / 2.0).to_radians().tan();
        let window_length = (WINDOW_HEIGHT as f64 / 2.0) / (FOV / 2.0).to_radians().tan();
        let upper_left_point = position + window_length * lookat + (WINDOW_HEIGHT as f64 / 2.0) * v
            - (WINDOW_WIDTH as f64 / 2.0) * u;
        Camera {
            position,
            lookat,
            updir,
            u,
            v,
            view_height,
            view_width,
            upper_left_point,
        }
    }

    pub fn generate_rays(
        &self,
        col_num: u32,
        row_num: u32,
        rng: &mut ThreadRng,
    ) -> [Ray; SAMPLES_PER_PIXEL] {
        let mut res = [Ray::default(); SAMPLES_PER_PIXEL];
        for n in 0..SAMPLES_PER_PIXEL {
            let target = self.upper_left_point
                + (col_num as f64 + rng.gen_range(0.0..1.0)) * self.u
                - (row_num as f64 + rng.gen_range(0.0..1.0)) * self.v;
            let ray = Ray::new(self.position, (target - self.position).normalize());
            res[n] = ray;
        }
        return res;
    }
}
