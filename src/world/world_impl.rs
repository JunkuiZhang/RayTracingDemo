use std::{sync::Arc, time::SystemTime, u32};

use image::{ImageBuffer, Rgb};
use rand::prelude::ThreadRng;

use crate::{
    camera::Camera,
    entity::{obj_traits::Hittable, Panel, Rectangle},
    material::{DiffuseLight, DiffuseMat},
    settings::{IMAGE_PATH, THREAD_NUM, WINDOW_HEIGHT, WINDOW_WIDTH},
    some_math::{Color, Point, Vector3},
    world::multithread_impl::ThreadPool,
};

use super::World;

impl World {
    pub fn new() -> Self {
        World {
            objects: Vec::new(),
            lights: Vec::new(),
            camera: Camera::default(),
            image_buffer: ImageBuffer::new(WINDOW_WIDTH, WINDOW_HEIGHT),
            rng: ThreadRng::default(),
        }
    }

    pub fn shade_pixel(&mut self) {
        let start_time = SystemTime::now();
        println!("{:?}", start_time);
        let c = Arc::new(self.camera);
        let o = Arc::new(self.objects.clone());
        let l = Arc::new(self.lights.clone());
        let thread_pool = ThreadPool::new(THREAD_NUM, c, o, l);
        for job in 0..WINDOW_HEIGHT {
            thread_pool.work(job);
        }
        let mut res = [[0u8; (WINDOW_WIDTH * 3) as usize]; WINDOW_HEIGHT as usize];
        let mut num = 0;
        let mut last_portion = 0;
        'job_loop: loop {
            if let Ok(job_res) = thread_pool.result.recv() {
                res[job_res.0 as usize] = job_res.1;
                num += 1;
                let portion = ((num as f64 / WINDOW_HEIGHT as f64) * 100.0) as u32;
                if portion > last_portion {
                    println!("{}% done.", portion);
                    last_portion = portion;
                }
            }
            if num == WINDOW_HEIGHT {
                break 'job_loop;
            }
        }
        for _ in 0..thread_pool.get_thread_num() {
            thread_pool.shut_down();
        }
        self.image_buffer =
            ImageBuffer::<Rgb<u8>, Vec<u8>>::from_vec(WINDOW_WIDTH, WINDOW_HEIGHT, res.concat())
                .unwrap();
        println!(
            "Total time cost: {}",
            SystemTime::now()
                .duration_since(start_time)
                .unwrap()
                .as_secs()
        );
    }

    pub fn save_image(&self) {
        println!("Saving image..");
        self.image_buffer.save(IMAGE_PATH).unwrap();
    }

    fn add(&mut self, obj: Arc<dyn Hittable + Send + Sync>) {
        self.objects.push(obj);
    }

    pub fn default_scene(&mut self) {
        let red = DiffuseMat::new(Color::new([0.65, 0.05, 0.05]));
        let white = DiffuseMat::new(Color::new([0.73, 0.73, 0.73]));
        let green = DiffuseMat::new(Color::new([0.12, 0.45, 0.15]));
        // let blue = DiffuseMat::new(Color::new([0.1, 0.2, 0.7]));
        let light = DiffuseLight::new(Color::new([5.0, 5.0, 5.0]));
        // light
        self.add(Arc::new(Panel::new(
            [
                Point::new([225.0, 599.0, -350.0]),
                Point::new([375.0, 599.0, -200.0]),
            ],
            Vector3::new([0.0, -1.0, 0.0]),
            Arc::new(light),
        )));
        self.lights.push(Arc::new(Panel::new(
            [
                Point::new([225.0, 599.0, -350.0]),
                Point::new([375.0, 599.0, -200.0]),
            ],
            Vector3::new([0.0, -1.0, 0.0]),
            Arc::new(light),
        )));
        // top
        self.add(Arc::new(Panel::new(
            [
                Point::new([0.0, 600.0, -600.0]),
                Point::new([600.0, 600.0, 0.0]),
            ],
            Vector3::new([0.0, -1.0, 0.0]),
            Arc::new(white),
        )));
        // left
        self.add(Arc::new(Panel::new(
            [
                Point::new([0.0, 0.0, -600.0]),
                Point::new([0.0, 600.0, 0.0]),
            ],
            Vector3::new([1.0, 0.0, 0.0]),
            Arc::new(green),
        )));
        // back
        self.add(Arc::new(Panel::new(
            [
                Point::new([0.0, 0.0, -600.0]),
                Point::new([600.0, 600.0, -600.0]),
            ],
            Vector3::new([0.0, 0.0, 1.0]),
            Arc::new(white),
        )));
        // right
        self.add(Arc::new(Panel::new(
            [
                Point::new([600.0, 0.0, -600.0]),
                Point::new([600.0, 600.0, 0.0]),
            ],
            Vector3::new([-1.0, 0.0, 0.0]),
            Arc::new(red),
        )));
        // bottom
        self.add(Arc::new(Panel::new(
            [
                Point::new([0.0, 0.0, -600.0]),
                Point::new([600.0, 0.0, 0.0]),
            ],
            Vector3::new([0.0, 1.0, 0.0]),
            Arc::new(white),
        )));
        self.add(Arc::new(Rectangle::new(
            [
                Point::new([130.0, 0.0, -530.0]),
                Point::new([300.0, 330.0, -350.0]),
            ],
            Some(15.0),
            Arc::new(white),
            // Arc::new(blue),
        )));
        self.add(Arc::new(Rectangle::new(
            [
                Point::new([300.0, 0.0, -300.0]),
                Point::new([450.0, 150.0, -150.0]),
            ],
            Some(-10.0),
            // None,
            Arc::new(white),
            // Arc::new(blue),
        )));
        self.camera = Camera::new(
            Point::new([300.0, 300.0, 800.0]),
            Vector3::new([0.0, 0.0, -1.0]),
            Vector3::new([0.0, 1.0, 0.0]),
        );
    }
}
