use glam::Mat4;

use super::{MaterialAsset, MaterialKind, MeshPrimitive, SceneAsset, SceneInstance, VertexAsset};

pub fn create() -> SceneAsset {
    let materials = vec![
        MaterialAsset::opaque("Cornell white", [0.75, 0.75, 0.75, 1.0]),
        MaterialAsset::opaque("Cornell red", [0.65, 0.05, 0.05, 1.0]),
        MaterialAsset::opaque("Cornell green", [0.12, 0.45, 0.15, 1.0]),
        MaterialAsset {
            name: "Cornell area light".to_string(),
            base_color_factor: [1.0; 4],
            metallic_factor: 0.0,
            roughness_factor: 1.0,
            normal_scale: 1.0,
            emissive_factor: [7.0; 3],
            ior: 1.5,
            kind: MaterialKind::Emissive,
            double_sided: false,
            base_color_texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            emissive_texture: None,
        },
        MaterialAsset {
            name: "Cornell metal".to_string(),
            base_color_factor: [0.82, 0.85, 0.9, 1.0],
            metallic_factor: 1.0,
            roughness_factor: 0.05,
            normal_scale: 1.0,
            emissive_factor: [0.0; 3],
            ior: 1.5,
            kind: MaterialKind::LegacyMetal,
            double_sided: false,
            base_color_texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            emissive_texture: None,
        },
        MaterialAsset {
            name: "Cornell glass".to_string(),
            base_color_factor: [0.98, 0.98, 0.98, 1.0],
            metallic_factor: 0.0,
            roughness_factor: 0.0,
            normal_scale: 1.0,
            emissive_factor: [0.0; 3],
            ior: 1.5,
            kind: MaterialKind::LegacyDielectric,
            double_sided: false,
            base_color_texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            emissive_texture: None,
        },
    ];
    let mut primitives = Vec::new();
    let mut instances = Vec::new();
    let mut add =
        |name: &str, positions: [[f32; 3]; 4], normal: [f32; 3], material_index: usize| {
            let primitive_index = primitives.len();
            primitives.push(MeshPrimitive {
                name: name.to_string(),
                vertices: positions
                    .into_iter()
                    .enumerate()
                    .map(|(index, position)| VertexAsset {
                        position,
                        normal,
                        tangent: [1.0, 0.0, 0.0, 1.0],
                        texcoord0: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]][index],
                        has_tangent: false,
                    })
                    .collect(),
                indices: vec![0, 1, 2, 0, 2, 3],
                material_index,
            });
            let matrix = Mat4::IDENTITY;
            instances.push(SceneInstance {
                stable_id: instances.len() as u32,
                primitive_index,
                base_world: matrix,
                current_world: matrix,
                previous_world: matrix,
            });
        };

    add(
        "floor",
        [
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, -1.0, 2.0],
            [-1.0, -1.0, 2.0],
        ],
        [0.0, 1.0, 0.0],
        0,
    );
    add(
        "ceiling",
        [
            [-1.0, 1.0, 2.0],
            [1.0, 1.0, 2.0],
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
        ],
        [0.0, -1.0, 0.0],
        0,
    );
    add(
        "back",
        [
            [-1.0, -1.0, 2.0],
            [1.0, -1.0, 2.0],
            [1.0, 1.0, 2.0],
            [-1.0, 1.0, 2.0],
        ],
        [0.0, 0.0, -1.0],
        0,
    );
    add(
        "left",
        [
            [-1.0, -1.0, 0.0],
            [-1.0, -1.0, 2.0],
            [-1.0, 1.0, 2.0],
            [-1.0, 1.0, 0.0],
        ],
        [1.0, 0.0, 0.0],
        2,
    );
    add(
        "right",
        [
            [1.0, -1.0, 2.0],
            [1.0, -1.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 1.0, 2.0],
        ],
        [-1.0, 0.0, 0.0],
        1,
    );
    add(
        "area light",
        [
            [-0.25, 0.9966667, 0.6666667],
            [0.25, 0.9966667, 0.6666667],
            [0.25, 0.9966667, 1.1666666],
            [-0.25, 0.9966667, 1.1666666],
        ],
        [0.0, -1.0, 0.0],
        3,
    );
    add_box(
        &mut add,
        "metal box",
        [-0.6333333, -1.0, 0.93333334],
        [-0.06666667, 0.1, 1.5333333],
        (-10.0_f32).to_radians(),
        4,
    );
    add_box(
        &mut add,
        "glass box",
        [0.16666667, -1.0, 0.4],
        [0.6666667, -0.5, 0.9],
        5.0_f32.to_radians(),
        5,
    );

    SceneAsset {
        primitives,
        materials,
        images: Vec::new(),
        instances,
        rigid_animation_groups: Vec::new(),
    }
}

fn add_box(
    add: &mut impl FnMut(&str, [[f32; 3]; 4], [f32; 3], usize),
    name: &str,
    min: [f32; 3],
    max: [f32; 3],
    angle: f32,
    material_index: usize,
) {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    let center = [(x0 + x1) * 0.5, (y0 + y1) * 0.5, (z0 + z1) * 0.5];
    let (sine, cosine) = angle.sin_cos();
    let transform = |position: [f32; 3]| {
        let x = position[0] - center[0];
        let z = position[2] - center[2];
        [
            center[0] + cosine * x + sine * z,
            position[1],
            center[2] - sine * x + cosine * z,
        ]
    };
    let faces = [
        (
            [[x0, y0, z0], [x1, y0, z0], [x1, y1, z0], [x0, y1, z0]],
            [0.0, 0.0, -1.0],
        ),
        (
            [[x1, y0, z1], [x0, y0, z1], [x0, y1, z1], [x1, y1, z1]],
            [0.0, 0.0, 1.0],
        ),
        (
            [[x0, y0, z1], [x0, y0, z0], [x0, y1, z0], [x0, y1, z1]],
            [-1.0, 0.0, 0.0],
        ),
        (
            [[x1, y0, z0], [x1, y0, z1], [x1, y1, z1], [x1, y1, z0]],
            [1.0, 0.0, 0.0],
        ),
        (
            [[x0, y1, z0], [x1, y1, z0], [x1, y1, z1], [x0, y1, z1]],
            [0.0, 1.0, 0.0],
        ),
        (
            [[x0, y0, z1], [x1, y0, z1], [x1, y0, z0], [x0, y0, z0]],
            [0.0, -1.0, 0.0],
        ),
    ];
    for (face_index, (positions, normal)) in faces.into_iter().enumerate() {
        let transformed_normal = [
            cosine * normal[0] + sine * normal[2],
            normal[1],
            -sine * normal[0] + cosine * normal[2],
        ];
        add(
            &format!("{name} face {face_index}"),
            positions.map(transform),
            transformed_normal,
            material_index,
        );
    }
}
