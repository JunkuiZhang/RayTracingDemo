use std::{
    sync::{Arc, RwLock},
    time::SystemTime,
};

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
    objects: Arc<RwLock<Vec<Arc<dyn Hittable + Send + Sync>>>>,
    camera: Arc<Camera>,
    lights: Arc<RwLock<Vec<Arc<dyn HittableLight + Send + Sync>>>>,
}
