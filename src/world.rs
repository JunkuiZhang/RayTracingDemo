use std::{sync::Arc, time::SystemTime};

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
    start_time: SystemTime,
    pub objects: Vec<Arc<dyn Hittable + Send + Sync>>,
    pub camera: Camera,
    pub rng: ThreadRng,
    pub lights: Vec<Arc<dyn HittableLight + Send + Sync>>,
}
