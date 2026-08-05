use std::{ffi::c_void, ptr::NonNull};

use windows::{
    Win32::Graphics::{Direct3D12::*, Dxgi::Common::*},
    core::Result,
};

/// 持久映射的逐帧上传缓冲，每个 Frame Context 使用独立的 256 字节区域。
pub struct UploadRing {
    resource: ID3D12Resource,
    mapped: NonNull<u8>,
    frame_stride: usize,
    frame_count: usize,
}

impl UploadRing {
    pub fn new(device: &ID3D12Device, frame_count: usize) -> Result<Self> {
        let frame_stride = D3D12_CONSTANT_BUFFER_DATA_PLACEMENT_ALIGNMENT as usize;
        let total_size = frame_stride * frame_count;
        let heap_properties = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_UPLOAD,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 0,
            VisibleNodeMask: 0,
        };
        let description = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0,
            Width: total_size as u64,
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
                &heap_properties,
                D3D12_HEAP_FLAG_NONE,
                &description,
                D3D12_RESOURCE_STATE_GENERIC_READ,
                None,
                &mut resource,
            )?;
        }
        let resource: ID3D12Resource = resource.unwrap();
        let mut mapped = std::ptr::null_mut::<c_void>();
        unsafe { resource.Map(0, None, Some(&mut mapped))? };
        Ok(Self {
            resource,
            mapped: NonNull::new(mapped.cast()).unwrap(),
            frame_stride,
            frame_count,
        })
    }

    pub fn write<T: Copy>(&mut self, frame_index: usize, value: &T) -> u64 {
        assert!(frame_index < self.frame_count, "上传帧索引越界");
        assert!(
            size_of::<T>() <= self.frame_stride,
            "常量数据超过单帧上传空间"
        );
        let offset = frame_index * self.frame_stride;
        unsafe {
            std::ptr::copy_nonoverlapping(
                (value as *const T).cast::<u8>(),
                self.mapped.as_ptr().add(offset),
                size_of::<T>(),
            );
            self.resource.GetGPUVirtualAddress() + offset as u64
        }
    }
}

impl Drop for UploadRing {
    fn drop(&mut self) {
        unsafe { self.resource.Unmap(0, None) };
    }
}
