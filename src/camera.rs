use rand::{RngExt, rngs::StdRng};

use crate::{
    entity::Ray,
    settings::{FOV, WINDOW_HEIGHT, WINDOW_WIDTH},
    some_math::{Point, Vector3},
};

#[derive(Debug, Clone, Copy, Default)]
pub struct Camera {
    position: Point,
    u: Vector3,
    v: Vector3,
    upper_left_point: Point,
}

impl Camera {
    pub fn new(position: Point, lookat: Vector3, updir: Vector3) -> Self {
        let u = lookat.cross_product(updir);
        let v = updir;
        let window_length = (WINDOW_HEIGHT as f64 / 2.0) / (FOV / 2.0).to_radians().tan();
        let upper_left_point = position + window_length * lookat + (WINDOW_HEIGHT as f64 / 2.0) * v
            - (WINDOW_WIDTH as f64 / 2.0) * u;
        Camera {
            position,
            u,
            v,
            upper_left_point,
        }
    }

    pub fn generate_rays(
        &self,
        col_num: u32,
        row_num: u32,
        samples_per_pixel: usize,
        rng: &mut StdRng,
    ) -> Vec<Ray> {
        let mut rays = Vec::with_capacity(samples_per_pixel);
        for sample_index in 0..samples_per_pixel {
            let target;
            if sample_index == 0 {
                target = self.upper_left_point + (col_num as f64 + 0.5) * self.u
                    - (row_num as f64 + 0.5) * self.v;
            } else {
                target = self.upper_left_point
                    + (col_num as f64 + rng.random_range(0.0..1.0)) * self.u
                    - (row_num as f64 + rng.random_range(0.0..1.0)) * self.v;
            }
            let ray = Ray::new(self.position, (target - self.position).normalize());
            rays.push(ray);
        }
        rays
    }
}
