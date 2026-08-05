use crate::settings::{WINDOW_HEIGHT, WINDOW_WIDTH};

use super::{GBInfo, GeometryBuffer, RowColGBuffer};

impl GeometryBuffer {
    pub fn new() -> Self {
        // G-buffer 与颜色缓冲一致，始终按行存储。
        let mut data = Vec::with_capacity(WINDOW_HEIGHT as usize);
        for _ in 0..WINDOW_HEIGHT {
            data.push(RowColGBuffer::new_empty())
        }
        GeometryBuffer { data }
    }

    pub fn set_row(&mut self, row_num: usize, row_data: RowColGBuffer) {
        self.data[row_num] = row_data;
    }

    pub fn get_data(&self, col_num: usize, row_num: usize) -> &GBInfo {
        self.data[row_num].get_data(col_num)
    }
}

impl RowColGBuffer {
    pub fn new_empty() -> Self {
        RowColGBuffer {
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
