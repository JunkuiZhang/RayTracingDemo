use crate::{camera::Camera, entity::obj_traits::Hittable};

mod world_impl;

pub struct World {
    pub objects: Vec<Box<dyn Hittable>>,
    pub camera: Camera,
}
