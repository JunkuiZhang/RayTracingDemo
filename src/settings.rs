pub const WINDOW_HEIGHT: u32 = 600;
pub const WINDOW_WIDTH: u32 = ((WINDOW_HEIGHT as f64) * ASPECT_RATIO) as u32;
pub const CAMERA_HEIGHT: f64 = 2.0;
pub const ASPECT_RATIO: f64 = 1.0 / 1.0;
pub const FOV: f64 = 40.0;
pub const SAMPLES_PER_PIXEL: usize = 10;
pub const RAY_DEPTH: i32 = 20;
pub const THREAD_NUM: usize = 4;
