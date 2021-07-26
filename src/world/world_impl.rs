use std::{
    sync::{Arc, RwLock},
    time::SystemTime,
    u32,
};

use image::{ImageBuffer, Rgb};

use crate::{
    camera::Camera,
    data::{GeometryBuffer, PixelContainer},
    entity::{obj_traits::Hittable, Panel, Rectangle, Sphere},
    material::{DiffuseLight, DiffuseMat, Glass, Metal},
    settings::{SAMPLES_PER_PIXEL, THREAD_NUM, WINDOW_HEIGHT, WINDOW_WIDTH},
    some_math::{
        generate_neighbor_pixel_coordinate, generate_num_sequence, num_inline, sum_vector_list,
        Color, Point, Vector3,
    },
    systems::image_process::pixel_filter,
    world::multithread_impl::ThreadPool,
};

use super::World;

impl World {
    pub fn new() -> Self {
        World {
            start_time: SystemTime::now(),
            last_end_time: SystemTime::now(),
            objects: Arc::new(RwLock::new(Vec::new())),
            lights: Arc::new(RwLock::new(Vec::new())),
            camera: Arc::new(Camera::default()),
        }
    }

    pub fn run(&mut self) {
        self.start_time = SystemTime::now();
        let (raw_pixel, gbuffer) = self.shade_pixel();
        let proc_pixel = self.outlier_removal(raw_pixel);
        self.denoising(proc_pixel, gbuffer);
    }

    fn shade_pixel(&mut self) -> (PixelContainer, GeometryBuffer) {
        // fn shade_pixel(&mut self) {
        println!("==> Starting shading...");
        let thread_pool = ThreadPool::new(
            THREAD_NUM,
            self.camera.clone(),
            self.objects.clone(),
            self.lights.clone(),
        );
        for job in 0..WINDOW_HEIGHT {
            thread_pool.work(job);
        }
        let res = self.res_process(&thread_pool);
        thread_pool.shut_down();
        let image_buffer = ImageBuffer::<Rgb<u8>, Vec<u8>>::from_vec(
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            res.0.to_pixels(),
        )
        .unwrap();
        println!("Saving raw image..");
        image_buffer
            .save(format!("{}SPP.png", SAMPLES_PER_PIXEL))
            .unwrap();
        let t_end = SystemTime::now();
        println!(
            "Ray tracing time cost: {}",
            t_end.duration_since(self.start_time).unwrap().as_secs()
        );
        self.last_end_time = t_end;
        return res;
    }

    fn res_process(&self, thread_pool: &ThreadPool) -> (PixelContainer, GeometryBuffer) {
        let mut pixel_res = PixelContainer::new();
        let mut gbuffer_res = GeometryBuffer::new();
        let mut num = 0;
        let mut last_portion = 0;
        'job_loop: loop {
            if let Ok(job_res) = thread_pool.result.recv() {
                let row_num = job_res.0 as usize;
                let row_content = job_res.1.clone();
                let gb_row_content = job_res.2.clone();
                pixel_res.set_row(row_num, row_content);
                gbuffer_res.set_row(row_num, gb_row_content);
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
        return (pixel_res, gbuffer_res);
    }

    fn outlier_removal(&self, raw_data: PixelContainer) -> PixelContainer {
        println!("==> Removing outlier");
        let mut res_vec = PixelContainer::new();
        for row_num in 0..WINDOW_HEIGHT as usize {
            for col_num in 0..WINDOW_WIDTH as usize {
                let mut colors_vec = Vec::new();
                for (col, row) in generate_neighbor_pixel_coordinate(col_num, row_num) {
                    colors_vec.push(raw_data.get_colors(col, row));
                }
                res_vec.set_colors(
                    col_num,
                    row_num,
                    num_inline(&colors_vec, raw_data.get_colors(col_num, row_num)),
                );
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
        let t_end = SystemTime::now();
        println!(
            "Outlier removal time cost: {}, total cost: {}",
            t_end.duration_since(self.last_end_time).unwrap().as_secs(),
            t_end.duration_since(self.start_time).unwrap().as_secs()
        );
        return res_vec;
    }

    fn denoising(&self, processed_img: PixelContainer, gbuffer: GeometryBuffer) {
        println!("==> Filtering...");
        let mut res_vec = PixelContainer::new();
        // row process
        for row_num in 0..WINDOW_HEIGHT as usize {
            let row_pixels = processed_img.get_row_content(row_num);
            let row_gbuffer = gbuffer.get_row_content(row_num);
            for col_num in 0..WINDOW_WIDTH as usize {
                let sample_points = generate_num_sequence(col_num, 4);
                let gb0 = row_gbuffer.get_data(col_num);
                let c0 = row_pixels.get_color(col_num);

                let mut color_vec = Vec::new();
                color_vec.push(c0);
                for temp_sample in sample_points.iter() {
                    color_vec.push(row_pixels.get_color(*temp_sample));
                }
                let temp_l = color_vec.len();
                let color_mean = sum_vector_list(&color_vec) / temp_l as f64;
                let color_sigma = (color_vec
                    .iter()
                    .map(|c| (*c - color_mean).length_square())
                    .sum::<f64>()
                    / temp_l as f64)
                    .sqrt();

                let mut weights = 1.0;
                let mut res_pixel = c0.clone();
                for sample in sample_points {
                    let c1 = row_pixels.get_color(sample);
                    let gb1 = row_gbuffer.get_data(sample);
                    let w = pixel_filter(gb0, gb1, c0, c1, color_sigma);
                    weights += w;
                    res_pixel += w * c1;
                }
                if weights > 0.0 {
                    res_pixel /= weights;
                }
                res_vec.set_colors(col_num, row_num, res_pixel.data);
            }
        }

        let image_buffer = ImageBuffer::<Rgb<u8>, Vec<u8>>::from_vec(
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            res_vec.to_pixels(),
        )
        .unwrap();
        println!("Saving filtered image..");
        image_buffer
            .save(format!("{}SPP-outlier-filtered.png", SAMPLES_PER_PIXEL))
            .unwrap();
        let t_end = SystemTime::now();
        println!(
            "Image filter time cost: {}, total cost: {}",
            t_end.duration_since(self.last_end_time).unwrap().as_secs(),
            t_end.duration_since(self.start_time).unwrap().as_secs()
        );
    }

    fn add(&mut self, obj: Arc<dyn Hittable + Send + Sync>) {
        self.objects.write().unwrap().push(obj);
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
        self.lights.write().unwrap().push(Arc::new(Panel::new(
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
            Arc::new(white),
            // Arc::new(cupper),
        )));
        // self.add(Arc::new(Sphere::new(
        //     Point::new([150.0, 60.0, -160.0]),
        //     60.0,
        //     Arc::new(glass),
        // )));
        self.camera = Arc::new(Camera::new(
            Point::new([300.0, 300.0, 800.0]),
            Vector3::new([0.0, 0.0, -1.0]),
            Vector3::new([0.0, 1.0, 0.0]),
        ));
    }
}
