use std::{ffi::c_void, mem::size_of, ptr::NonNull};

use windows::{
    Win32::Graphics::{Direct3D12::*, Dxgi::Common::*},
    core::Result,
};

/// 阶段 3 的单三角形几何资源，顶点和索引暂存于上传堆。
pub struct TriangleGeometry {
    vertex_buffer: ID3D12Resource,
    index_buffer: ID3D12Resource,
}

impl TriangleGeometry {
    pub fn new(device: &ID3D12Device) -> Result<Self> {
        let vertices: [[f32; 3]; 3] = [[-0.8, -0.7, 0.0], [0.0, 0.8, 0.0], [0.8, -0.7, 0.0]];
        let indices: [u32; 3] = [0, 1, 2];
        Ok(Self {
            vertex_buffer: create_upload_buffer(device, &vertices, "DXR 三角形顶点")?,
            index_buffer: create_upload_buffer(device, &indices, "DXR 三角形索引")?,
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
                    IndexCount: 3,
                    VertexCount: 3,
                    IndexBuffer: index_buffer,
                    VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE {
                        StartAddress: vertex_buffer,
                        StrideInBytes: (3 * size_of::<f32>()) as u64,
                    },
                },
            },
        }
    }
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
