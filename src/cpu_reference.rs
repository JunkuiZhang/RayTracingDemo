use std::{error::Error, fs, path::PathBuf};

use crate::{settings::DEFAULT_REFERENCE_SEED, world::World};

/// CPU 参考渲染的运行参数。
pub struct CpuReferenceConfig {
    pub output_dir: PathBuf,
    pub samples_per_pixel: usize,
    pub seed: u64,
    pub denoise: bool,
}

impl Default for CpuReferenceConfig {
    fn default() -> Self {
        Self {
            output_dir: PathBuf::from("output/cpu-reference"),
            samples_per_pixel: 1,
            seed: DEFAULT_REFERENCE_SEED,
            denoise: true,
        }
    }
}

/// 运行保留的 CPU 路径追踪器，供后续 GPU 实现对照结果。
pub fn run(config: CpuReferenceConfig) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(&config.output_dir)?;
    println!(
        "CPU 参考渲染：{} SPP，种子 0x{:016X}，输出目录 {}",
        config.samples_per_pixel,
        config.seed,
        config.output_dir.display()
    );

    let mut world = World::new(
        config.output_dir,
        config.samples_per_pixel,
        config.seed,
        config.denoise,
    );
    world.default_scene();
    world.run();
    Ok(())
}
