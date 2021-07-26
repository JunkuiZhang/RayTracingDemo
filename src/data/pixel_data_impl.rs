use crate::{
    settings::{WINDOW_HEIGHT, WINDOW_WIDTH},
    some_math::{to_u8, Color},
};

use super::{FilterType, PixelContainer, RowColPixels};

impl PixelContainer {
    pub fn new() -> Self {
        // row container by default
        let mut data = Vec::with_capacity(WINDOW_HEIGHT as usize);
        for _ in 0..WINDOW_HEIGHT {
            data.push(RowColPixels::new(FilterType::Row));
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

    pub fn set_colors(&mut self, x: usize, y: usize, colors: [f64; 3], filter_type: FilterType) {
        match filter_type {
            FilterType::Row => self.data[y].set_color(x, colors),
            FilterType::Col => self.data[x].set_color(y, colors),
        }
    }

    pub fn get_x_or_y(&self, row_col_num: usize, indicator: FilterType) -> RowColPixels {
        match indicator {
            FilterType::Row => RowColPixels {
                data: self.data[row_col_num].data.clone(),
            },
            FilterType::Col => {
                let mut data = Vec::with_capacity((WINDOW_HEIGHT * 3) as usize);
                for row in self.data.iter() {
                    for num in row.get_color(row_col_num).data.iter() {
                        data.push(*num);
                    }
                }
                return RowColPixels { data };
            }
        }
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
        return res.concat();
    }
}

impl RowColPixels {
    pub fn new(indicator: FilterType) -> Self {
        match indicator {
            FilterType::Row => RowColPixels {
                data: [0.0; (WINDOW_WIDTH * 3) as usize].to_vec(),
            },
            FilterType::Col => RowColPixels {
                data: [0.0; (WINDOW_HEIGHT * 3) as usize].to_vec(),
            },
        }
    }

    pub fn get_color(&self, index: usize) -> Color {
        Color::new([
            self.get_value(index * 3),
            self.get_value(index * 3 + 1),
            self.get_value(index * 3 + 2),
        ])
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