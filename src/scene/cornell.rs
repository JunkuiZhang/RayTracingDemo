use glam::Mat4;

use super::{
    MaterialAsset, MaterialKind, MeshPrimitive, SamplerKey, SceneAsset, SceneInstance, VertexAsset,
};

pub const NESTED_DIELECTRIC_MIN_GAP: f32 = 0.05;
pub const NESTED_DIELECTRIC_OUTER_MIN: [f32; 3] = [0.10, -1.005, 0.35];
pub const NESTED_DIELECTRIC_OUTER_MAX: [f32; 3] = [0.72, -0.35, 1.05];
pub const NESTED_DIELECTRIC_INNER_MIN: [f32; 3] = [0.20, -0.88, 0.47];
pub const NESTED_DIELECTRIC_INNER_MAX: [f32; 3] = [0.62, -0.45, 0.93];
const NESTED_DIELECTRIC_ROTATION: f32 = 5.0_f32.to_radians();

pub fn create() -> SceneAsset {
    create_cornell(true)
}

fn create_cornell(include_right_glass: bool) -> SceneAsset {
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
            nested_priority: 1,
            thin_surface: false,
            absorption_coefficient: [0.0; 3],
            kind: MaterialKind::Emissive,
            // Keep the thin light card visible from either DXR face. Emission
            // sidedness is a separate shading contract: NEE and BSDF-hit
            // emission both use the fixed room-facing -Y light normal.
            double_sided: true,
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
            nested_priority: 1,
            thin_surface: false,
            absorption_coefficient: [0.0; 3],
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
            nested_priority: 1,
            thin_surface: false,
            absorption_coefficient: [0.0; 3],
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
                has_texcoord0: true,
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
    // Model the emitter as an actual opening in the ceiling. A full ceiling
    // plus a nearly coplanar light card creates a narrow occlusion cavity and
    // projects non-physical indirect-light shadows around the fixture.
    for (name, positions) in [
        (
            "ceiling left",
            [
                [-1.0, 1.0, 2.0],
                [-0.25, 1.0, 2.0],
                [-0.25, 1.0, 0.0],
                [-1.0, 1.0, 0.0],
            ],
        ),
        (
            "ceiling right",
            [
                [0.25, 1.0, 2.0],
                [1.0, 1.0, 2.0],
                [1.0, 1.0, 0.0],
                [0.25, 1.0, 0.0],
            ],
        ),
        (
            "ceiling back",
            [
                [-0.25, 1.0, 2.0],
                [0.25, 1.0, 2.0],
                [0.25, 1.0, 1.1666666],
                [-0.25, 1.0, 1.1666666],
            ],
        ),
        (
            "ceiling front",
            [
                [-0.25, 1.0, 0.6666667],
                [0.25, 1.0, 0.6666667],
                [0.25, 1.0, 0.0],
                [-0.25, 1.0, 0.0],
            ],
        ),
    ] {
        add(name, positions, [0.0, -1.0, 0.0], 0);
    }
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
            [-0.25, 1.0, 0.6666667],
            [0.25, 1.0, 0.6666667],
            [0.25, 1.0, 1.1666666],
            [-0.25, 1.0, 1.1666666],
        ],
        [0.0, -1.0, 0.0],
        3,
    );
    add_box(
        &mut add,
        "metal box",
        // Sink the opaque box slightly into the floor so its hidden bottom
        // face is not exactly coplanar with the floor. The visible side faces
        // still meet the floor without a floating gap, while DXR no longer
        // has two coincident triangles at the contact footprint.
        [-0.6333333, -1.005, 0.93333334],
        [-0.06666667, 0.1, 1.5333333],
        (-10.0_f32).to_radians(),
        4,
    );
    if include_right_glass {
        add_box(
            &mut add,
            "glass box",
            // Keep the closed dielectric bottom below the Cornell floor. A
            // coplanar bottom is an ambiguous zero-thickness medium boundary,
            // while lifting the box exposes a real bright gap. A slight embed
            // removes both cases and keeps the visible side/floor contact closed.
            [0.16666667, -1.005, 0.4],
            [0.6666667, -0.5, 0.9],
            5.0_f32.to_radians(),
            5,
        );
    }

    SceneAsset {
        primitives,
        materials,
        images: Vec::new(),
        samplers: vec![SamplerKey::default()],
        instances,
        rigid_animation_groups: Vec::new(),
    }
}

/// Cornell plus two independently closed, jointly rotated volumes. The inner
/// liquid is inset from the outer glass by more than the declared gap. Using
/// one rigid rotation preserves a directly testable containment relation while
/// keeping every pair of medium boundaries non-coplanar and camera-visible.
pub fn nested_dielectric() -> SceneAsset {
    // Start without Cornell's original right glass box. Appending the fixture
    // to the complete scene would overlap three unrelated solids (metal, old
    // glass and the new container), making medium diagnostics meaningless.
    let mut scene = create_cornell(false);
    let glass_material = scene.materials.len();
    scene.materials.push(MaterialAsset {
        name: "Nested outer glass".to_string(),
        base_color_factor: [0.98, 0.99, 1.0, 1.0],
        metallic_factor: 0.0,
        roughness_factor: 0.02,
        normal_scale: 1.0,
        emissive_factor: [0.0; 3],
        ior: 1.5,
        nested_priority: 1,
        thin_surface: false,
        absorption_coefficient: [0.025, 0.01, 0.008],
        kind: MaterialKind::LegacyDielectric,
        double_sided: false,
        base_color_texture: None,
        metallic_roughness_texture: None,
        normal_texture: None,
        emissive_texture: None,
    });
    let liquid_material = scene.materials.len();
    scene.materials.push(MaterialAsset {
        name: "Nested blue-green liquid".to_string(),
        base_color_factor: [0.12, 0.62, 0.72, 1.0],
        metallic_factor: 0.0,
        roughness_factor: 0.08,
        normal_scale: 1.0,
        emissive_factor: [0.0; 3],
        ior: 1.333,
        nested_priority: 2,
        thin_surface: false,
        absorption_coefficient: [0.015, 0.075, 0.11],
        kind: MaterialKind::LegacyDielectric,
        double_sided: false,
        base_color_texture: None,
        metallic_roughness_texture: None,
        normal_texture: None,
        emissive_texture: None,
    });

    let mut add =
        |name: &str, positions: [[f32; 3]; 4], normal: [f32; 3], material_index: usize| {
            let primitive_index = scene.primitives.len();
            scene.primitives.push(MeshPrimitive {
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
                has_texcoord0: true,
            });
            let matrix = Mat4::IDENTITY;
            scene.instances.push(SceneInstance {
                stable_id: scene.instances.len() as u32,
                primitive_index,
                base_world: matrix,
                current_world: matrix,
                previous_world: matrix,
            });
        };

    add_box(
        &mut add,
        "nested outer glass",
        NESTED_DIELECTRIC_OUTER_MIN,
        NESTED_DIELECTRIC_OUTER_MAX,
        NESTED_DIELECTRIC_ROTATION,
        glass_material,
    );
    add_box(
        &mut add,
        "nested inner liquid",
        NESTED_DIELECTRIC_INNER_MIN,
        NESTED_DIELECTRIC_INNER_MAX,
        NESTED_DIELECTRIC_ROTATION,
        liquid_material,
    );
    scene
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
