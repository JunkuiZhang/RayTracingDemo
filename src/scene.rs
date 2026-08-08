#![allow(dead_code)]

use glam::Mat4;

pub mod cornell;
pub mod gltf_loader;

pub const MATERIAL_FLAG_DOUBLE_SIDED: u32 = 1 << 0;
pub const MATERIAL_FLAG_HAS_TANGENT: u32 = 1 << 1;
pub const MATERIAL_FLAG_LEGACY_DIELECTRIC: u32 = 1 << 2;
pub const MATERIAL_FLAG_LEGACY_METAL: u32 = 1 << 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialKind {
    Opaque,
    LegacyMetal,
    LegacyDielectric,
    Emissive,
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
    pub kind: MaterialKind,
    pub double_sided: bool,
    pub base_color_texture: Option<usize>,
    pub metallic_roughness_texture: Option<usize>,
    pub normal_texture: Option<usize>,
    pub emissive_texture: Option<usize>,
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
}

#[derive(Clone, Debug)]
pub struct SceneInstance {
    pub stable_id: u32,
    pub primitive_index: usize,
    pub base_world: Mat4,
    pub current_world: Mat4,
    pub previous_world: Mat4,
}

#[derive(Clone, Debug, Default)]
pub struct SceneAsset {
    pub primitives: Vec<MeshPrimitive>,
    pub materials: Vec<MaterialAsset>,
    pub images: Vec<ImageAsset>,
    pub instances: Vec<SceneInstance>,
    pub animated_root_instances: Vec<usize>,
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

    pub fn append(&mut self, mut other: Self) -> Vec<usize> {
        let primitive_offset = self.primitives.len();
        let material_offset = self.materials.len();
        let image_offset = self.images.len();
        for material in &mut other.materials {
            for texture in [
                &mut material.base_color_texture,
                &mut material.metallic_roughness_texture,
                &mut material.normal_texture,
                &mut material.emissive_texture,
            ] {
                if let Some(index) = texture.as_mut() {
                    *index += image_offset;
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
        let animated_instances = other
            .animated_root_instances
            .iter()
            .map(|index| index + instance_offset)
            .collect::<Vec<_>>();
        self.primitives.extend(other.primitives);
        self.materials.extend(other.materials);
        self.images.extend(other.images);
        self.instances.extend(other.instances);
        self.animated_root_instances
            .extend(animated_instances.iter().copied());
        (instance_offset..self.instances.len()).collect()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.primitives.is_empty() {
            return Err("场景没有可渲染的 mesh primitive".to_string());
        }
        if self.materials.is_empty() {
            return Err("场景没有材质".to_string());
        }
        for (material_index, material) in self.materials.iter().enumerate() {
            for (texture_name, texture_index) in [
                ("base_color", material.base_color_texture),
                ("metallic_roughness", material.metallic_roughness_texture),
                ("normal", material.normal_texture),
                ("emissive", material.emissive_texture),
            ] {
                if let Some(texture_index) = texture_index
                    && texture_index >= self.images.len()
                {
                    return Err(format!(
                        "material {material_index} 的 {texture_name} texture {texture_index} 越界（图片数 {}）",
                        self.images.len()
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
        if self
            .animated_root_instances
            .iter()
            .any(|&index| index >= self.instances.len())
        {
            return Err("animated_root_instances 包含越界实例索引".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, size_of};

    use super::*;

    #[test]
    fn gpu_layouts_match_hlsl_contract() {
        assert_eq!(size_of::<GpuVertex>(), 48);
        assert_eq!(size_of::<GpuMaterial>(), 64);
        assert_eq!(size_of::<InstanceGpu>(), 64);
        assert_eq!(align_of::<GpuVertex>(), 4);
        assert_eq!(align_of::<GpuMaterial>(), 4);
        assert_eq!(align_of::<InstanceGpu>(), 4);
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
    }

    #[test]
    fn scene_validation_rejects_out_of_range_texture_reference() {
        let mut scene = SceneAsset::cornell_box();
        scene.materials[0].base_color_texture = Some(0);
        let error = scene.validate().unwrap_err();
        assert!(error.contains("base_color texture 0 越界"));
    }
}
