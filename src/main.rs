//
//      @author: 张峻魁 | Junkui Zhang
//      @email:  junkuizhangchina@gmail.com
//      @date:   2021, Jul
//
use world::World;

mod camera;
mod data;
mod entity;
mod material;
mod settings;
mod some_math;
mod systems;
mod world;

fn main() {
    let mut world = World::new();
    world.default_scene();
    world.shade_pixel();
    world.save_image();
}
