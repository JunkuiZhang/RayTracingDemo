use std::{
    ffi::c_void,
    mem::{ManuallyDrop, size_of},
    ptr::NonNull,
};

use windows::{
    Win32::Graphics::{Direct3D::ID3DBlob, Direct3D12::*, Dxgi::Common::*},
    core::{Interface, PCWSTR, Result},
};

use crate::scene::{
    GpuMaterial, GpuVertex, InstanceGpu, MATERIAL_FLAG_DOUBLE_SIDED,
    MATERIAL_FLAG_LEGACY_DIELECTRIC, MaterialKind, SceneAsset,
};

#[derive(Clone, Copy)]
struct PrimitiveRange {
    vertex_offset: u32,
    index_offset: u32,
    vertex_count: u32,
    index_count: u32,
}

/// Cornell Box 的网格资源，包含逐顶点法线和逐三角形材质索引。
pub struct SceneGeometry {
    vertex_buffer: ID3D12Resource,
    index_buffer: ID3D12Resource,
    material_buffer: ID3D12Resource,
    instance_buffer: ID3D12Resource,
    vertex_count: u32,
    index_count: u32,
    material_count: u32,
    instance_count: u32,
    primitive_ranges: Vec<PrimitiveRange>,
    instance_transforms: Vec<[f32; 12]>,
    instance_primitive_indices: Vec<usize>,
    upload_buffers: Vec<ID3D12Resource>,
}

/// 保持 BLAS、TLAS 及其构建依赖资源存活。
pub struct AccelerationStructures {
    pub tlas: ID3D12Resource,
    _blas: Vec<ID3D12Resource>,
    build_scratch: Option<ID3D12Resource>,
    _instance_buffer: Option<ID3D12Resource>,
}

impl SceneGeometry {
    pub fn new(
        device: &ID3D12Device,
        command_list: &ID3D12GraphicsCommandList,
        scene: &SceneAsset,
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
        let materials = scene.materials.iter().map(gpu_material).collect::<Vec<_>>();
        let instances = scene
            .instances
            .iter()
            .enumerate()
            .map(|(index, instance)| {
                let range = primitive_ranges[instance.primitive_index];
                InstanceGpu {
                    previous_object_to_world_row0: matrix_rows(instance.previous_world)[0],
                    previous_object_to_world_row1: matrix_rows(instance.previous_world)[1],
                    previous_object_to_world_row2: matrix_rows(instance.previous_world)[2],
                    vertex_offset: range.vertex_offset,
                    index_offset: range.index_offset,
                    material_index: scene.primitives[instance.primitive_index].material_index
                        as u32,
                    stable_surface_id: instance.stable_id.max(index as u32),
                }
            })
            .collect::<Vec<_>>();
        let instance_transforms = scene
            .instances
            .iter()
            .map(|instance| matrix_rows(instance.current_world))
            .map(|rows| {
                [
                    rows[0][0], rows[0][1], rows[0][2], rows[0][3], rows[1][0], rows[1][1],
                    rows[1][2], rows[1][3], rows[2][0], rows[2][1], rows[2][2], rows[2][3],
                ]
            })
            .collect();
        let instance_primitive_indices = scene
            .instances
            .iter()
            .map(|instance| instance.primitive_index)
            .collect();
        let (vertex_buffer, vertex_upload) =
            create_static_buffer(device, command_list, &vertices, "Cornell Box 顶点")?;
        let (index_buffer, index_upload) =
            create_static_buffer(device, command_list, &indices, "Cornell Box 索引")?;
        let (material_buffer, material_upload) =
            create_static_buffer(device, command_list, &materials, "场景材质")?;
        let (instance_buffer, instance_upload) =
            create_static_buffer(device, command_list, &instances, "场景实例元数据")?;
        Ok(Self {
            vertex_buffer,
            index_buffer,
            material_buffer,
            instance_buffer,
            vertex_count: vertices.len() as u32,
            index_count: indices.len() as u32,
            material_count: materials.len() as u32,
            instance_count: instances.len() as u32,
            primitive_ranges,
            instance_transforms,
            instance_primitive_indices,
            upload_buffers: vec![
                vertex_upload,
                index_upload,
                material_upload,
                instance_upload,
            ],
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

    pub fn instance_buffer(&self) -> &ID3D12Resource {
        &self.instance_buffer
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

    pub fn instance_count(&self) -> u32 {
        self.instance_count
    }

    pub fn primitive_count(&self) -> usize {
        self.primitive_ranges.len()
    }

    pub fn instance_descriptors(
        &self,
        blas: &[ID3D12Resource],
    ) -> Vec<D3D12_RAYTRACING_INSTANCE_DESC> {
        self.instance_transforms
            .iter()
            .enumerate()
            .map(|(index, transform)| D3D12_RAYTRACING_INSTANCE_DESC {
                Transform: *transform,
                _bitfield1: (index as u32 & 0x00FF_FFFF) | (0xFF << 24),
                _bitfield2: 0,
                AccelerationStructure: unsafe {
                    blas[self.instance_primitive_indices[index]].GetGPUVirtualAddress()
                },
            })
            .collect()
    }
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

fn gpu_material(material: &crate::scene::MaterialAsset) -> GpuMaterial {
    let mut flags = 0;
    if material.double_sided {
        flags |= MATERIAL_FLAG_DOUBLE_SIDED;
    }
    if material.kind == MaterialKind::LegacyDielectric {
        flags |= MATERIAL_FLAG_LEGACY_DIELECTRIC;
    }
    GpuMaterial {
        base_color_factor: material.base_color_factor,
        emissive_factor: material.emissive_factor,
        metallic_factor: material.metallic_factor,
        roughness_factor: material.roughness_factor,
        normal_scale: material.normal_scale,
        ior: material.ior,
        flags,
        base_color_texture_and_sampler: 0,
        metallic_roughness_texture_and_sampler: 0,
        normal_texture_and_sampler: 0,
        emissive_texture_and_sampler: 0,
    }
}

impl AccelerationStructures {
    pub fn build(
        device: &ID3D12Device,
        command_list: &ID3D12GraphicsCommandList,
        geometry: &SceneGeometry,
    ) -> Result<Self> {
        let device5: ID3D12Device5 = device.cast()?;
        let command_list4: ID3D12GraphicsCommandList4 = command_list.cast()?;
        let mut blas = Vec::with_capacity(geometry.primitive_count());
        let mut blas_infos = Vec::with_capacity(geometry.primitive_count());
        let mut scratch_size = 0_u64;
        for primitive_index in 0..geometry.primitive_count() {
            let geometry_desc = geometry.geometry_desc(primitive_index);
            let blas_inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
                Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
                Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
                NumDescs: 1,
                DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 {
                    pGeometryDescs: &geometry_desc,
                },
            };
            let mut info = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
            unsafe {
                device5.GetRaytracingAccelerationStructurePrebuildInfo(&blas_inputs, &mut info);
            }
            scratch_size = scratch_size.max(info.ScratchDataSizeInBytes);
            blas_infos.push(info);
            blas.push(create_default_buffer(
                device,
                info.ResultDataMaxSizeInBytes,
                D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
            )?);
        }
        let instance_descs = geometry.instance_descriptors(&blas);
        let instance_buffer = create_upload_buffer(device, &instance_descs, "DXR 场景实例")?;
        let tlas_inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
            Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_ALLOW_UPDATE
                | D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
            NumDescs: instance_descs.len() as u32,
            DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
            Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 {
                InstanceDescs: unsafe { instance_buffer.GetGPUVirtualAddress() },
            },
        };
        let mut tlas_info = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
        unsafe {
            device5.GetRaytracingAccelerationStructurePrebuildInfo(&tlas_inputs, &mut tlas_info);
        }
        let tlas = create_default_buffer(
            device,
            tlas_info.ResultDataMaxSizeInBytes,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
        )?;
        scratch_size = scratch_size.max(tlas_info.ScratchDataSizeInBytes);
        let scratch = create_default_buffer(
            device,
            scratch_size,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        )?;

        for (primitive_index, blas_resource) in blas.iter().enumerate() {
            let geometry_desc = geometry.geometry_desc(primitive_index);
            let blas_inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
                Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
                Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
                NumDescs: 1,
                DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
                Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 {
                    pGeometryDescs: &geometry_desc,
                },
            };
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
        Ok(Self {
            tlas,
            _blas: blas,
            build_scratch: Some(scratch),
            _instance_buffer: Some(instance_buffer),
        })
    }

    /// BLAS scratch is only needed until the initial AS build fence completes;
    /// the instance upload remains alive for the TLAS update path.
    pub fn release_build_resources(&mut self) {
        self.build_scratch = None;
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
    let description = D3D12_RESOURCE_DESC {
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
    };
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
        D3D12_RESOURCE_STATE_COPY_DEST,
    )?;
    set_resource_name(&default, name)?;
    let upload = create_upload_buffer(device, values, &format!("{name} Upload"))?;
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
            MaxPayloadSizeInBytes: 32,
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
            NumDescriptors: 5,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: 0,
        },
        D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
            NumDescriptors: 9,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: 5,
        },
    ];
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
}
