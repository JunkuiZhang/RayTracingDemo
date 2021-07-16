use std::sync::Arc;

use crate::{
    camera::Camera,
    entity::{obj_traits::Hittable, Panel, Rectangle},
    material::{DiffuseLight, DiffuseMat},
    some_math::{Color, Point, Vector3},
};

use super::World;

impl World {
    pub fn new() -> Self {
        World {
            objects: Vec::new(),
            camera: Camera::default(),
        }
    }

    pub fn add(&mut self, obj: Box<dyn Hittable>) {
        self.objects.push(obj);
    }

    pub fn default_scene(&mut self) {
        let red = DiffuseMat::new(Color::new([0.65, 0.05, 0.05]));
        let white = DiffuseMat::new(Color::new([0.73, 0.73, 0.73]));
        let green = DiffuseMat::new(Color::new([0.12, 0.45, 0.15]));
        let light = DiffuseLight::new(Color::new([15.0, 15.0, 15.0]));
        // light
        self.add(Box::new(Panel::new(
            [
                Point::new([250.0, 599.0, -350.0]),
                Point::new([350.0, 599.0, -250.0]),
            ],
            Vector3::new([0.0, -1.0, 0.0]),
            Arc::new(light),
        )));
        // top
        self.add(Box::new(Panel::new(
            [
                Point::new([0.0, 600.0, -600.0]),
                Point::new([600.0, 600.0, 0.0]),
            ],
            Vector3::new([0.0, -1.0, 0.0]),
            Arc::new(white),
        )));
        // left
        self.add(Box::new(Panel::new(
            [
                Point::new([0.0, 0.0, -600.0]),
                Point::new([0.0, 600.0, 0.0]),
            ],
            Vector3::new([1.0, 0.0, 0.0]),
            Arc::new(green),
        )));
        // back
        self.add(Box::new(Panel::new(
            [
                Point::new([0.0, 0.0, -600.0]),
                Point::new([600.0, 600.0, -600.0]),
            ],
            Vector3::new([0.0, 0.0, 1.0]),
            Arc::new(white),
        )));
        // right
        self.add(Box::new(Panel::new(
            [
                Point::new([600.0, 0.0, -600.0]),
                Point::new([600.0, 600.0, 0.0]),
            ],
            Vector3::new([-1.0, 0.0, 0.0]),
            Arc::new(red),
        )));
        // bottom
        self.add(Box::new(Panel::new(
            [
                Point::new([0.0, 0.0, -600.0]),
                Point::new([600.0, 0.0, 0.0]),
            ],
            Vector3::new([0.0, 1.0, 0.0]),
            Arc::new(white),
        )));
        self.add(Box::new(Rectangle::new(
            [
                Point::new([130.0, 0.0, -530.0]),
                Point::new([300.0, 170.0, -350.0]),
            ],
            None,
            Arc::new(white),
        )));
        self.add(Box::new(Rectangle::new(
            [
                Point::new([170.0, 0.0, -300.0]),
                Point::new([430.0, 330.0, -150.0]),
            ],
            None,
            Arc::new(white),
        )));
    }
}
