use std::{
    ffi::c_void,
    mem::{ManuallyDrop, size_of},
    ptr::NonNull,
};

use windows::{
    Win32::Graphics::{Direct3D::ID3DBlob, Direct3D12::*, Dxgi::Common::*},
    core::{Interface, PCWSTR, Result},
};

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Material {
    albedo: [f32; 4],
    emission_and_kind: [f32; 4],
}

/// Cornell Box 的网格资源，包含逐顶点法线和逐三角形材质索引。
pub struct SceneGeometry {
    vertex_buffer: ID3D12Resource,
    index_buffer: ID3D12Resource,
    material_index_buffer: ID3D12Resource,
    material_buffer: ID3D12Resource,
    vertex_count: u32,
    index_count: u32,
}

/// 保持 BLAS、TLAS 及其构建依赖资源存活。
pub struct AccelerationStructures {
    pub tlas: ID3D12Resource,
    _blas: ID3D12Resource,
    _scratch: ID3D12Resource,
    _instance_buffer: ID3D12Resource,
}

impl SceneGeometry {
    pub fn new(device: &ID3D12Device) -> Result<Self> {
        let (vertices, indices, material_indices) = create_cornell_box();
        let materials = create_materials();
        Ok(Self {
            vertex_buffer: create_upload_buffer(device, &vertices, "Cornell Box 顶点")?,
            index_buffer: create_upload_buffer(device, &indices, "Cornell Box 索引")?,
            material_index_buffer: create_upload_buffer(
                device,
                &material_indices,
                "Cornell Box 材质索引",
            )?,
            material_buffer: create_upload_buffer(device, &materials, "Cornell Box 材质")?,
            vertex_count: vertices.len() as u32,
            index_count: indices.len() as u32,
        })
    }

    /// 返回构建 BLAS 时使用的三角形描述。
    pub fn geometry_desc(&self) -> D3D12_RAYTRACING_GEOMETRY_DESC {
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
                    IndexCount: self.index_count,
                    VertexCount: self.vertex_count,
                    IndexBuffer: index_buffer,
                    VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE {
                        StartAddress: vertex_buffer,
                        StrideInBytes: size_of::<Vertex>() as u64,
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

    pub fn material_index_buffer(&self) -> &ID3D12Resource {
        &self.material_index_buffer
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
}

fn create_materials() -> [Material; 6] {
    [
        Material {
            albedo: [0.73, 0.73, 0.73, 1.0],
            emission_and_kind: [0.0, 0.0, 0.0, 0.0],
        },
        Material {
            albedo: [0.65, 0.05, 0.05, 1.0],
            emission_and_kind: [0.0, 0.0, 0.0, 0.0],
        },
        Material {
            albedo: [0.12, 0.45, 0.15, 1.0],
            emission_and_kind: [0.0, 0.0, 0.0, 0.0],
        },
        Material {
            albedo: [1.0, 1.0, 1.0, 1.0],
            emission_and_kind: [14.0, 12.0, 9.0, 3.0],
        },
        Material {
            albedo: [0.82, 0.85, 0.9, 1.0],
            emission_and_kind: [0.0, 0.0, 0.0, 1.0],
        },
        Material {
            albedo: [0.98, 0.98, 0.98, 1.0],
            emission_and_kind: [0.0, 0.0, 0.0, 2.0],
        },
    ]
}

fn create_cornell_box() -> (Vec<Vertex>, Vec<u32>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut materials = Vec::new();
    add_quad(
        &mut vertices,
        &mut indices,
        &mut materials,
        [
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, -1.0, 2.0],
            [-1.0, -1.0, 2.0],
        ],
        [0.0, 1.0, 0.0],
        0,
    );
    add_quad(
        &mut vertices,
        &mut indices,
        &mut materials,
        [
            [-1.0, 2.0, 2.0],
            [1.0, 2.0, 2.0],
            [1.0, 2.0, 0.0],
            [-1.0, 2.0, 0.0],
        ],
        [0.0, -1.0, 0.0],
        0,
    );
    add_quad(
        &mut vertices,
        &mut indices,
        &mut materials,
        [
            [-1.0, -1.0, 2.0],
            [1.0, -1.0, 2.0],
            [1.0, 2.0, 2.0],
            [-1.0, 2.0, 2.0],
        ],
        [0.0, 0.0, -1.0],
        0,
    );
    add_quad(
        &mut vertices,
        &mut indices,
        &mut materials,
        [
            [-1.0, -1.0, 0.0],
            [-1.0, -1.0, 2.0],
            [-1.0, 2.0, 2.0],
            [-1.0, 2.0, 0.0],
        ],
        [1.0, 0.0, 0.0],
        1,
    );
    add_quad(
        &mut vertices,
        &mut indices,
        &mut materials,
        [
            [1.0, -1.0, 2.0],
            [1.0, -1.0, 0.0],
            [1.0, 2.0, 0.0],
            [1.0, 2.0, 2.0],
        ],
        [-1.0, 0.0, 0.0],
        2,
    );
    add_quad(
        &mut vertices,
        &mut indices,
        &mut materials,
        [
            [-0.35, 1.98, 0.75],
            [0.35, 1.98, 0.75],
            [0.35, 1.98, 1.35],
            [-0.35, 1.98, 1.35],
        ],
        [0.0, -1.0, 0.0],
        3,
    );
    add_box(
        &mut vertices,
        &mut indices,
        &mut materials,
        [-0.75, -1.0, 0.55],
        [-0.1, 0.1, 1.2],
        4,
    );
    add_box(
        &mut vertices,
        &mut indices,
        &mut materials,
        [0.15, -1.0, 1.0],
        [0.75, 0.65, 1.65],
        5,
    );
    (vertices, indices, materials)
}

fn add_box(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    materials: &mut Vec<u32>,
    min: [f32; 3],
    max: [f32; 3],
    material: u32,
) {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    add_quad(
        vertices,
        indices,
        materials,
        [[x0, y0, z0], [x1, y0, z0], [x1, y1, z0], [x0, y1, z0]],
        [0.0, 0.0, -1.0],
        material,
    );
    add_quad(
        vertices,
        indices,
        materials,
        [[x1, y0, z1], [x0, y0, z1], [x0, y1, z1], [x1, y1, z1]],
        [0.0, 0.0, 1.0],
        material,
    );
    add_quad(
        vertices,
        indices,
        materials,
        [[x0, y0, z1], [x0, y0, z0], [x0, y1, z0], [x0, y1, z1]],
        [-1.0, 0.0, 0.0],
        material,
    );
    add_quad(
        vertices,
        indices,
        materials,
        [[x1, y0, z0], [x1, y0, z1], [x1, y1, z1], [x1, y1, z0]],
        [1.0, 0.0, 0.0],
        material,
    );
    add_quad(
        vertices,
        indices,
        materials,
        [[x0, y1, z0], [x1, y1, z0], [x1, y1, z1], [x0, y1, z1]],
        [0.0, 1.0, 0.0],
        material,
    );
    add_quad(
        vertices,
        indices,
        materials,
        [[x0, y0, z1], [x1, y0, z1], [x1, y0, z0], [x0, y0, z0]],
        [0.0, -1.0, 0.0],
        material,
    );
}

fn add_quad(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    materials: &mut Vec<u32>,
    positions: [[f32; 3]; 4],
    normal: [f32; 3],
    material: u32,
) {
    let base = vertices.len() as u32;
    vertices.extend(positions.map(|position| Vertex { position, normal }));
    indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    materials.extend([material, material]);
}

impl AccelerationStructures {
    pub fn build(
        device: &ID3D12Device,
        command_list: &ID3D12GraphicsCommandList,
        geometry: &SceneGeometry,
    ) -> Result<Self> {
        let device5: ID3D12Device5 = device.cast()?;
        let command_list4: ID3D12GraphicsCommandList4 = command_list.cast()?;
        let geometry_desc = geometry.geometry_desc();
        let blas_inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL,
            Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
            NumDescs: 1,
            DescsLayout: D3D12_ELEMENTS_LAYOUT_ARRAY,
            Anonymous: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0 {
                pGeometryDescs: &geometry_desc,
            },
        };
        let mut blas_info = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
        unsafe {
            device5.GetRaytracingAccelerationStructurePrebuildInfo(&blas_inputs, &mut blas_info);
        }
        let blas = create_default_buffer(
            device,
            blas_info.ResultDataMaxSizeInBytes,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE,
        )?;

        let instance = D3D12_RAYTRACING_INSTANCE_DESC {
            Transform: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            _bitfield1: 0xFF << 24,
            _bitfield2: 0,
            AccelerationStructure: unsafe { blas.GetGPUVirtualAddress() },
        };
        let instance_buffer =
            create_upload_buffer(device, std::slice::from_ref(&instance), "DXR 场景实例")?;
        let tlas_inputs = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
            Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL,
            Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PREFER_FAST_TRACE,
            NumDescs: 1,
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
        let scratch_size = blas_info
            .ScratchDataSizeInBytes
            .max(tlas_info.ScratchDataSizeInBytes);
        let scratch = create_default_buffer(
            device,
            scratch_size,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        )?;

        let blas_build = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
            DestAccelerationStructureData: unsafe { blas.GetGPUVirtualAddress() },
            Inputs: blas_inputs,
            SourceAccelerationStructureData: 0,
            ScratchAccelerationStructureData: unsafe { scratch.GetGPUVirtualAddress() },
        };
        unsafe {
            command_list4.BuildRaytracingAccelerationStructure(&blas_build, None);
        }
        uav_barrier(command_list, &blas);
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
            _scratch: scratch,
            _instance_buffer: instance_buffer,
        })
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
        Width: (size_of::<T>() * values.len()) as u64,
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
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    unsafe {
        resource.SetName(windows::core::PCWSTR(wide.as_ptr()))?;
    }
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
        let identifiers = [
            unsafe { properties.GetShaderIdentifier(PCWSTR(raygen_name.as_ptr())) },
            unsafe { properties.GetShaderIdentifier(PCWSTR(miss_name.as_ptr())) },
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
                SizeInBytes: record_size as u64,
                StrideInBytes: record_size as u64,
            },
            hit_group: D3D12_GPU_VIRTUAL_ADDRESS_RANGE_AND_STRIDE {
                StartAddress: address + (record_size * 2) as u64,
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
            NumDescriptors: 1,
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
                    Num32BitValues: 1,
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
