use rand::{prelude::ThreadRng, Rng};

use crate::{
    data::ScatterInfo,
    entity::Ray,
    some_math::{reflect, refract, Color, Vector3},
};

use super::{Glass, Material};

impl Glass {
    pub fn new(color: Color, eta: f64) -> Self {
        Glass { color, eta }
    }
}

impl Material for Glass {
    fn scatter(&self, ray_in: &Ray, hit_normal: &Vector3, rng: &mut ThreadRng) -> ScatterInfo {
        let reflection_portion = self.get_fresnel(&ray_in.direction, hit_normal);
        if rng.gen_range(0.0..1.0) < reflection_portion {
            let scatter_dir = reflect(&ray_in.direction, hit_normal);
            return ScatterInfo {
                scatter_dir,
                color: self.color,
                pdf: 1.0,
            };
        } else {
            let refraction_ratio;
            if ray_in.direction * (*hit_normal) > 0.0 {
                refraction_ratio = self.eta;
            } else {
                refraction_ratio = 1.0 / self.eta;
            }
            let cos_theta = ((-1.0) * ray_in.direction * (*hit_normal)).min(1.0);
            let sin_theta = (1.0 - cos_theta * cos_theta).sqrt();
            let connot_refract = sin_theta * refraction_ratio > 1.0;
            let scatter_dir;
            if connot_refract {
                scatter_dir = reflect(&ray_in.direction, hit_normal);
            } else {
                scatter_dir = refract(&ray_in.direction, hit_normal, refraction_ratio);
            }
            // let scatter_dir = refract(&ray_in.direction, hit_normal, refraction_ratio);
            return ScatterInfo {
                scatter_dir,
                color: self.color,
                pdf: 1.0,
            };
        }
    }

    fn emit(&self) -> Color {
        Color::BLACK
    }

    fn get_color(&self) -> Color {
        self.color
    }

    fn is_light(&self) -> bool {
        false
    }

    fn get_fresnel(&self, ray_in_dir: &Vector3, hit_normal: &Vector3) -> f64 {
        let f0 = 0.05;
        return f0 + (1.0 - f0) * ((*ray_in_dir) * (*hit_normal)).abs().powi(5);
    }
}
