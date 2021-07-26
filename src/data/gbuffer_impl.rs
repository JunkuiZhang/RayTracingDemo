use crate::settings::{WINDOW_HEIGHT, WINDOW_WIDTH};

use super::{FilterType, GBInfo, GeometryBuffer, RowColGBuffer};

impl GeometryBuffer {
    pub fn new() -> Self {
        // row container by default
        let mut data = Vec::with_capacity(WINDOW_HEIGHT as usize);
        for _ in 0..WINDOW_HEIGHT {
            data.push(RowColGBuffer::new_empty(FilterType::Row))
        }
        GeometryBuffer { data }
    }

    pub fn set_row(&mut self, row_num: usize, row_data: RowColGBuffer) {
        self.data[row_num] = row_data;
    }

    pub fn get_x_or_y(&self, row_col_num: usize, indicator: FilterType) -> RowColGBuffer {
        match indicator {
            FilterType::Row => RowColGBuffer {
                data: self.data[row_col_num].data.clone(),
            },
            FilterType::Col => {
                let mut data = Vec::with_capacity(WINDOW_HEIGHT as usize);
                for row in self.data.iter() {
                    data.push(*row.get_data(row_col_num));
                }
                return RowColGBuffer { data };
            }
        }
    }
}

impl RowColGBuffer {
    pub fn new_empty(filter_type: FilterType) -> Self {
        match filter_type {
            FilterType::Row => RowColGBuffer {
                data: Vec::with_capacity(WINDOW_WIDTH as usize),
            },
            FilterType::Col => RowColGBuffer {
                data: Vec::with_capacity(WINDOW_HEIGHT as usize),
            },
        }
    }

    pub fn get_data(&self, index: usize) -> &GBInfo {
        &self.data[index]
    }

    pub fn push_data(&mut self, data: GBInfo) {
        self.data.push(data);
    }
}
