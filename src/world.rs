use std::sync::Arc;

use image::{ImageBuffer, Rgb};
use rand::prelude::ThreadRng;

use crate::{
    camera::Camera,
    entity::obj_traits::{Hittable, HittableLight},
};

mod job_distribution;
mod multithread_impl;
mod world_impl;

pub struct World {
    pub objects: Vec<Arc<dyn Hittable + Send + Sync>>,
    pub camera: Camera,
    pub image_buffer: ImageBuffer<Rgb<u8>, Vec<u8>>,
    pub rng: ThreadRng,
    pub lights: Vec<Arc<dyn HittableLight + Send + Sync>>,
}
