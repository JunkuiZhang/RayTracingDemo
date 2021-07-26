use std::{
    sync::{Arc, RwLock},
    time::SystemTime,
};

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
    last_end_time: SystemTime,
    pub objects: Arc<RwLock<Vec<Arc<dyn Hittable + Send + Sync>>>>,
    pub camera: Arc<Camera>,
    pub rng: ThreadRng,
    pub lights: Arc<RwLock<Vec<Arc<dyn HittableLight + Send + Sync>>>>,
}
