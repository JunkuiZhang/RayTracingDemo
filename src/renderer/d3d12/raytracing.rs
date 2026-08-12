use std::{
    ffi::c_void,
    mem::{ManuallyDrop, size_of},
    ptr::NonNull,
    time::Duration,
};

use windows::{
    Win32::Graphics::{Direct3D::ID3DBlob, Direct3D12::*, Dxgi::Common::*},
    core::{Interface, PCWSTR, Result},
};

use crate::scene::{
    GpuMaterial, GpuVertex, InstanceGpu, MATERIAL_FLAG_DOUBLE_SIDED, MATERIAL_FLAG_HAS_TANGENT,
    MATERIAL_FLAG_LEGACY_DIELECTRIC, MATERIAL_FLAG_LEGACY_EMISSIVE, MATERIAL_FLAG_LEGACY_METAL,
    MaterialKind, SceneAsset,
};
use crate::{
    as_policy::{
        AccelerationStructureStats, BlasAllocationRecord, CompactionDecision,
        acceleration_structure_policy, align_acceleration_structure_size, evaluate_compaction,
        required_scratch_size as policy_required_scratch_size,
    },
    realtime::AccelerationStructureMode,
};

use super::texture::TextureSet;

#[derive(Clone, Copy)]
struct PrimitiveRange {
    vertex_offset: u32,
    index_offset: u32,
    vertex_count: u32,
    index_count: u32,
}

struct AnimationGroup {
    instance_indices: Vec<usize>,
    pivot_world: glam::Vec3,
}

const INSTANCE_FLAG_CULL_DISABLE: u32 = 1;
const INSTANCE_FLAG_FRONT_COUNTER_CLOCKWISE: u32 = 2;

/// Cornell Box 的网格资源，包含逐顶点法线和逐三角形材质索引。
pub struct SceneGeometry {
    vertex_buffer: ID3D12Resource,
    index_buffer: ID3D12Resource,
    material_buffer: ID3D12Resource,
    vertex_count: u32,
    index_count: u32,
    material_count: u32,
    primitive_ranges: Vec<PrimitiveRange>,
    current_transforms: Vec<glam::Mat4>,
    instance_primitive_indices: Vec<usize>,
    instance_flags: Vec<u32>,
    upload_buffers: Vec<ID3D12Resource>,
    instance_data: Vec<InstanceGpu>,
    base_transforms: Vec<glam::Mat4>,
    previous_transforms: Vec<glam::Mat4>,
    animation_groups: Vec<AnimationGroup>,
}

/// 保持 BLAS、TLAS 及其构建依赖资源存活。
pub struct AccelerationStructures {
    pub tlas: ID3D12Resource,
    blas: Vec<ID3D12Resource>,
    build_scratch: Option<ID3D12Resource>,
    frame_data: Vec<FrameInstanceData>,
    tlas_update_enabled: bool,
    tlas_build_flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS,
    initialization_retirements: Vec<ID3D12Resource>,
    telemetry: AccelerationStructureStats,
}

const FRAME_CONTEXT_COUNT: usize = 3;

struct FrameInstanceData {
    instance_descs: MappedUpload,
    instance_gpu: MappedUpload,
}

struct MappedUpload {
    resource: ID3D12Resource,
    mapped: NonNull<u8>,
    byte_size: usize,
}

impl MappedUpload {
    fn new<T: Copy>(device: &ID3D12Device, values: &[T], name: &str) -> Result<Self> {
        let resource = create_upload_buffer(device, values, name)?;
        let mut mapped = std::ptr::null_mut::<c_void>();
        unsafe { resource.Map(0, None, Some(&mut mapped))? };
        Ok(Self {
            resource,
            mapped: NonNull::new(mapped.cast()).unwrap(),
            byte_size: size_of_val(values),
        })
    }

    fn write<T: Copy>(&self, values: &[T]) {
        let byte_size = size_of_val(values);
        assert!(
            byte_size <= self.byte_size,
            "Frame-local upload buffer is too small"
        );
        unsafe {
            std::ptr::copy_nonoverlapping(
                values.as_ptr().cast::<u8>(),
                self.mapped.as_ptr(),
                byte_size,
            );
        }
    }
}

impl Drop for MappedUpload {
    fn drop(&mut self) {
        unsafe { self.resource.Unmap(0, None) };
    }
}

impl SceneGeometry {
    pub fn new(
        device: &ID3D12Device,
        command_list: &ID3D12GraphicsCommandList,
        scene: &SceneAsset,
        textures: &TextureSet,
    ) -> Result<Self> {
        scene.validate().map_err(|error| {
            windows::core::Error::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                format!("验证 Cornell Box CPU 场景：{error}"),
            )
        })?;
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut primitive_ranges = Vec::with_capacity(scene.primitives.len());
        for primitive in &scene.primitives {
            let vertex_offset = vertices.len() as u32;
            let index_offset = indices.len() as u32;
            vertices.extend(primitive.vertices.iter().map(|vertex| GpuVertex {
                position: vertex.position,
                normal: vertex.normal,
                tangent: vertex.tangent,
                texcoord0: vertex.texcoord0,
            }));
            indices.extend(primitive.indices.iter().copied());
            primitive_ranges.push(PrimitiveRange {
                vertex_offset,
                index_offset,
                vertex_count: primitive.vertices.len() as u32,
                index_count: primitive.indices.len() as u32,
            });
        }
        let mut material_has_tangent = vec![true; scene.materials.len()];
        for primitive in &scene.primitives {
            if primitive.vertices.iter().any(|vertex| !vertex.has_tangent) {
                material_has_tangent[primitive.material_index] = false;
            }
        }
        let materials = scene
            .materials
            .iter()
            .enumerate()
            .map(|(index, material)| gpu_material(material, textures, material_has_tangent[index]))
            .collect::<Result<Vec<_>>>()?;
        let instances = scene
            .instances
            .iter()
            .map(|instance| {
                let range = primitive_ranges[instance.primitive_index];
                InstanceGpu {
                    previous_object_to_world_row0: matrix_rows(instance.previous_world)[0],
                    previous_object_to_world_row1: matrix_rows(instance.previous_world)[1],
                    previous_object_to_world_row2: matrix_rows(instance.previous_world)[2],
                    vertex_offset: range.vertex_offset,
                    index_offset: range.index_offset,
                    material_index: scene.primitives[instance.primitive_index].material_index
                        as u32,
                    stable_surface_id: instance.stable_id,
                }
            })
            .collect::<Vec<_>>();
        let current_transforms = scene
            .instances
            .iter()
            .map(|instance| instance.current_world)
            .collect::<Vec<_>>();
        let base_transforms = scene
            .instances
            .iter()
            .map(|instance| instance.base_world)
            .collect::<Vec<_>>();
        let previous_transforms = scene
            .instances
            .iter()
            .map(|instance| instance.previous_world)
            .collect::<Vec<_>>();
        let instance_primitive_indices = scene
            .instances
            .iter()
            .map(|instance| instance.primitive_index)
            .collect();
        let instance_flags = scene
            .instances
            .iter()
            .map(|instance| {
                let material =
                    &scene.materials[scene.primitives[instance.primitive_index].material_index];
                compute_instance_flags(material)
            })
            .collect();
        let (vertex_buffer, vertex_upload) =
            create_static_buffer(device, command_list, &vertices, "Cornell Box 顶点")?;
        let (index_buffer, index_upload) =
            create_static_buffer(device, command_list, &indices, "Cornell Box 索引")?;
        let (material_buffer, material_upload) =
            create_static_buffer(device, command_list, &materials, "场景材质")?;
        Ok(Self {
            vertex_buffer,
            index_buffer,
            material_buffer,
            vertex_count: vertices.len() as u32,
            index_count: indices.len() as u32,
            material_count: materials.len() as u32,
            primitive_ranges,
            current_transforms,
            instance_primitive_indices,
            instance_flags,
            upload_buffers: vec![vertex_upload, index_upload, material_upload],
            instance_data: instances,
            base_transforms,
            previous_transforms,
            animation_groups: scene
                .rigid_animation_groups
                .iter()
                .map(|group| AnimationGroup {
                    instance_indices: group.instance_indices.clone(),
                    pivot_world: glam::Vec3::from_array(group.pivot_world),
                })
                .collect(),
        })
    }

    /// Upload buffers must stay alive until the initialization fence completes.
    pub fn release_uploads(&mut self) {
        self.upload_buffers.clear();
    }

    /// 返回构建 BLAS 时使用的三角形描述。
    pub fn geometry_desc(&self, primitive_index: usize) -> D3D12_RAYTRACING_GEOMETRY_DESC {
        let range = self.primitive_ranges[primitive_index];
        let (index_buffer, vertex_buffer) = unsafe {
            (
                self.index_buffer.GetGPUVirtualAddress(),
                self.vertex_buffer.GetGPUVirtualAddress(),
            )
        };
        D3D12_RAYTRACING_GEOMETRY_DESC {
            Type: D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES,
            Flags: D3D12_RAYTRACING_GEOMETRY_FLAG_OPAQUE,
            Anonymous: D3D12_RAYTRACING_GEOMETRY_DESC_0 {
                Triangles: D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
                    Transform3x4: 0,
                    IndexFormat: DXGI_FORMAT_R32_UINT,
                    VertexFormat: DXGI_FORMAT_R32G32B32_FLOAT,
                    IndexCount: range.index_count,
                    VertexCount: range.vertex_count,
                    IndexBuffer: index_buffer + range.index_offset as u64 * size_of::<u32>() as u64,
                    VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE {
                        StartAddress: vertex_buffer
                            + range.vertex_offset as u64 * size_of::<GpuVertex>() as u64,
                        StrideInBytes: size_of::<GpuVertex>() as u64,
                    },
                },
            },
        }
    }

    pub fn vertex_buffer(&self) -> &ID3D12Resource {
        &self.vertex_buffer
    }

    pub fn index_buffer(&self) -> &ID3D12Resource {
        &self.index_buffer
    }

    pub fn material_buffer(&self) -> &ID3D12Resource {
        &self.material_buffer
    }

    pub fn vertex_count(&self) -> u32 {
        self.vertex_count
    }

    pub fn index_count(&self) -> u32 {
        self.index_count
    }

    pub fn material_count(&self) -> u32 {
        self.material_count
    }

    pub fn primitive_count(&self) -> usize {
        self.primitive_ranges.len()
    }

    pub fn has_animation_groups(&self) -> bool {
        !self.animation_groups.is_empty()
    }

    pub fn instance_descriptors(
        &self,
        blas: &[ID3D12Resource],
    ) -> Vec<D3D12_RAYTRACING_INSTANCE_DESC> {
        self.current_transforms
            .iter()
            .enumerate()
            .map(|(index, transform)| D3D12_RAYTRACING_INSTANCE_DESC {
                Transform: matrix_to_d3d12_transform(*transform),
                _bitfield1: (index as u32 & 0x00FF_FFFF) | (0xFF << 24),
                _bitfield2: self.instance_flags[index] << 24,
                AccelerationStructure: unsafe {
                    blas[self.instance_primitive_indices[index]].GetGPUVirtualAddress()
                },
            })
            .collect()
    }

    pub fn instance_gpu_data(&self) -> &[InstanceGpu] {
        &self.instance_data
    }

    pub fn prepare_animation(&mut self, elapsed: Duration, animate_model: bool) -> bool {
        if !animate_model || self.animation_groups.is_empty() {
            return false;
        }
        let dirty = animation_dirty(
            &self.current_transforms,
            &self.base_transforms,
            &self.animation_groups,
            elapsed,
            animate_model,
        );
        for group in &self.animation_groups {
            let transform = animation_transform_at(group.pivot_world, elapsed);
            for &index in &group.instance_indices {
                self.current_transforms[index] = transform * self.base_transforms[index];
                let rows = matrix_rows(self.previous_transforms[index]);
                self.instance_data[index].previous_object_to_world_row0 = rows[0];
                self.instance_data[index].previous_object_to_world_row1 = rows[1];
                self.instance_data[index].previous_object_to_world_row2 = rows[2];
            }
        }
        dirty
    }

    pub fn commit_animation(&mut self, animate_model: bool) {
        if !animate_model || self.animation_groups.is_empty() {
            return;
        }
        for group in &self.animation_groups {
            for &index in &group.instance_indices {
                self.previous_transforms[index] = self.current_transforms[index];
            }
        }
    }
}

fn animation_transform(pivot_world: glam::Vec3, angle: f32) -> glam::Mat4 {
    let pivot = glam::Mat4::from_translation(pivot_world);
    pivot * glam::Mat4::from_rotation_y(angle) * glam::Mat4::from_translation(-pivot_world)
}

fn animation_transform_at(pivot_world: glam::Vec3, elapsed: Duration) -> glam::Mat4 {
    animation_transform(pivot_world, elapsed.as_secs_f32() * 0.35)
}

fn animation_dirty(
    current_transforms: &[glam::Mat4],
    base_transforms: &[glam::Mat4],
    animation_groups: &[AnimationGroup],
    elapsed: Duration,
    animate_model: bool,
) -> bool {
    if !animate_model {
        return false;
    }
    animation_groups.iter().any(|group| {
        let transform = animation_transform_at(group.pivot_world, elapsed);
        group
            .instance_indices
            .iter()
            .any(|&index| current_transforms[index] != transform * base_transforms[index])
    })
}

fn matrix_to_d3d12_transform(matrix: glam::Mat4) -> [f32; 12] {
    let rows = matrix_rows(matrix);
    [
        rows[0][0], rows[0][1], rows[0][2], rows[0][3], rows[1][0], rows[1][1], rows[1][2],
        rows[1][3], rows[2][0], rows[2][1], rows[2][2], rows[2][3],
    ]
}

fn matrix_rows(matrix: glam::Mat4) -> [[f32; 4]; 4] {
    let columns = matrix.to_cols_array_2d();
    [
        [columns[0][0], columns[1][0], columns[2][0], columns[3][0]],
        [columns[0][1], columns[1][1], columns[2][1], columns[3][1]],
        [columns[0][2], columns[1][2], columns[2][2], columns[3][2]],
        [columns[0][3], columns[1][3], columns[2][3], columns[3][3]],
    ]
}

/// DXR triangle facing is defined in object space. Instance transforms,
/// including transforms with a negative determinant, do not change winding.
fn compute_instance_flags(material: &crate::scene::MaterialAsset) -> u32 {
    let mut flags = INSTANCE_FLAG_FRONT_COUNTER_CLOCKWISE;
    if material.double_sided || material.kind == MaterialKind::LegacyDielectric {
        flags |= INSTANCE_FLAG_CULL_DISABLE;
    }
    flags
}

fn gpu_material(
    material: &crate::scene::MaterialAsset,
    textures: &TextureSet,
    has_tangent: bool,
) -> Result<GpuMaterial> {
    let mut flags = 0;
    if material.double_sided {
        flags |= MATERIAL_FLAG_DOUBLE_SIDED;
    }
    if material.kind == MaterialKind::LegacyDielectric {
        flags |= MATERIAL_FLAG_LEGACY_DIELECTRIC;
    }
    if material.kind == MaterialKind::LegacyMetal {
        flags |= MATERIAL_FLAG_LEGACY_METAL;
    }
    if material.kind == MaterialKind::Emissive {
        flags |= MATERIAL_FLAG_LEGACY_EMISSIVE;
    }
    if material.normal_texture.is_some() && has_tangent {
        flags |= MATERIAL_FLAG_HAS_TANGENT;
    }
    let [base_color, metallic_roughness, normal, emissive] =
        textures.material_texture_indices(material)?;
    Ok(GpuMaterial {
        base_color_factor: material.base_color_factor,
        emissive_factor: material.emissive_factor,
        metallic_factor: material.metallic_factor,
        roughness_factor: material.roughness_factor,
        normal_scale: material.normal_scale,
        ior: material.ior,
        flags,
        base_color_texture_and_sampler: base_color,
        metallic_roughness_texture_and_sampler: metallic_roughness,
        normal_texture_and_sampler: normal,
        emissive_texture_and_sampler: emissive,
    })
}

struct BlasBuildInfo {
    prebuild: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO,
    original_allocation_bytes: u64,
}

pub(super) struct PendingAccelerationStructures {
    mode: AccelerationStructureMode,
    tlas_update_enabled: bool,
    tlas_build_flags: u32,
    blas_infos: Vec<BlasBuildInfo>,
    original_blas: Vec<Option<ID3D12Resource>>,
    phase_a_scratch: Option<ID3D12Resource>,
    postbuild_buffer: Option<ID3D12Resource>,
    readback: Option<ID3D12Resource>,
}

impl AccelerationStructures {
    pub fn build_phase_a(
        device: &ID3D12Device,
        command_list: &ID3D12GraphicsCommandList,
        geometry: &SceneGeometry,
        mode: AccelerationStructureMode,
        animate_model: bool,
    ) -> Result<PendingAccelerationStructures> {
        let device5: ID3D12Device5 = device.cast()?;
        let policy =
            acceleration_structure_policy(mode, animate_model, geometry.has_animation_groups());
        let blas_flags =
            D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS(policy.blas_build_flags as i32);
        let command_list4: ID3D12GraphicsCommandList4 = command_list.cast()?;
        let mut original_blas = Vec::with_capacity(geometry.primitive_count());
        let mut blas_infos = Vec::with_capacity(geometry.primitive_count());
        for primitive_index in 0..geometry.primitive_count() {
            let geometry_desc = geometry.geometry_desc(primitive_index);
            let blas_inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
                Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
                Flags: blas_flags,
                NumDescs: 1,
                DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 {
                    pGeometryDescs: &geometry_desc,
                },
            };
            let mut prebuild = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
            unsafe {
                device5.GetRaytracingAccelerationStructurePrebuildInfo(&blas_inputs, &mut prebuild);
            }
            let result_bytes = align_acceleration_structure_size(prebuild.ResultDataMaxSizeInBytes)
                .ok_or_else(|| as_error(format!("BLAS {primitive_index} result size overflow")))?;
            let original_allocation_bytes = resource_allocation_bytes(
                device,
                result_bytes,
                D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            )
            .ok_or_else(|| {
                as_error(format!(
                    "BLAS {primitive_index} allocation query failed for {result_bytes} bytes"
                ))
            })?;
            blas_infos.push(BlasBuildInfo {
                prebuild,
                original_allocation_bytes,
            });
            original_blas.push(Some(create_default_buffer(
                device,
                result_bytes,
                D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
            )?));
        }
        let scratch_size = policy_required_scratch_size(
            blas_infos
                .iter()
                .map(|info| info.prebuild.ScratchDataSizeInBytes),
            0,
            None,
        );
        let scratch = create_default_buffer(
            device,
            scratch_size,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_COMMON,
        )?;
        transition_buffer(
            command_list,
            &scratch,
            D3D12_RESOURCE_STATE_COMMON,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        );
        for (primitive_index, blas_resource) in original_blas.iter().enumerate() {
            let geometry_desc = geometry.geometry_desc(primitive_index);
            let blas_inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
                Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
                Flags: blas_flags,
                NumDescs: 1,
                DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 {
                    pGeometryDescs: &geometry_desc,
                },
            };
            let blas_resource = blas_resource
                .as_ref()
                .ok_or_else(|| as_error(format!("BLAS {primitive_index} resource missing")))?;
            let blas_build = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
                DestAccelerationStructureData: unsafe { blas_resource.GetGPUVirtualAddress() },
                Inputs: blas_inputs,
                SourceAccelerationStructureData: 0,
                ScratchAccelerationStructureData: unsafe { scratch.GetGPUVirtualAddress() },
            };
            unsafe {
                command_list4.BuildRaytracingAccelerationStructure(&blas_build, None);
            }
            uav_barrier(command_list, blas_resource);
        }
        let (postbuild_buffer, readback) = if policy.compact_blas {
            let query_size = (geometry.primitive_count() as u64)
                .checked_mul(size_of::<u64>() as u64)
                .ok_or_else(|| as_error("BLAS postbuild query size overflow"))?;
            let postbuild = create_default_buffer(
                device,
                query_size,
                D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COMMON,
            )?;
            transition_buffer(
                command_list,
                &postbuild,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            );
            let source_addresses = original_blas
                .iter()
                .map(|resource| unsafe { resource.as_ref().unwrap().GetGPUVirtualAddress() })
                .collect::<Vec<_>>();
            let postbuild_desc = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC {
                DestBuffer: unsafe { postbuild.GetGPUVirtualAddress() },
                InfoType: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_COMPACTED_SIZE,
            };
            unsafe {
                command_list4.EmitRaytracingAccelerationStructurePostbuildInfo(
                    &postbuild_desc,
                    &source_addresses,
                );
            }
            uav_barrier(command_list, &postbuild);
            transition_buffer(
                command_list,
                &postbuild,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            );
            let readback = create_readback_buffer(device, query_size)?;
            unsafe {
                command_list.CopyBufferRegion(&readback, 0, &postbuild, 0, query_size);
            }
            (Some(postbuild), Some(readback))
        } else {
            (None, None)
        };
        Ok(PendingAccelerationStructures {
            mode,
            tlas_update_enabled: policy.tlas_update_enabled,
            tlas_build_flags: policy.tlas_build_flags,
            blas_infos,
            original_blas,
            phase_a_scratch: Some(scratch),
            postbuild_buffer,
            readback,
        })
    }
}

impl PendingAccelerationStructures {
    pub fn finish_phase_b(
        mut self,
        device: &ID3D12Device,
        command_list: &ID3D12GraphicsCommandList,
        geometry: &SceneGeometry,
    ) -> Result<AccelerationStructures> {
        let compacted_sizes = match self.readback.as_ref() {
            Some(readback) => read_compacted_sizes(readback, self.blas_infos.len())?,
            None => vec![0; self.blas_infos.len()],
        };
        let command_list4: ID3D12GraphicsCommandList4 = command_list.cast()?;
        let mut final_blas = Vec::with_capacity(self.original_blas.len());
        let mut retirements = Vec::with_capacity(self.original_blas.len() + 3);
        let mut records = Vec::with_capacity(self.blas_infos.len());
        for (primitive_index, reported_size) in compacted_sizes.iter().copied().enumerate() {
            let info = &self.blas_infos[primitive_index];
            let original = self.original_blas[primitive_index].take().ok_or_else(|| {
                as_error(format!("BLAS {primitive_index} original resource missing"))
            })?;
            let candidate_size = (reported_size > 0)
                .then(|| align_acceleration_structure_size(reported_size))
                .flatten();
            let candidate_allocation_bytes = candidate_size
                .and_then(|size| {
                    resource_allocation_bytes(
                        device,
                        size,
                        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                    )
                })
                .unwrap_or(0);
            let evaluation = evaluate_compaction(
                self.mode == AccelerationStructureMode::Optimized,
                info.prebuild.ResultDataMaxSizeInBytes,
                reported_size,
                info.original_allocation_bytes,
                candidate_allocation_bytes,
            );
            let (final_result_bytes, final_allocation_bytes) =
                if evaluation.decision == CompactionDecision::Compacted {
                    let compacted_size = evaluation
                        .aligned_compacted_bytes
                        .ok_or_else(|| as_error("compacted BLAS size missing"))?;
                    let compacted = create_default_buffer(
                        device,
                        compacted_size,
                        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                        D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
                    )?;
                    unsafe {
                        command_list4.CopyRaytracingAccelerationStructure(
                            compacted.GetGPUVirtualAddress(),
                            original.GetGPUVirtualAddress(),
                            D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_COMPACT,
                        );
                    }
                    uav_barrier(command_list, &compacted);
                    retirements.push(original);
                    final_blas.push(compacted);
                    (compacted_size, candidate_allocation_bytes)
                } else {
                    final_blas.push(original);
                    (
                        info.prebuild.ResultDataMaxSizeInBytes,
                        info.original_allocation_bytes,
                    )
                };
            records.push(BlasAllocationRecord {
                primitive_index,
                original_result_bytes: info.prebuild.ResultDataMaxSizeInBytes,
                reported_compacted_bytes: reported_size,
                original_allocation_bytes: info.original_allocation_bytes,
                candidate_allocation_bytes,
                final_result_bytes,
                final_allocation_bytes,
                decision: evaluation.decision,
            });
        }
        if let Some(scratch) = self.phase_a_scratch.take() {
            retirements.push(scratch);
        }
        if let Some(postbuild_buffer) = self.postbuild_buffer.take() {
            retirements.push(postbuild_buffer);
        }
        if let Some(readback) = self.readback.take() {
            retirements.push(readback);
        }

        let instance_descs = geometry.instance_descriptors(&final_blas);
        let mut frame_data = Vec::with_capacity(FRAME_CONTEXT_COUNT);
        for frame_index in 0..FRAME_CONTEXT_COUNT {
            frame_data.push(FrameInstanceData {
                instance_descs: MappedUpload::new(
                    device,
                    &instance_descs,
                    &format!("Frame {frame_index} TLAS 实例"),
                )?,
                instance_gpu: MappedUpload::new(
                    device,
                    geometry.instance_gpu_data(),
                    &format!("Frame {frame_index} InstanceGpu"),
                )?,
            });
        }
        let tlas_build_flags =
            D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS(self.tlas_build_flags as i32);
        let instance_buffer = &frame_data[0].instance_descs.resource;
        let tlas_inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
            Flags: tlas_build_flags,
            NumDescs: instance_descs.len() as u32,
            DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
            Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 {
                InstanceDescs: unsafe { instance_buffer.GetGPUVirtualAddress() },
            },
        };
        let device5: ID3D12Device5 = device.cast()?;
        let mut tlas_info = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
        unsafe {
            device5.GetRaytracingAccelerationStructurePrebuildInfo(&tlas_inputs, &mut tlas_info);
        }
        let tlas_result_bytes = tlas_info.ResultDataMaxSizeInBytes;
        let tlas_result_allocation_size = align_acceleration_structure_size(tlas_result_bytes)
            .ok_or_else(|| as_error("TLAS result size overflow"))?;
        let tlas = create_default_buffer(
            device,
            tlas_result_allocation_size,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
        )?;
        let scratch_size = policy_required_scratch_size(
            std::iter::empty::<u64>(),
            tlas_info.ScratchDataSizeInBytes,
            self.tlas_update_enabled
                .then_some(tlas_info.UpdateScratchDataSizeInBytes),
        );
        let scratch = create_default_buffer(
            device,
            scratch_size,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_COMMON,
        )?;
        transition_buffer(
            command_list,
            &scratch,
            D3D12_RESOURCE_STATE_COMMON,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        );
        let tlas_build = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
            DestAccelerationStructureData: unsafe { tlas.GetGPUVirtualAddress() },
            Inputs: tlas_inputs,
            SourceAccelerationStructureData: 0,
            ScratchAccelerationStructureData: unsafe { scratch.GetGPUVirtualAddress() },
        };
        unsafe {
            command_list4.BuildRaytracingAccelerationStructure(&tlas_build, None);
        }
        uav_barrier(command_list, &tlas);
        let tlas_allocation_bytes = resource_allocation_bytes(
            device,
            tlas_result_allocation_size,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        )
        .ok_or_else(|| {
            as_error(format!(
                "TLAS allocation query failed for {tlas_result_allocation_size} bytes"
            ))
        })?;
        let retained_update_scratch_required_bytes = if self.tlas_update_enabled {
            scratch_size
        } else {
            0
        };
        let retained_update_scratch_allocation_bytes = if self.tlas_update_enabled {
            resource_allocation_bytes(
                device,
                scratch_size,
                D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            )
            .ok_or_else(|| {
                as_error(format!(
                    "TLAS scratch allocation query failed for {scratch_size} bytes"
                ))
            })?
        } else {
            0
        };
        let telemetry = AccelerationStructureStats::from_records(
            self.mode,
            self.tlas_update_enabled,
            records,
            tlas_result_bytes,
            tlas_allocation_bytes,
            retained_update_scratch_required_bytes,
            retained_update_scratch_allocation_bytes,
        );
        debug_assert_eq!(
            crate::as_policy::final_blas_primitive_order(&telemetry.blas),
            (0..final_blas.len()).collect::<Vec<_>>()
        );
        Ok(AccelerationStructures {
            tlas,
            blas: final_blas,
            build_scratch: Some(scratch),
            frame_data,
            tlas_update_enabled: self.tlas_update_enabled,
            tlas_build_flags,
            initialization_retirements: retirements,
            telemetry,
        })
    }
}

impl AccelerationStructures {
    pub fn update(
        &mut self,
        command_list: &ID3D12GraphicsCommandList,
        frame_index: usize,
        geometry: &SceneGeometry,
    ) -> Result<()> {
        if !self.tlas_update_enabled {
            return Err(as_error("静态 TLAS 不允许 update；调用方违反了 AS 策略"));
        }
        let frame = &self.frame_data[frame_index % FRAME_CONTEXT_COUNT];
        let instance_descs = geometry.instance_descriptors(&self.blas);
        frame.instance_descs.write(&instance_descs);
        frame.instance_gpu.write(geometry.instance_gpu_data());
        let scratch = self.build_scratch.as_ref().ok_or_else(|| {
            windows::core::Error::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                "TLAS update scratch 已被释放",
            )
        })?;
        let inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
            Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS(
                crate::as_policy::update_flags(self.tlas_build_flags.0 as u32) as i32,
            ),
            NumDescs: instance_descs.len() as u32,
            DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
            Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 {
                InstanceDescs: unsafe { frame.instance_descs.resource.GetGPUVirtualAddress() },
            },
        };
        let build = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
            DestAccelerationStructureData: unsafe { self.tlas.GetGPUVirtualAddress() },
            Inputs: inputs,
            SourceAccelerationStructureData: unsafe { self.tlas.GetGPUVirtualAddress() },
            ScratchAccelerationStructureData: unsafe { scratch.GetGPUVirtualAddress() },
        };
        let command_list4: ID3D12GraphicsCommandList4 = command_list.cast()?;
        unsafe { command_list4.BuildRaytracingAccelerationStructure(&build, None) };
        uav_barrier(command_list, &self.tlas);
        Ok(())
    }

    pub fn instance_gpu_address(&self, frame_index: usize) -> u64 {
        unsafe {
            self.frame_data[frame_index % FRAME_CONTEXT_COUNT]
                .instance_gpu
                .resource
                .GetGPUVirtualAddress()
        }
    }

    pub fn release_build_resources(&mut self) {
        self.initialization_retirements.clear();
        if !self.tlas_update_enabled {
            self.build_scratch = None;
        }
    }

    pub fn stats(&self) -> &AccelerationStructureStats {
        &self.telemetry
    }
}

fn as_error(message: impl Into<String>) -> windows::core::Error {
    windows::core::Error::new(
        windows::core::HRESULT(0x80004005_u32 as i32),
        message.into(),
    )
}

fn read_compacted_sizes(resource: &ID3D12Resource, count: usize) -> Result<Vec<u64>> {
    let byte_size = count
        .checked_mul(size_of::<u64>())
        .ok_or_else(|| as_error("BLAS compacted size readback size overflow"))?;
    let read_range = D3D12_RANGE {
        Begin: 0,
        End: byte_size,
    };
    let mut mapped = std::ptr::null_mut::<c_void>();
    unsafe {
        resource.Map(0, Some(&read_range), Some(&mut mapped))?;
        let values = std::slice::from_raw_parts(mapped.cast::<u64>(), count).to_vec();
        resource.Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
        Ok(values)
    }
}

fn resource_allocation_bytes(
    device: &ID3D12Device,
    size: u64,
    flags: D3D12_RESOURCE_FLAGS,
) -> Option<u64> {
    if size == 0 {
        return None;
    }
    let description = buffer_resource_desc(size, flags);
    let size_in_bytes =
        unsafe { device.GetResourceAllocationInfo(0, std::slice::from_ref(&description)) }
            .SizeInBytes;
    valid_allocation_size(size_in_bytes)
}

fn valid_allocation_size(size_in_bytes: u64) -> Option<u64> {
    (size_in_bytes != 0 && size_in_bytes != u64::MAX).then_some(size_in_bytes)
}

fn buffer_resource_desc(size: u64, flags: D3D12_RESOURCE_FLAGS) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        Width: size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: flags,
    }
}

fn uav_barrier(command_list: &ID3D12GraphicsCommandList, resource: &ID3D12Resource) {
    let mut barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            UAV: ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
            }),
        },
    };
    unsafe {
        command_list.ResourceBarrier(std::slice::from_ref(&barrier));
        ManuallyDrop::drop(&mut (*barrier.Anonymous.UAV).pResource);
    }
}

fn create_default_buffer(
    device: &ID3D12Device,
    size: u64,
    flags: D3D12_RESOURCE_FLAGS,
    state: D3D12_RESOURCE_STATES,
) -> Result<ID3D12Resource> {
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
        MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
        CreationNodeMask: 0,
        VisibleNodeMask: 0,
    };
    let description = buffer_resource_desc(size, flags);
    let mut resource = None;
    unsafe {
        device.CreateCommittedResource(
            &heap,
            D3D12_HEAP_FLAG_NONE,
            &description,
            state,
            None,
            &mut resource,
        )?;
    }
    Ok(resource.unwrap())
}

fn create_readback_buffer(device: &ID3D12Device, size: u64) -> Result<ID3D12Resource> {
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_READBACK,
        ..Default::default()
    };
    let description = buffer_resource_desc(size, D3D12_RESOURCE_FLAGS(0));
    let mut resource = None;
    unsafe {
        device.CreateCommittedResource(
            &heap,
            D3D12_HEAP_FLAG_NONE,
            &description,
            D3D12_RESOURCE_STATE_COPY_DEST,
            None,
            &mut resource,
        )?;
    }
    Ok(resource.unwrap())
}

fn create_static_buffer<T: Copy>(
    device: &ID3D12Device,
    command_list: &ID3D12GraphicsCommandList,
    values: &[T],
    name: &str,
) -> Result<(ID3D12Resource, ID3D12Resource)> {
    let byte_size = size_of_val(values) as u64;
    let default = create_default_buffer(
        device,
        byte_size,
        D3D12_RESOURCE_FLAG_NONE,
        D3D12_RESOURCE_STATE_COMMON,
    )?;
    set_resource_name(&default, name)?;
    let upload = create_upload_buffer(device, values, &format!("{name} Upload"))?;
    transition_buffer(
        command_list,
        &default,
        D3D12_RESOURCE_STATE_COMMON,
        D3D12_RESOURCE_STATE_COPY_DEST,
    );
    unsafe {
        command_list.CopyBufferRegion(&default, 0, &upload, 0, byte_size);
    }
    transition_buffer(
        command_list,
        &default,
        D3D12_RESOURCE_STATE_COPY_DEST,
        D3D12_RESOURCE_STATE_GENERIC_READ,
    );
    Ok((default, upload))
}

fn transition_buffer(
    command_list: &ID3D12GraphicsCommandList,
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) {
    let mut barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    };
    unsafe {
        command_list.ResourceBarrier(std::slice::from_ref(&barrier));
        ManuallyDrop::drop(&mut (*barrier.Anonymous.Transition).pResource);
    }
}

fn set_resource_name(resource: &ID3D12Resource, name: &str) -> Result<()> {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    unsafe { resource.SetName(windows::core::PCWSTR(wide.as_ptr())) }
}

fn create_upload_buffer<T: Copy>(
    device: &ID3D12Device,
    values: &[T],
    name: &str,
) -> Result<ID3D12Resource> {
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_UPLOAD,
        CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
        MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
        CreationNodeMask: 0,
        VisibleNodeMask: 0,
    };
    let description = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        Width: size_of_val(values) as u64,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_NONE,
    };
    let mut resource = None;
    unsafe {
        device.CreateCommittedResource(
            &heap,
            D3D12_HEAP_FLAG_NONE,
            &description,
            D3D12_RESOURCE_STATE_GENERIC_READ,
            None,
            &mut resource,
        )?;
    }
    let resource: ID3D12Resource = resource.unwrap();
    let mut mapped = std::ptr::null_mut::<c_void>();
    unsafe {
        resource.Map(0, None, Some(&mut mapped))?;
    }
    let mapped = NonNull::new(mapped.cast::<T>()).unwrap();
    unsafe {
        std::ptr::copy_nonoverlapping(values.as_ptr(), mapped.as_ptr(), values.len());
        resource.Unmap(0, None);
    }
    set_resource_name(&resource, name)?;
    Ok(resource)
}

/// DXR 状态对象、全局根签名和三条 Shader Table 记录。
pub struct RaytracingPipeline {
    pub state_object: ID3D12StateObject,
    pub root_signature: ID3D12RootSignature,
    _shader_table: ID3D12Resource,
    pub raygen: D3D12_GPU_VIRTUAL_ADDRESS_RANGE,
    pub miss: D3D12_GPU_VIRTUAL_ADDRESS_RANGE_AND_STRIDE,
    pub hit_group: D3D12_GPU_VIRTUAL_ADDRESS_RANGE_AND_STRIDE,
}

impl RaytracingPipeline {
    pub fn new(device: &ID3D12Device, shader: &[u8]) -> Result<Self> {
        let device5: ID3D12Device5 = device.cast()?;
        let root_signature = create_raytracing_root_signature(device)?;
        let library = D3D12_DXIL_LIBRARY_DESC {
            DXILLibrary: D3D12_SHADER_BYTECODE {
                pShaderBytecode: shader.as_ptr().cast(),
                BytecodeLength: shader.len(),
            },
            NumExports: 0,
            pExports: std::ptr::null(),
        };
        let hit_group_name = wide("HitGroup");
        let closest_hit_name = wide("ClosestHit");
        let hit_group = D3D12_HIT_GROUP_DESC {
            HitGroupExport: PCWSTR(hit_group_name.as_ptr()),
            Type: D3D12_HIT_GROUP_TYPE_TRIANGLES,
            AnyHitShaderImport: PCWSTR::null(),
            ClosestHitShaderImport: PCWSTR(closest_hit_name.as_ptr()),
            IntersectionShaderImport: PCWSTR::null(),
        };
        let shader_config = D3D12_RAYTRACING_SHADER_CONFIG {
            // The 92-byte payload includes NRD PSR's mirror plane and path
            // throughput. Keep a small aligned ceiling for laptop GPUs;
            // increasing this value further directly raises DXR payload cost.
            MaxPayloadSizeInBytes: 96,
            MaxAttributeSizeInBytes: 8,
        };
        let global_root = D3D12_GLOBAL_ROOT_SIGNATURE {
            pGlobalRootSignature: ManuallyDrop::new(Some(root_signature.clone())),
        };
        let pipeline_config = D3D12_RAYTRACING_PIPELINE_CONFIG {
            MaxTraceRecursionDepth: 4,
        };
        let subobjects = [
            D3D12_STATE_SUBOBJECT {
                Type: D3D12_STATE_SUBOBJECT_TYPE_DXIL_LIBRARY,
                pDesc: (&library as *const D3D12_DXIL_LIBRARY_DESC).cast(),
            },
            D3D12_STATE_SUBOBJECT {
                Type: D3D12_STATE_SUBOBJECT_TYPE_HIT_GROUP,
                pDesc: (&hit_group as *const D3D12_HIT_GROUP_DESC).cast(),
            },
            D3D12_STATE_SUBOBJECT {
                Type: D3D12_STATE_SUBOBJECT_TYPE_RAYTRACING_SHADER_CONFIG,
                pDesc: (&shader_config as *const D3D12_RAYTRACING_SHADER_CONFIG).cast(),
            },
            D3D12_STATE_SUBOBJECT {
                Type: D3D12_STATE_SUBOBJECT_TYPE_GLOBAL_ROOT_SIGNATURE,
                pDesc: (&global_root as *const D3D12_GLOBAL_ROOT_SIGNATURE).cast(),
            },
            D3D12_STATE_SUBOBJECT {
                Type: D3D12_STATE_SUBOBJECT_TYPE_RAYTRACING_PIPELINE_CONFIG,
                pDesc: (&pipeline_config as *const D3D12_RAYTRACING_PIPELINE_CONFIG).cast(),
            },
        ];
        let description = D3D12_STATE_OBJECT_DESC {
            Type: D3D12_STATE_OBJECT_TYPE_RAYTRACING_PIPELINE,
            NumSubobjects: subobjects.len() as u32,
            pSubobjects: subobjects.as_ptr(),
        };
        let state_object: ID3D12StateObject = unsafe { device5.CreateStateObject(&description)? };
        let properties: ID3D12StateObjectProperties = state_object.cast()?;
        let raygen_name = wide("RayGen");
        let miss_name = wide("Miss");
        let shadow_miss_name = wide("ShadowMiss");
        let identifiers = [
            unsafe { properties.GetShaderIdentifier(PCWSTR(raygen_name.as_ptr())) },
            unsafe { properties.GetShaderIdentifier(PCWSTR(miss_name.as_ptr())) },
            unsafe { properties.GetShaderIdentifier(PCWSTR(shadow_miss_name.as_ptr())) },
            unsafe { properties.GetShaderIdentifier(PCWSTR(hit_group_name.as_ptr())) },
        ];
        let record_size = D3D12_RAYTRACING_SHADER_TABLE_BYTE_ALIGNMENT as usize;
        let mut table_bytes = vec![0_u8; record_size * identifiers.len()];
        for (index, identifier) in identifiers.into_iter().enumerate() {
            assert!(!identifier.is_null(), "DXR Shader 导出标识不存在");
            unsafe {
                std::ptr::copy_nonoverlapping(
                    identifier.cast::<u8>(),
                    table_bytes.as_mut_ptr().add(index * record_size),
                    D3D12_SHADER_IDENTIFIER_SIZE_IN_BYTES as usize,
                );
            }
        }
        let shader_table = create_upload_buffer(device, &table_bytes, "DXR Shader Table")?;
        let address = unsafe { shader_table.GetGPUVirtualAddress() };
        Ok(Self {
            state_object,
            root_signature,
            _shader_table: shader_table,
            raygen: D3D12_GPU_VIRTUAL_ADDRESS_RANGE {
                StartAddress: address,
                SizeInBytes: record_size as u64,
            },
            miss: D3D12_GPU_VIRTUAL_ADDRESS_RANGE_AND_STRIDE {
                StartAddress: address + record_size as u64,
                SizeInBytes: (record_size * 2) as u64,
                StrideInBytes: record_size as u64,
            },
            hit_group: D3D12_GPU_VIRTUAL_ADDRESS_RANGE_AND_STRIDE {
                StartAddress: address + (record_size * 3) as u64,
                SizeInBytes: record_size as u64,
                StrideInBytes: record_size as u64,
            },
        })
    }
}

fn create_raytracing_root_signature(device: &ID3D12Device) -> Result<ID3D12RootSignature> {
    let ranges = [
        D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
            NumDescriptors: 4,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: 0,
        },
        D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
            NumDescriptors: super::texture::MAX_TEXTURE_VIEWS as u32,
            BaseShaderRegister: 5,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: super::texture::DXR_TEXTURE_BASE as u32,
        },
        D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
            NumDescriptors: 20,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: super::texture::DXR_UAV_BASE as u32,
        },
    ];
    let sampler_range = D3D12_DESCRIPTOR_RANGE {
        RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER,
        NumDescriptors: crate::scene::MAX_SCENE_SAMPLERS as u32,
        BaseShaderRegister: 0,
        RegisterSpace: 0,
        OffsetInDescriptorsFromTableStart: 0,
    };
    let parameters = [
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: ranges.len() as u32,
                    pDescriptorRanges: ranges.as_ptr(),
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Constants: D3D12_ROOT_CONSTANTS {
                    ShaderRegister: 0,
                    RegisterSpace: 0,
                    Num32BitValues: 16,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_SRV,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Descriptor: D3D12_ROOT_DESCRIPTOR {
                    ShaderRegister: 4,
                    RegisterSpace: 0,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: &sampler_range,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
    ];
    let description = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: parameters.len() as u32,
        pParameters: parameters.as_ptr(),
        NumStaticSamplers: 0,
        pStaticSamplers: std::ptr::null(),
        Flags: D3D12_ROOT_SIGNATURE_FLAG_NONE,
    };
    let mut serialized: Option<ID3DBlob> = None;
    unsafe {
        D3D12SerializeRootSignature(
            &description,
            D3D_ROOT_SIGNATURE_VERSION_1,
            &mut serialized,
            None,
        )?;
    }
    let serialized = serialized.unwrap();
    let bytes = unsafe {
        std::slice::from_raw_parts(
            serialized.GetBufferPointer().cast::<u8>(),
            serialized.GetBufferSize(),
        )
    };
    unsafe { device.CreateRootSignature(0, bytes) }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cornell_box_geometry_has_consistent_primitive_metadata() {
        let scene = SceneAsset::cornell_box();
        scene.validate().unwrap();
        assert_eq!(
            scene
                .primitives
                .iter()
                .map(|primitive| primitive.vertices.len())
                .sum::<usize>(),
            72
        );
        assert_eq!(
            scene
                .primitives
                .iter()
                .map(|primitive| primitive.indices.len())
                .sum::<usize>(),
            108
        );
        let object_counts = (0_usize..8)
            .map(|id| {
                scene
                    .instances
                    .iter()
                    .filter(|instance| {
                        let object_id = if instance.primitive_index < 6 {
                            instance.primitive_index
                        } else if instance.primitive_index < 12 {
                            6
                        } else {
                            7
                        };
                        object_id == id
                    })
                    .map(|instance| scene.primitives[instance.primitive_index].indices.len() / 3)
                    .sum::<usize>()
            })
            .collect::<Vec<_>>();
        assert_eq!(object_counts, [2, 2, 2, 2, 2, 2, 12, 12]);
    }

    #[test]
    fn cornell_box_normals_stay_normalized_after_rotation() {
        let scene = SceneAsset::cornell_box();
        for vertex in scene
            .primitives
            .iter()
            .flat_map(|primitive| &primitive.vertices)
        {
            let length_squared = vertex.normal.iter().map(|value| value * value).sum::<f32>();
            assert!((length_squared - 1.0).abs() < 1.0e-5);
        }
    }

    fn ggx_d(no_h: f32, roughness: f32) -> f32 {
        let alpha = roughness * roughness;
        let alpha_squared = alpha * alpha;
        let denominator = no_h * no_h * (alpha_squared - 1.0) + 1.0;
        alpha_squared / (std::f32::consts::PI * denominator * denominator).max(1.0e-7)
    }

    fn ggx_vndf_v3_pdf(no_v: f32, no_h: f32, roughness: f32) -> f32 {
        let alpha = roughness * roughness;
        let alpha_squared = alpha * alpha;
        let tangent = (1.0 - no_v * no_v).max(0.0).sqrt();
        let stretched_length = (alpha_squared * tangent * tangent + no_v * no_v).sqrt();
        let scale = 1.0 + tangent;
        let scale_squared = scale * scale;
        let bound = (1.0 - alpha_squared) * scale_squared
            / (scale_squared + alpha_squared * no_v * no_v).max(1.0e-6);
        0.5 * ggx_d(no_h, roughness) / (bound * no_v + stretched_length).max(1.0e-6)
    }

    fn sample_ggx_vndf_v3(
        view: glam::Vec3,
        roughness: f32,
        mut sample: glam::Vec2,
    ) -> (glam::Vec3, glam::Vec3) {
        sample.y *= 0.95;
        let alpha = roughness * roughness;
        let stretched_view = glam::Vec3::new(alpha * view.x, alpha * view.y, view.z).normalize();
        let phi = std::f32::consts::TAU * sample.x;
        let tangent = glam::Vec2::new(view.x, view.y).length();
        let scale = 1.0 + tangent;
        let alpha_squared = alpha * alpha;
        let scale_squared = scale * scale;
        let bound = (1.0 - alpha_squared) * scale_squared
            / (scale_squared + alpha_squared * view.z * view.z).max(1.0e-6);
        let lower_bound = bound * stretched_view.z;
        let disk_z = 1.0 - sample.y * (1.0 + lower_bound);
        let disk_radius = (1.0 - disk_z * disk_z).max(0.0).sqrt();
        let visible_normal =
            glam::Vec3::new(disk_radius * phi.cos(), disk_radius * phi.sin(), disk_z)
                + stretched_view;
        let half_vector = glam::Vec3::new(
            alpha * visible_normal.x,
            alpha * visible_normal.y,
            visible_normal.z.max(1.0e-6),
        )
        .normalize();
        let direction = (-view).reflect(half_vector).normalize();
        (half_vector, direction)
    }

    fn fresnel_schlick(cosine: f32, f0: f32) -> f32 {
        f0 + (1.0 - f0) * (1.0 - cosine.clamp(0.0, 1.0)).powi(5)
    }

    fn reflect_point_across_plane(
        point: glam::Vec3,
        normal: glam::Vec3,
        plane_distance: f32,
    ) -> glam::Vec3 {
        point - 2.0 * (normal.dot(point) - plane_distance) * normal
    }

    #[test]
    fn planar_psr_unfolding_is_an_involution() {
        let normal = glam::Vec3::Z;
        let physical_hit = glam::Vec3::new(0.25, -0.4, -3.0);
        let virtual_hit = reflect_point_across_plane(physical_hit, normal, 0.0);
        assert!((virtual_hit - glam::Vec3::new(0.25, -0.4, 3.0)).length() < 1.0e-6);
        let round_trip = reflect_point_across_plane(virtual_hit, normal, 0.0);
        assert!((round_trip - physical_hit).length() < 1.0e-6);
    }

    #[test]
    fn ggx_extremes_are_finite_non_negative_and_energy_bounded() {
        for roughness in [0.045, 0.1, 0.5, 1.0] {
            for no_h in [0.0, 0.001, 0.5, 1.0] {
                let distribution = ggx_d(no_h, roughness);
                assert!(distribution.is_finite() && distribution >= 0.0);
            }
        }
        for cosine in [0.0, 0.25, 0.75, 1.0] {
            let fresnel = fresnel_schlick(cosine, 0.04);
            assert!((0.04..=1.0).contains(&fresnel));
        }
        let metallic_diffuse_weight = (1.0 - 1.0) * (1.0 - fresnel_schlick(0.5, 0.9));
        let dielectric_diffuse_weight = (1.0 - 0.0) * (1.0 - fresnel_schlick(0.5, 0.04));
        assert!(metallic_diffuse_weight.abs() < 1.0e-6);
        assert!(dielectric_diffuse_weight > 0.0);
    }

    #[test]
    fn bounded_ggx_vndf_samples_and_pdfs_remain_finite() {
        for roughness in [0.045, 0.1, 0.5, 1.0] {
            for no_v in [0.001_f32, 0.05, 0.25, 0.75, 1.0] {
                let view = glam::Vec3::new((1.0 - no_v * no_v).sqrt(), 0.0, no_v);
                for y in 0..16 {
                    for x in 0..16 {
                        let sample =
                            glam::Vec2::new((x as f32 + 0.5) / 16.0, (y as f32 + 0.5) / 16.0);
                        let (half_vector, direction) = sample_ggx_vndf_v3(view, roughness, sample);
                        assert!(half_vector.is_finite() && direction.is_finite());
                        assert!(half_vector.z > 0.0);
                        assert!((half_vector.length() - 1.0).abs() < 1.0e-5);
                        assert!((direction.length() - 1.0).abs() < 1.0e-5);
                        let pdf = ggx_vndf_v3_pdf(no_v, half_vector.z, roughness);
                        assert!(pdf.is_finite() && pdf >= 0.0);
                    }
                }
            }
        }
    }

    #[test]
    fn nrd_bayer_lobe_pattern_covers_every_stratum() {
        let bayer = [0_u32, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
        for frame in 0..16_u32 {
            let mut strata = bayer.map(|value| (value + frame * 5) & 15);
            strata.sort_unstable();
            assert_eq!(
                strata,
                std::array::from_fn::<_, 16, _>(|index| index as u32)
            );
        }
        for pixel_value in bayer {
            let mut temporal =
                std::array::from_fn::<_, 16, _>(|frame| (pixel_value + frame as u32 * 5) & 15);
            temporal.sort_unstable();
            assert_eq!(
                temporal,
                std::array::from_fn::<_, 16, _>(|index| index as u32)
            );
        }
    }

    #[test]
    fn tlas_update_scratch_is_included_in_required_capacity() {
        let blas = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO {
            ScratchDataSizeInBytes: 64,
            ..Default::default()
        };
        let tlas = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO {
            ScratchDataSizeInBytes: 128,
            UpdateScratchDataSizeInBytes: 512,
            ..Default::default()
        };
        assert_eq!(
            policy_required_scratch_size(
                [blas.ScratchDataSizeInBytes],
                tlas.ScratchDataSizeInBytes,
                Some(tlas.UpdateScratchDataSizeInBytes),
            ),
            512
        );
    }

    #[test]
    fn allocation_query_failure_sentinel_is_rejected() {
        assert_eq!(valid_allocation_size(0), None);
        assert_eq!(valid_allocation_size(u64::MAX), None);
        assert_eq!(valid_allocation_size(65_536), Some(65_536));
    }

    #[test]
    fn static_scene_does_not_mark_acceleration_structure_dirty() {
        let base = [glam::Mat4::from_translation(glam::Vec3::new(2.0, 0.0, 0.0))];
        let groups = [AnimationGroup {
            instance_indices: vec![0],
            pivot_world: glam::Vec3::new(2.0, 0.0, 0.0),
        }];
        assert!(!animation_dirty(
            &base,
            &base,
            &groups,
            Duration::from_secs(1),
            false,
        ));
        assert!(animation_dirty(
            &base,
            &base,
            &groups,
            Duration::from_secs(1),
            true,
        ));
    }

    #[test]
    fn rigid_animation_rotates_in_place_around_world_pivot() {
        let pivot = glam::Vec3::new(2.0, 0.5, -1.0);
        let first = pivot + glam::Vec3::new(1.0, 0.0, 0.0);
        let second = pivot + glam::Vec3::new(0.0, 0.0, 2.0);
        let original_distance = (first - pivot).length();
        let original_relative_distance = (first - second).length();
        for angle in [0.0, 0.5 * std::f32::consts::PI, std::f32::consts::PI] {
            let transform = animation_transform(pivot, angle);
            let transformed_first = transform.transform_point3(first);
            let transformed_second = transform.transform_point3(second);
            assert!((transformed_first.distance(pivot) - original_distance).abs() < 1.0e-5);
            assert!(
                (transformed_first.distance(transformed_second) - original_relative_distance).abs()
                    < 1.0e-5
            );
            assert!((transform.transform_point3(pivot) - pivot).length() < 1.0e-5);
        }
    }

    #[test]
    fn instance_flags_depend_only_on_material_sidedness() {
        let material = crate::scene::MaterialAsset::opaque("single", [1.0; 4]);
        assert_eq!(compute_instance_flags(&material), 2);

        let mut double_sided = material.clone();
        double_sided.double_sided = true;
        assert_eq!(compute_instance_flags(&double_sided), 3);

        let mut dielectric = material;
        dielectric.kind = MaterialKind::LegacyDielectric;
        assert_eq!(compute_instance_flags(&dielectric), 3);
    }

    #[test]
    fn shader_rays_cull_single_sided_backfaces_without_dropping_transmission() {
        let shader = include_str!("../../../shaders/stage3_triangle.hlsl");
        assert_eq!(
            shader
                .matches("RAY_FLAG_CULL_BACK_FACING_TRIANGLES")
                .count(),
            5,
            "primary, bounce and shadow rays in both shader paths must use the same culling rule"
        );
        assert!(!shader.contains("TraceRay(Scene, RAY_FLAG_NONE"));
        assert!(shader.contains("if (!sampledTransmission && dot(normal, direction) <= 0.0)"));
    }

    fn split_first_bounce(
        diffuse_brdf: glam::Vec3,
        specular_brdf: glam::Vec3,
        no_l: f32,
        mixture_pdf: f32,
        child_radiance: glam::Vec3,
    ) -> (glam::Vec3, glam::Vec3, glam::Vec3) {
        let scale = no_l / mixture_pdf.max(1.0e-6);
        let diffuse = diffuse_brdf * scale * child_radiance;
        let specular = specular_brdf * scale * child_radiance;
        (diffuse, specular, diffuse + specular)
    }

    #[test]
    fn first_bounce_split_uses_one_mixture_pdf_for_both_lobes() {
        let (diffuse, specular, total) = split_first_bounce(
            glam::Vec3::splat(0.2),
            glam::Vec3::new(0.1, 0.3, 0.5),
            0.7,
            0.4,
            glam::Vec3::new(2.0, 1.0, 0.5),
        );
        assert_eq!(total, diffuse + specular);
        for value in diffuse
            .to_array()
            .into_iter()
            .chain(specular.to_array())
            .chain(total.to_array())
        {
            assert!(value.is_finite() && value >= 0.0);
        }
    }

    #[test]
    fn first_bounce_metallic_diffuse_is_zero_for_any_proposal() {
        let base_color = glam::Vec3::new(0.8, 0.4, 0.2);
        for _proposal in 0..2 {
            let metallic_diffuse_brdf = base_color * (1.0 - 1.0);
            let (diffuse, _, _) = split_first_bounce(
                metallic_diffuse_brdf,
                glam::Vec3::splat(0.04),
                0.5,
                0.25,
                glam::Vec3::ONE,
            );
            assert!(diffuse.length_squared() < 1.0e-12);
        }
        let dielectric_diffuse_brdf = base_color * (1.0 - 0.0);
        let (diffuse, _, _) = split_first_bounce(
            dielectric_diffuse_brdf,
            glam::Vec3::splat(0.04),
            0.5,
            0.25,
            glam::Vec3::ONE,
        );
        assert!(diffuse.length_squared() > 0.0);
    }

    #[test]
    fn black_albedo_does_not_remove_unmodulated_emissive_signal() {
        let filtered_diffuse = glam::Vec3::new(10.0, 5.0, 2.0);
        let albedo = glam::Vec3::ZERO;
        let emissive = glam::Vec3::new(0.2, 0.4, 0.8);
        let final_color = filtered_diffuse * albedo + emissive;
        assert_eq!(final_color, emissive);
    }
}
