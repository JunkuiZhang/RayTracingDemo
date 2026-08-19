#![allow(dead_code)]

use glam::Mat4;

pub mod cornell;
pub mod gltf_loader;

pub const MATERIAL_FLAG_DOUBLE_SIDED: u32 = 1 << 0;
pub const MATERIAL_FLAG_HAS_TANGENT: u32 = 1 << 1;
pub const MATERIAL_FLAG_LEGACY_DIELECTRIC: u32 = 1 << 2;
pub const MATERIAL_FLAG_LEGACY_METAL: u32 = 1 << 3;
pub const MATERIAL_FLAG_LEGACY_EMISSIVE: u32 = 1 << 4;
pub const MATERIAL_MEDIUM_PRIORITY_MASK: u32 = 0x0f;
pub const MATERIAL_MEDIUM_FLAG_THIN_SURFACE: u32 = 1 << 4;
pub const MAX_MATERIAL_NESTED_PRIORITY: u8 = 15;
pub const MAX_SCENE_SAMPLERS: usize = 64;
pub const TEXTURE_VIEW_BITS: u32 = 7;
pub const SAMPLER_INDEX_BITS: u32 = 6;
pub const MAX_PACKED_TEXTURE_VIEWS: usize = 1 << TEXTURE_VIEW_BITS;
pub const MAX_PACKED_SAMPLERS: usize = 1 << SAMPLER_INDEX_BITS;

pub fn pack_texture_and_sampler(texture_view: usize, sampler_index: usize) -> Result<u32, String> {
    if texture_view >= MAX_PACKED_TEXTURE_VIEWS {
        return Err(format!(
            "texture view {} 超过打包上限 {}",
            texture_view, MAX_PACKED_TEXTURE_VIEWS
        ));
    }
    if sampler_index >= MAX_PACKED_SAMPLERS {
        return Err(format!(
            "sampler {} 超过打包上限 {}",
            sampler_index, MAX_PACKED_SAMPLERS
        ));
    }
    Ok((texture_view as u32) | ((sampler_index as u32) << TEXTURE_VIEW_BITS))
}

pub fn unpack_texture_and_sampler(packed: u32) -> (usize, usize) {
    let texture_mask = (1_u32 << TEXTURE_VIEW_BITS) - 1;
    let sampler_mask = (1_u32 << SAMPLER_INDEX_BITS) - 1;
    (
        (packed & texture_mask) as usize,
        ((packed >> TEXTURE_VIEW_BITS) & sampler_mask) as usize,
    )
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FilterMode {
    Nearest,
    Linear,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WrapMode {
    Repeat,
    Clamp,
    Mirror,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SamplerKey {
    pub min_filter: FilterMode,
    pub mag_filter: FilterMode,
    pub wrap_u: WrapMode,
    pub wrap_v: WrapMode,
}

impl Default for SamplerKey {
    fn default() -> Self {
        Self {
            min_filter: FilterMode::Linear,
            mag_filter: FilterMode::Linear,
            wrap_u: WrapMode::Repeat,
            wrap_v: WrapMode::Repeat,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialKind {
    Opaque,
    LegacyMetal,
    LegacyDielectric,
    Emissive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureBindingAsset {
    pub image_index: usize,
    pub sampler_index: usize,
    pub texcoord_set: u32,
}

#[derive(Clone, Debug)]
pub struct MaterialAsset {
    pub name: String,
    pub base_color_factor: [f32; 4],
    pub metallic_factor: f32,
    pub roughness_factor: f32,
    pub normal_scale: f32,
    pub emissive_factor: [f32; 3],
    pub ior: f32,
    /// Zero is the asset-facing highest priority, matching the stable-plane
    /// interior-list contract. Ordinary closed glass should normally use one.
    pub nested_priority: u8,
    /// Thin sheets refract once and never change the path's interior list.
    pub thin_surface: bool,
    /// Beer-Lambert absorption per world-space distance unit.
    pub absorption_coefficient: [f32; 3],
    pub kind: MaterialKind,
    pub double_sided: bool,
    pub base_color_texture: Option<TextureBindingAsset>,
    pub metallic_roughness_texture: Option<TextureBindingAsset>,
    pub normal_texture: Option<TextureBindingAsset>,
    pub emissive_texture: Option<TextureBindingAsset>,
}

impl MaterialAsset {
    pub fn opaque(name: impl Into<String>, base_color_factor: [f32; 4]) -> Self {
        Self {
            name: name.into(),
            base_color_factor,
            metallic_factor: 0.0,
            roughness_factor: 1.0,
            normal_scale: 1.0,
            emissive_factor: [0.0; 3],
            ior: 1.5,
            nested_priority: 1,
            thin_surface: false,
            absorption_coefficient: [0.0; 3],
            kind: MaterialKind::Opaque,
            double_sided: false,
            base_color_texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            emissive_texture: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImageAsset {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub struct VertexAsset {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub tangent: [f32; 4],
    pub texcoord0: [f32; 2],
    pub has_tangent: bool,
}

#[derive(Clone, Debug)]
pub struct MeshPrimitive {
    pub name: String,
    pub vertices: Vec<VertexAsset>,
    pub indices: Vec<u32>,
    pub material_index: usize,
    pub has_texcoord0: bool,
}

#[derive(Clone, Debug)]
pub struct SceneInstance {
    pub stable_id: u32,
    pub primitive_index: usize,
    pub base_world: Mat4,
    pub current_world: Mat4,
    pub previous_world: Mat4,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RigidAnimationGroup {
    pub instance_indices: Vec<usize>,
    pub pivot_world: [f32; 3],
}

#[derive(Clone, Debug, Default)]
pub struct SceneAsset {
    pub primitives: Vec<MeshPrimitive>,
    pub materials: Vec<MaterialAsset>,
    pub images: Vec<ImageAsset>,
    pub samplers: Vec<SamplerKey>,
    pub instances: Vec<SceneInstance>,
    pub rigid_animation_groups: Vec<RigidAnimationGroup>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub tangent: [f32; 4],
    pub texcoord0: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuMaterial {
    pub base_color_factor: [f32; 4],
    pub emissive_factor: [f32; 3],
    pub metallic_factor: f32,
    pub roughness_factor: f32,
    pub normal_scale: f32,
    pub ior: f32,
    pub flags: u32,
    pub base_color_texture_and_sampler: u32,
    pub metallic_roughness_texture_and_sampler: u32,
    pub normal_texture_and_sampler: u32,
    pub emissive_texture_and_sampler: u32,
    pub absorption_coefficient: [f32; 3],
    pub medium_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InstanceGpu {
    pub previous_object_to_world_row0: [f32; 4],
    pub previous_object_to_world_row1: [f32; 4],
    pub previous_object_to_world_row2: [f32; 4],
    pub vertex_offset: u32,
    pub index_offset: u32,
    pub material_index: u32,
    pub stable_surface_id: u32,
}

impl SceneAsset {
    pub fn cornell_box() -> Self {
        cornell::create()
    }

    pub fn nested_dielectric_fixture() -> Self {
        cornell::nested_dielectric()
    }

    pub fn append(&mut self, mut other: Self) -> Vec<usize> {
        let primitive_offset = self.primitives.len();
        let material_offset = self.materials.len();
        let image_offset = self.images.len();
        let sampler_offset = self.samplers.len();
        for material in &mut other.materials {
            for texture in [
                &mut material.base_color_texture,
                &mut material.metallic_roughness_texture,
                &mut material.normal_texture,
                &mut material.emissive_texture,
            ] {
                if let Some(binding) = texture.as_mut() {
                    binding.image_index += image_offset;
                    binding.sampler_index += sampler_offset;
                }
            }
        }
        for primitive in &mut other.primitives {
            primitive.material_index += material_offset;
        }
        let instance_offset = self.instances.len();
        let stable_id_base = self
            .instances
            .iter()
            .map(|instance| instance.stable_id)
            .max()
            .map_or(0, |maximum| maximum.saturating_add(1));
        for (local_index, instance) in other.instances.iter_mut().enumerate() {
            instance.primitive_index += primitive_offset;
            instance.stable_id = stable_id_base.saturating_add(local_index as u32);
        }
        let animated_groups = other
            .rigid_animation_groups
            .into_iter()
            .map(|mut group| {
                for index in &mut group.instance_indices {
                    *index += instance_offset;
                }
                group
            })
            .collect::<Vec<_>>();
        self.primitives.extend(other.primitives);
        self.materials.extend(other.materials);
        self.images.extend(other.images);
        self.samplers.extend(other.samplers);
        self.instances.extend(other.instances);
        self.rigid_animation_groups.extend(animated_groups);
        (instance_offset..self.instances.len()).collect()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.primitives.is_empty() {
            return Err("场景没有可渲染的 mesh primitive".to_string());
        }
        if self.materials.is_empty() {
            return Err("场景没有材质".to_string());
        }
        if self.samplers.is_empty() {
            return Err("场景没有 sampler（至少需要缺省 sampler）".to_string());
        }
        if self.samplers.len() > MAX_SCENE_SAMPLERS {
            return Err(format!(
                "场景 sampler 数 {} 超过上限 {MAX_SCENE_SAMPLERS}",
                self.samplers.len()
            ));
        }
        for (material_index, material) in self.materials.iter().enumerate() {
            if !material.ior.is_finite() || material.ior <= 0.0 {
                return Err(format!(
                    "material {material_index} 的 IOR 必须为有限正数"
                ));
            }
            if material.nested_priority > MAX_MATERIAL_NESTED_PRIORITY {
                return Err(format!(
                    "material {material_index} 的 nested_priority {} 超过上限 {MAX_MATERIAL_NESTED_PRIORITY}",
                    material.nested_priority
                ));
            }
            if !material
                .absorption_coefficient
                .iter()
                .all(|value| value.is_finite() && *value >= 0.0)
            {
                return Err(format!(
                    "material {material_index} 的 absorption_coefficient 必须为有限非负数"
                ));
            }
            if material.thin_surface && material.kind != MaterialKind::LegacyDielectric {
                return Err(format!(
                    "material {material_index} 只有 dielectric 才能声明 thin_surface"
                ));
            }
            for (texture_name, texture_index) in [
                ("base_color", material.base_color_texture),
                ("metallic_roughness", material.metallic_roughness_texture),
                ("normal", material.normal_texture),
                ("emissive", material.emissive_texture),
            ] {
                if let Some(binding) = texture_index
                    && binding.image_index >= self.images.len()
                {
                    return Err(format!(
                        "material {material_index} 的 {texture_name} image {} 越界（图片数 {}）",
                        binding.image_index,
                        self.images.len()
                    ));
                }
                if let Some(binding) = texture_index
                    && binding.sampler_index >= self.samplers.len()
                {
                    return Err(format!(
                        "material {material_index} 的 {texture_name} sampler {} 越界（sampler 数 {}）",
                        binding.sampler_index,
                        self.samplers.len()
                    ));
                }
                if let Some(binding) = texture_index
                    && binding.texcoord_set != 0
                {
                    return Err(format!(
                        "material {material_index} 的 {texture_name} 使用不支持的 texCoord {}，本阶段只支持 TEXCOORD_0",
                        binding.texcoord_set
                    ));
                }
            }
        }
        for (primitive_index, primitive) in self.primitives.iter().enumerate() {
            if primitive.vertices.is_empty() {
                return Err(format!("primitive {primitive_index} 没有顶点"));
            }
            if primitive.indices.len() % 3 != 0 {
                return Err(format!(
                    "primitive {primitive_index} 的 index 数量 {} 不是三角形的倍数",
                    primitive.indices.len()
                ));
            }
            if primitive.material_index >= self.materials.len() {
                return Err(format!(
                    "primitive {primitive_index} 引用越界材质 {}（材质数 {}）",
                    primitive.material_index,
                    self.materials.len()
                ));
            }
            let material = &self.materials[primitive.material_index];
            if !primitive.has_texcoord0
                && [
                    material.base_color_texture,
                    material.metallic_roughness_texture,
                    material.normal_texture,
                    material.emissive_texture,
                ]
                .into_iter()
                .any(|binding| binding.is_some())
            {
                return Err(format!(
                    "primitive {primitive_index} 的材质引用纹理但缺少 TEXCOORD_0"
                ));
            }
            for (vertex_index, vertex) in primitive.vertices.iter().enumerate() {
                if !vertex
                    .position
                    .iter()
                    .chain(vertex.normal.iter())
                    .chain(vertex.tangent.iter())
                    .chain(vertex.texcoord0.iter())
                    .all(|value| value.is_finite())
                {
                    return Err(format!(
                        "primitive {primitive_index} vertex {vertex_index} 包含 NaN/Inf"
                    ));
                }
            }
            if let Some(&index) = primitive
                .indices
                .iter()
                .find(|&&index| index as usize >= primitive.vertices.len())
            {
                return Err(format!(
                    "primitive {primitive_index} index {index} 越界（顶点数 {}）",
                    primitive.vertices.len()
                ));
            }
        }

        let mut stable_ids = self
            .instances
            .iter()
            .map(|instance| instance.stable_id)
            .collect::<Vec<_>>();
        stable_ids.sort_unstable();
        if stable_ids.windows(2).any(|ids| ids[0] == ids[1]) {
            return Err("场景实例 stable_id 重复".to_string());
        }
        // Stage 11 uses the high bit to distinguish a physical instance ID
        // from the same instance seen in unfolded mirror space. Keep the all
        // ones value free as the invalid sentinel as well.
        if stable_ids.iter().any(|&id| id >= 0x7fff_ffff) {
            return Err("场景实例 stable_id 必须小于 0x7fffffff".to_string());
        }
        for (instance_index, instance) in self.instances.iter().enumerate() {
            if instance.primitive_index >= self.primitives.len() {
                return Err(format!(
                    "instance {instance_index} 引用越界 primitive {}",
                    instance.primitive_index
                ));
            }
            for (matrix_name, matrix) in [
                ("base_world", instance.base_world),
                ("current_world", instance.current_world),
                ("previous_world", instance.previous_world),
            ] {
                if !matrix.to_cols_array().iter().all(|value| value.is_finite()) {
                    return Err(format!(
                        "instance {instance_index} 的 {matrix_name} 包含 NaN/Inf"
                    ));
                }
                if matrix.determinant().abs() <= f32::EPSILON {
                    return Err(format!("instance {instance_index} 的 {matrix_name} 不可逆"));
                }
            }
        }
        for (group_index, group) in self.rigid_animation_groups.iter().enumerate() {
            if group.instance_indices.is_empty() {
                return Err(format!("刚体动画组 {group_index} 没有实例"));
            }
            if !group.pivot_world.iter().all(|value| value.is_finite()) {
                return Err(format!("刚体动画组 {group_index} 的 pivot 包含 NaN/Inf"));
            }
            if group
                .instance_indices
                .iter()
                .any(|&index| index >= self.instances.len())
            {
                return Err(format!("刚体动画组 {group_index} 包含越界实例索引"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};

    use super::*;

    #[test]
    fn gpu_layouts_match_hlsl_contract() {
        assert_eq!(size_of::<GpuVertex>(), 48);
        assert_eq!(size_of::<GpuMaterial>(), 80);
        assert_eq!(size_of::<InstanceGpu>(), 64);
        assert_eq!(offset_of!(GpuMaterial, absorption_coefficient), 64);
        assert_eq!(offset_of!(GpuMaterial, medium_flags), 76);
        assert_eq!(align_of::<GpuVertex>(), 4);
        assert_eq!(align_of::<GpuMaterial>(), 4);
        assert_eq!(align_of::<InstanceGpu>(), 4);

        let shader = include_str!("../shaders/stage11_material.hlsli");
        assert!(shader.contains("float3 absorptionCoefficient;"));
        assert!(shader.contains("uint mediumFlags;"));
        assert!(shader.contains("total stride of 80 bytes"));

        let scene_shader = include_str!("../shaders/stage11_scene.hlsli");
        assert!(scene_shader.contains("struct Vertex"));
        assert!(scene_shader.contains("struct InstanceGpu"));
    }

    #[test]
    fn default_cornell_scene_has_valid_references_and_stable_ids() {
        let scene = SceneAsset::cornell_box();
        scene.validate().unwrap();
        assert_eq!(scene.primitives.len(), 18);
        assert_eq!(scene.instances.len(), 18);
        assert!(
            scene
                .instances
                .iter()
                .enumerate()
                .all(|(index, instance)| instance.stable_id == index as u32)
        );
        let area_light = scene
            .materials
            .iter()
            .find(|material| material.kind == MaterialKind::Emissive)
            .expect("Cornell scene owns one visible area-light material");
        assert!(
            area_light.double_sided,
            "the sampled area light must also remain primary-ray visible"
        );
    }

    #[test]
    fn nested_dielectric_fixture_has_closed_non_coplanar_media() {
        let scene = SceneAsset::nested_dielectric_fixture();
        scene.validate().unwrap();
        let outer = scene
            .primitives
            .iter()
            .filter(|primitive| primitive.name.starts_with("nested outer glass face"))
            .collect::<Vec<_>>();
        let inner = scene
            .primitives
            .iter()
            .filter(|primitive| primitive.name.starts_with("nested inner liquid face"))
            .collect::<Vec<_>>();
        assert_eq!(outer.len(), 6);
        assert_eq!(inner.len(), 6);
        let outer_material = scene
            .materials
            .iter()
            .find(|material| material.name == "Nested outer glass")
            .unwrap();
        let inner_material = scene
            .materials
            .iter()
            .find(|material| material.name == "Nested blue-green liquid")
            .unwrap();
        assert_eq!(outer_material.nested_priority, 1);
        assert_eq!(inner_material.nested_priority, 2);
        assert!((outer_material.ior - 1.5).abs() < f32::EPSILON);
        assert!((inner_material.ior - 1.333).abs() < f32::EPSILON);
        assert!(outer_material.absorption_coefficient.iter().any(|v| *v > 0.0));
        assert!(inner_material.absorption_coefficient.iter().any(|v| *v > 0.0));

        let outer_normals = outer
            .iter()
            .map(|primitive| primitive.vertices[0].normal)
            .collect::<Vec<_>>();
        let inner_normals = inner
            .iter()
            .map(|primitive| primitive.vertices[0].normal)
            .collect::<Vec<_>>();
        assert!(outer_normals.iter().any(|outer_normal| {
            inner_normals.iter().any(|inner_normal| {
                let dot = outer_normal[0] * inner_normal[0]
                    + outer_normal[1] * inner_normal[1]
                    + outer_normal[2] * inner_normal[2];
                dot.abs() < 0.999
            })
        }));

        let span = |primitives: &[&MeshPrimitive]| {
            let mut minimum = [f32::INFINITY; 3];
            let mut maximum = [f32::NEG_INFINITY; 3];
            for vertex in primitives.iter().flat_map(|primitive| primitive.vertices.iter()) {
                for axis in 0..3 {
                    minimum[axis] = minimum[axis].min(vertex.position[axis]);
                    maximum[axis] = maximum[axis].max(vertex.position[axis]);
                }
            }
            [
                maximum[0] - minimum[0],
                maximum[1] - minimum[1],
                maximum[2] - minimum[2],
            ]
        };
        let outer_span = span(&outer);
        let inner_span = span(&inner);
        assert!(outer_span
            .iter()
            .zip(inner_span)
            .all(|(outer, inner)| *outer - inner > cornell::NESTED_DIELECTRIC_MIN_GAP));
    }

    #[test]
    fn stable_surface_ids_reserve_virtual_mirror_namespace() {
        let mut scene = SceneAsset::cornell_box();
        scene.instances[0].stable_id = 0x7fff_ffff;
        assert_eq!(
            scene.validate().unwrap_err(),
            "场景实例 stable_id 必须小于 0x7fffffff"
        );
    }

    #[test]
    fn material_validation_rejects_invalid_medium_parameters() {
        let mut scene = SceneAsset::cornell_box();
        scene.materials[5].nested_priority = 16;
        assert!(scene.validate().unwrap_err().contains("nested_priority"));

        let mut scene = SceneAsset::cornell_box();
        scene.materials[5].absorption_coefficient[1] = -0.1;
        assert!(
            scene
                .validate()
                .unwrap_err()
                .contains("absorption_coefficient")
        );

        let mut scene = SceneAsset::cornell_box();
        scene.materials[0].thin_surface = true;
        assert!(scene.validate().unwrap_err().contains("thin_surface"));
    }

    #[test]
    fn cornell_closed_box_bottoms_do_not_overlap_or_float_above_floor() {
        let scene = SceneAsset::cornell_box();
        for name in ["metal box face 5", "glass box face 5"] {
            let bottom = scene
                .primitives
                .iter()
                .find(|primitive| primitive.name == name)
                .unwrap_or_else(|| panic!("missing Cornell primitive {name}"));
            assert!(
                bottom.vertices.iter().all(|vertex| vertex.position[1] < -1.0),
                "{name} must remain slightly embedded below the floor"
            );
        }
    }

    #[test]
    fn scene_validation_rejects_out_of_range_texture_reference() {
        let mut scene = SceneAsset::cornell_box();
        scene.materials[0].base_color_texture = Some(TextureBindingAsset {
            image_index: 0,
            sampler_index: 0,
            texcoord_set: 0,
        });
        let error = scene.validate().unwrap_err();
        assert!(error.contains("base_color image 0 越界"));
    }

    #[test]
    fn append_offsets_animation_group_indices_but_preserves_world_pivot() {
        let mut destination = SceneAsset::cornell_box();
        let mut source = SceneAsset::cornell_box();
        source.rigid_animation_groups = vec![RigidAnimationGroup {
            instance_indices: vec![0, 2],
            pivot_world: [0.25, -0.5, 0.75],
        }];
        let offset = destination.instances.len();
        destination.append(source);
        assert_eq!(
            destination.rigid_animation_groups,
            vec![RigidAnimationGroup {
                instance_indices: vec![offset, offset + 2],
                pivot_world: [0.25, -0.5, 0.75],
            }]
        );
        destination.validate().unwrap();
    }

    #[test]
    fn scene_validation_rejects_nonzero_texcoord_set() {
        let mut scene = SceneAsset::cornell_box();
        scene.images.push(ImageAsset {
            name: "test".to_string(),
            width: 1,
            height: 1,
            rgba8: vec![255; 4],
        });
        scene.materials[0].base_color_texture = Some(TextureBindingAsset {
            image_index: 0,
            sampler_index: 0,
            texcoord_set: 1,
        });
        let error = scene.validate().unwrap_err();
        assert!(error.contains("texCoord 1"));
    }

    #[test]
    fn scene_validation_rejects_textured_primitive_without_uv0() {
        let mut scene = SceneAsset::cornell_box();
        scene.images.push(ImageAsset {
            name: "test".to_string(),
            width: 1,
            height: 1,
            rgba8: vec![255; 4],
        });
        scene.primitives[0].has_texcoord0 = false;
        scene.materials[0].base_color_texture = Some(TextureBindingAsset {
            image_index: 0,
            sampler_index: 0,
            texcoord_set: 0,
        });
        let error = scene.validate().unwrap_err();
        assert!(error.contains("缺少 TEXCOORD_0"));
    }

    #[test]
    fn factor_only_primitive_without_uv0_remains_valid() {
        let mut scene = SceneAsset::cornell_box();
        scene.primitives[0].has_texcoord0 = false;
        scene.validate().unwrap();
    }

    #[test]
    fn texture_sampler_packing_round_trips_and_rejects_overflow() {
        let packed = pack_texture_and_sampler(127, 63).unwrap();
        assert_eq!(unpack_texture_and_sampler(packed), (127, 63));
        assert!(pack_texture_and_sampler(128, 0).is_err());
        assert!(pack_texture_and_sampler(0, 64).is_err());
    }
}
