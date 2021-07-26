use std::sync::Arc;

use crate::{
    material::Material,
    settings::{WINDOW_HEIGHT, WINDOW_WIDTH},
    some_math::{to_u8, Color, Point, Vector3},
};

pub struct PixelContainer {
    data: Vec<RowPixels>,
}

#[derive(Debug, Clone)]
pub struct RowPixels {
    data: Vec<f64>,
}

#[derive(Clone)]
pub struct HitInfo {
    pub hit_point: Point,
    pub t: f64,
    pub normal: Vector3,
    pub material: Arc<dyn Material>,
}

#[derive(Debug, Clone, Copy)]
pub struct ScatterInfo {
    pub scatter_dir: Vector3,
    pub color: Color,
    pub pdf: f64,
}

impl PixelContainer {
    pub fn new() -> Self {
        let mut data = Vec::with_capacity(WINDOW_HEIGHT as usize);
        for _ in 0..WINDOW_HEIGHT {
            data.push(RowPixels::new());
        }
        return PixelContainer { data };
    }

    pub fn get_colors(&self, col_num: usize, row_num: usize) -> [f64; 3] {
        [
            self.data[row_num].get_value(col_num * 3),
            self.data[row_num].get_value(col_num * 3 + 1),
            self.data[row_num].get_value(col_num * 3 + 2),
        ]
    }

    pub fn set_colors(&mut self, col_num: usize, row_num: usize, colors: [f64; 3]) {
        self.data[row_num].set_color(col_num, colors);
    }

    pub fn set_row(&mut self, row_num: usize, row_content: RowPixels) {
        self.data[row_num] = row_content;
    }

    pub fn to_pixels(&self) -> Vec<u8> {
        let mut res = Vec::with_capacity(WINDOW_HEIGHT as usize);
        for row_pixel_f64 in self.data.iter() {
            let mut row_pixel_u8 = Vec::with_capacity((WINDOW_WIDTH * 3) as usize);
            for pixel_f64 in row_pixel_f64.data.iter() {
                row_pixel_u8.push(to_u8(pixel_f64));
            }
            res.push(row_pixel_u8);
        }
        return res.concat();
    }
}

impl RowPixels {
    pub fn new() -> Self {
        RowPixels {
            data: [0.0; (WINDOW_WIDTH * 3) as usize].to_vec(),
        }
    }

    pub fn get_value(&self, index: usize) -> f64 {
        self.data[index]
    }

    pub fn set_value(&mut self, index: usize, value: f64) {
        self.data[index] = value;
    }

    pub fn set_color(&mut self, col_num: usize, color: [f64; 3]) {
        self.set_value(col_num * 3, color[0]);
        self.set_value(col_num * 3 + 1, color[1]);
        self.set_value(col_num * 3 + 2, color[2]);
    }
}
