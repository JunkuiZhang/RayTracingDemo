use std::{
    sync::{Arc, RwLock},
    time::SystemTime,
    u32,
};

use image::{ImageBuffer, Rgb};

use crate::{
    camera::Camera,
    data::{GeometryBuffer, PixelContainer},
    entity::{obj_traits::Hittable, Panel, Rectangle},
    material::{DiffuseLight, DiffuseMat},
    settings::{FILTER_STEP, SAMPLES_PER_PIXEL, THREAD_NUM, WINDOW_HEIGHT, WINDOW_WIDTH},
    some_math::{generate_neighbor_pixel_coordinate, num_inline, Color, Point, Vector3},
    systems::image_process::{is_same_surface, luminance, pixel_filter},
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
        let clipped_pixel = self.outlier_removal(raw_pixel, &gbuffer, 1);
        self.atrous_filter(clipped_pixel, &gbuffer);
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

        self.save_image(&res.0, "origin-img".to_string(), 0);
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

    fn outlier_removal(
        &mut self,
        raw_data: PixelContainer,
        gbuffer: &GeometryBuffer,
        indicator: usize,
    ) -> PixelContainer {
        println!("==> Removing outlier");
        let mut res_vec = PixelContainer::new();
        for row_num in 0..WINDOW_HEIGHT as usize {
            for col_num in 0..WINDOW_WIDTH as usize {
                let mut colors_vec = Vec::new();
                let center_gbuffer = gbuffer.get_data(col_num, row_num);
                for (col, row) in generate_neighbor_pixel_coordinate(col_num, row_num) {
                    if is_same_surface(center_gbuffer, gbuffer.get_data(col, row)) {
                        colors_vec.push(raw_data.get_colors(col, row));
                    }
                }
                let center_color = raw_data.get_colors(col_num, row_num);
                // 小样本统计不可靠，此时保留原值比跨边界借用颜色更安全。
                let filtered_color = if colors_vec.len() >= 4 {
                    num_inline(&colors_vec, center_color)
                } else {
                    center_color
                };
                res_vec.set_colors(col_num, row_num, filtered_color);
            }
        }

        self.save_image(&res_vec, "outlier-removal".to_string(), indicator);
        return res_vec;
    }

    fn atrous_filter(
        &mut self,
        input_pixels: PixelContainer,
        gbuffer: &GeometryBuffer,
    ) -> PixelContainer {
        println!("==> 开始二维 À-Trous 滤波...");
        let mut current_pixels = input_pixels;
        for iteration in 0..FILTER_STEP {
            let step = 1 << iteration;
            current_pixels = self.atrous_iteration(&current_pixels, gbuffer, step);
        }
        self.save_image(&current_pixels, "a-trous-filter".to_string(), 2);
        current_pixels
    }

    fn atrous_iteration(
        &self,
        input_pixels: &PixelContainer,
        gbuffer: &GeometryBuffer,
        step: usize,
    ) -> PixelContainer {
        // B3 样条核在二维中做外积，既保持旋转对称，也避免横纵分离产生条纹。
        const KERNEL: [f64; 5] = [1.0, 4.0, 6.0, 4.0, 1.0];
        let mut result = PixelContainer::new();

        for row_num in 0..WINDOW_HEIGHT as usize {
            for col_num in 0..WINDOW_WIDTH as usize {
                let center_gbuffer = gbuffer.get_data(col_num, row_num);
                let center_color = Color::new(input_pixels.get_colors(col_num, row_num));
                let mut samples = Vec::with_capacity(25);
                let mut sample_colors = Vec::with_capacity(25);

                for row_offset in -2..=2 {
                    for col_offset in -2..=2 {
                        let Some(sample_col) =
                            offset_coordinate(col_num, col_offset, step, WINDOW_WIDTH as usize)
                        else {
                            continue;
                        };
                        let Some(sample_row) =
                            offset_coordinate(row_num, row_offset, step, WINDOW_HEIGHT as usize)
                        else {
                            continue;
                        };
                        let is_center = col_offset == 0 && row_offset == 0;
                        if !is_center
                            && !is_same_surface(
                                center_gbuffer,
                                gbuffer.get_data(sample_col, sample_row),
                            )
                        {
                            continue;
                        }

                        let kernel_weight =
                            KERNEL[(col_offset + 2) as usize] * KERNEL[(row_offset + 2) as usize];
                        let sample_color =
                            Color::new(input_pixels.get_colors(sample_col, sample_row));
                        samples.push((sample_col, sample_row, kernel_weight, sample_color));
                        sample_colors.push(sample_color);
                    }
                }

                let luminance_mean = sample_colors
                    .iter()
                    .map(|color| luminance(*color))
                    .sum::<f64>()
                    / sample_colors.len() as f64;
                let variance_divisor = sample_colors.len().saturating_sub(1).max(1) as f64;
                let color_sigma = (sample_colors
                    .iter()
                    .map(|color| (luminance(*color) - luminance_mean).powi(2))
                    .sum::<f64>()
                    / variance_divisor)
                    .sqrt();
                let mut total_weight = 0.0;
                let mut filtered_color = Color::BLACK;

                for (sample_col, sample_row, kernel_weight, sample_color) in samples {
                    let guide_weight = pixel_filter(
                        center_gbuffer,
                        gbuffer.get_data(sample_col, sample_row),
                        center_color,
                        sample_color,
                        color_sigma,
                    );
                    let combined_weight = kernel_weight * guide_weight;
                    total_weight += combined_weight;
                    filtered_color += combined_weight * sample_color;
                }

                if total_weight > 0.0 {
                    filtered_color /= total_weight;
                } else {
                    filtered_color = center_color;
                }
                result.set_colors(col_num, row_num, filtered_color.data);
            }
        }
        result
    }

    fn save_image(&mut self, res_vec: &PixelContainer, process_label: String, num: usize) {
        let image_buffer = ImageBuffer::<Rgb<u8>, Vec<u8>>::from_vec(
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            res_vec.to_pixels(),
        )
        .unwrap();
        println!("Saving {} image..", process_label);
        image_buffer
            .save(format!(
                "0{}-{}SPP-{}.png",
                num, SAMPLES_PER_PIXEL, process_label
            ))
            .unwrap();
        let t_end = SystemTime::now();
        println!(
            "Image {} time cost: {}, total cost: {}",
            process_label,
            t_end.duration_since(self.last_end_time).unwrap().as_secs(),
            t_end.duration_since(self.start_time).unwrap().as_secs()
        );
        self.last_end_time = t_end;
    }

    pub fn default_scene(&mut self) {
        let mut objs: Vec<Arc<dyn Hittable + Send + Sync>> = Vec::new();

        let red = DiffuseMat::new(Color::new([0.65, 0.05, 0.05]));
        let white = DiffuseMat::new(Color::new([0.75, 0.75, 0.75]));
        let green = DiffuseMat::new(Color::new([0.12, 0.45, 0.15]));
        // let cupper = Metal::new(Color::new([0.7, 0.45, 0.2]), 0.5);
        // let glass = Glass::new(Color::new([0.9, 0.9, 0.9]), 1.5);
        let light = DiffuseLight::new(Color::new([7.0, 7.0, 7.0]));
        // light
        let panel_light = Arc::new(Panel::new(
            [
                Point::new([225.0, 599.0, -350.0]),
                Point::new([375.0, 599.0, -200.0]),
            ],
            Vector3::new([0.0, -1.0, 0.0]),
            Arc::new(light),
            objs.len(),
        ));

        objs.push(panel_light.clone());
        self.lights.write().unwrap().push(panel_light);
        // top
        objs.push(Arc::new(Panel::new(
            [
                Point::new([0.0, 600.0, -600.0]),
                Point::new([600.0, 600.0, 0.0]),
            ],
            Vector3::new([0.0, -1.0, 0.0]),
            Arc::new(white),
            objs.len(),
        )));
        // left
        objs.push(Arc::new(Panel::new(
            [
                Point::new([0.0, 0.0, -600.0]),
                Point::new([0.0, 600.0, 0.0]),
            ],
            Vector3::new([1.0, 0.0, 0.0]),
            Arc::new(green),
            objs.len(),
        )));
        // back
        objs.push(Arc::new(Panel::new(
            [
                Point::new([0.0, 0.0, -600.0]),
                Point::new([600.0, 600.0, -600.0]),
            ],
            Vector3::new([0.0, 0.0, 1.0]),
            Arc::new(white),
            objs.len(),
        )));
        // right
        objs.push(Arc::new(Panel::new(
            [
                Point::new([600.0, 0.0, -600.0]),
                Point::new([600.0, 600.0, 0.0]),
            ],
            Vector3::new([-1.0, 0.0, 0.0]),
            Arc::new(red),
            objs.len(),
        )));
        // bottom
        objs.push(Arc::new(Panel::new(
            [
                Point::new([0.0, 0.0, -600.0]),
                Point::new([600.0, 0.0, 0.0]),
            ],
            Vector3::new([0.0, 1.0, 0.0]),
            Arc::new(white),
            objs.len(),
        )));
        objs.push(Arc::new(Rectangle::new(
            [
                Point::new([110.0, 0.0, -460.0]),
                Point::new([280.0, 330.0, -280.0]),
            ],
            Some(10.0),
            Arc::new(white),
            objs.len(),
            // Arc::new(blue),
        )));
        objs.push(Arc::new(Rectangle::new(
            [
                Point::new([350.0, 0.0, -270.0]),
                Point::new([500.0, 150.0, -120.0]),
            ],
            Some(-5.0),
            // None,
            Arc::new(white),
            objs.len(),
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
        self.objects = Arc::new(RwLock::new(objs));
    }
}

fn offset_coordinate(base: usize, offset: i32, step: usize, upper_bound: usize) -> Option<usize> {
    let coordinate = base as i32 + offset * step as i32;
    if coordinate < 0 || coordinate >= upper_bound as i32 {
        None
    } else {
        Some(coordinate as usize)
    }
}
