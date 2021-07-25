use std::{sync::Arc, time::SystemTime, u32};

use image::{ImageBuffer, Rgb};
use rand::prelude::ThreadRng;

use crate::{
    camera::Camera,
    data::PixelContainer,
    entity::{obj_traits::Hittable, Panel, Rectangle, Sphere},
    material::{DiffuseLight, DiffuseMat, Glass, Metal},
    settings::{SAMPLES_PER_PIXEL, THREAD_NUM, WINDOW_HEIGHT, WINDOW_WIDTH},
    some_math::{generate_neighbor_pixel_coordinate, num_inline, Color, Point, Vector3},
    world::multithread_impl::ThreadPool,
};

use super::World;

impl World {
    pub fn new() -> Self {
        World {
            start_time: SystemTime::now(),
            objects: Vec::new(),
            lights: Vec::new(),
            camera: Camera::default(),
            rng: ThreadRng::default(),
        }
    }

    pub fn run(&mut self) {
        self.start_time = SystemTime::now();
        let raw_pixel = self.shade_pixel();
        let proc_pixel = self.outlier_removal(&raw_pixel);
    }

    fn shade_pixel(&mut self) -> PixelContainer {
        println!("==> Starting shading...");
        let c = Arc::new(self.camera);
        let o = Arc::new(self.objects.clone());
        let l = Arc::new(self.lights.clone());
        let thread_pool = ThreadPool::new(THREAD_NUM, c, o, l);
        for job in 0..WINDOW_HEIGHT {
            thread_pool.work(job);
        }
        let mut res = PixelContainer::new();
        let mut num = 0;
        let mut last_portion = 0;
        'job_loop: loop {
            if let Ok(job_res) = thread_pool.result.recv() {
                res.data[job_res.0 as usize] = job_res.1;
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
        thread_pool.shut_down();
        let image_buffer =
            ImageBuffer::<Rgb<u8>, Vec<u8>>::from_vec(WINDOW_WIDTH, WINDOW_HEIGHT, res.to_pixels())
                .unwrap();
        println!("Saving raw image..");
        image_buffer
            .save(format!("{}SPP.png", SAMPLES_PER_PIXEL))
            .unwrap();
        println!(
            "Ray tracing time cost: {}",
            SystemTime::now()
                .duration_since(self.start_time)
                .unwrap()
                .as_secs()
        );
        return res;
    }

    fn outlier_removal(&self, raw_data: &PixelContainer) -> PixelContainer {
        println!("==> Removing outlier");
        let mut res_vec = PixelContainer::new();
        for row_num in 0..WINDOW_HEIGHT as usize {
            for col_num in 0..WINDOW_WIDTH as usize {
                let mut r_vec = Vec::new();
                let mut g_vec = Vec::new();
                let mut b_vec = Vec::new();
                for (col, row) in generate_neighbor_pixel_coordinate(col_num, row_num) {
                    r_vec.push(raw_data.data[row][col * 3]);
                    g_vec.push(raw_data.data[row][col * 3 + 1]);
                    b_vec.push(raw_data.data[row][col * 3 + 2]);
                }
                res_vec.data[row_num][col_num * 3] =
                    num_inline(&r_vec, raw_data.data[row_num][col_num * 3]);
                res_vec.data[row_num][col_num * 3 + 1] =
                    num_inline(&g_vec, raw_data.data[row_num][col_num * 3 + 1]);
                res_vec.data[row_num][col_num * 3 + 2] =
                    num_inline(&b_vec, raw_data.data[row_num][col_num * 3 + 2]);
            }
        }
        let image_buffer = ImageBuffer::<Rgb<u8>, Vec<u8>>::from_vec(
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            res_vec.to_pixels(),
        )
        .unwrap();
        println!("Saving processed image..");
        image_buffer
            .save(format!("{}SPP-outlier-removed.png", SAMPLES_PER_PIXEL))
            .unwrap();
        println!(
            "Outlier removal time cost: {}",
            SystemTime::now()
                .duration_since(self.start_time)
                .unwrap()
                .as_secs()
        );
        return res_vec;
    }

    fn add(&mut self, obj: Arc<dyn Hittable + Send + Sync>) {
        self.objects.push(obj);
    }

    pub fn default_scene(&mut self) {
        let red = DiffuseMat::new(Color::new([0.65, 0.05, 0.05]));
        let white = DiffuseMat::new(Color::new([0.75, 0.75, 0.75]));
        let green = DiffuseMat::new(Color::new([0.12, 0.45, 0.15]));
        let cupper = Metal::new(Color::new([0.7, 0.45, 0.2]), 0.5);
        let glass = Glass::new(Color::new([0.9, 0.9, 0.9]), 1.5);
        let light = DiffuseLight::new(Color::new([7.0, 7.0, 7.0]));
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
                Point::new([110.0, 0.0, -460.0]),
                Point::new([280.0, 330.0, -280.0]),
            ],
            Some(10.0),
            Arc::new(white),
            // Arc::new(blue),
        )));
        self.add(Arc::new(Rectangle::new(
            [
                Point::new([350.0, 0.0, -270.0]),
                Point::new([500.0, 150.0, -120.0]),
            ],
            Some(-5.0),
            // None,
            // Arc::new(white),
            Arc::new(cupper),
        )));
        self.add(Arc::new(Sphere::new(
            Point::new([150.0, 60.0, -160.0]),
            60.0,
            Arc::new(glass),
        )));
        self.camera = Camera::new(
            Point::new([300.0, 300.0, 800.0]),
            Vector3::new([0.0, 0.0, -1.0]),
            Vector3::new([0.0, 1.0, 0.0]),
        );
    }
}
