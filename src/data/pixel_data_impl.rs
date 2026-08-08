use crate::{
    settings::{WINDOW_HEIGHT, WINDOW_WIDTH},
    some_math::to_u8,
};

use super::{PixelContainer, RowColPixels};

impl PixelContainer {
    pub fn new() -> Self {
        // 图像始终按行存储，每行包含完整的 RGB 数据。
        let mut data = Vec::with_capacity(WINDOW_HEIGHT as usize);
        for _ in 0..WINDOW_HEIGHT {
            data.push(RowColPixels::new());
        }
        PixelContainer { data }
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

    pub fn set_row(&mut self, row_num: usize, row_content: RowColPixels) {
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
        res.concat()
    }
}

impl RowColPixels {
    pub fn new() -> Self {
        RowColPixels {
            data: [0.0; (WINDOW_WIDTH * 3) as usize].to_vec(),
        }
    }

    fn get_value(&self, index: usize) -> f64 {
        self.data[index]
    }

    pub fn set_color(&mut self, index: usize, color: [f64; 3]) {
        self.set_value(index * 3, color[0]);
        self.set_value(index * 3 + 1, color[1]);
        self.set_value(index * 3 + 2, color[2]);
    }

    fn set_value(&mut self, index: usize, value: f64) {
        self.data[index] = value;
    }
}
