use crate::settings::{WINDOW_HEIGHT, WINDOW_WIDTH};

use super::{GBInfo, GeometryBuffer, GeometryColBuffer, GeometryRowBuffer};

impl GeometryBuffer {
    pub fn new() -> Self {
        let mut data = Vec::with_capacity(WINDOW_HEIGHT as usize);
        for _ in 0..WINDOW_HEIGHT {
            data.push(GeometryRowBuffer::new_empty())
        }
        GeometryBuffer { data }
    }

    pub fn get_row_content(&self, row_num: usize) -> &GeometryRowBuffer {
        &self.data[row_num]
    }

    pub fn get_col_content(&self, col_num: usize) -> GeometryColBuffer {
        let mut data = Vec::with_capacity(WINDOW_HEIGHT as usize);
        for row in self.data.iter() {
            data.push(*row.get_data(col_num));
        }
        return GeometryColBuffer { data };
    }

    pub fn set_row(&mut self, row_num: usize, row_data: GeometryRowBuffer) {
        self.data[row_num] = row_data;
    }
}

impl GeometryRowBuffer {
    pub fn new_empty() -> Self {
        GeometryRowBuffer {
            data: Vec::with_capacity(WINDOW_WIDTH as usize),
        }
    }

    pub fn get_data(&self, index: usize) -> &GBInfo {
        &self.data[index]
    }

    pub fn push_data(&mut self, data: GBInfo) {
        self.data.push(data);
    }
}

impl GeometryColBuffer {
    pub fn get_data(&self, index: usize) -> &GBInfo {
        &self.data[index]
    }
}
