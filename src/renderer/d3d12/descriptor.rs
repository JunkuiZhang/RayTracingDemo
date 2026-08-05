use windows::{Win32::Graphics::Direct3D12::*, core::Result};

/// 统一管理一块连续的 DX12 描述符堆。
#[allow(dead_code)]
pub struct DescriptorHeap {
    heap: ID3D12DescriptorHeap,
    descriptor_size: usize,
    descriptor_count: usize,
    shader_visible: bool,
}

#[allow(dead_code)]
impl DescriptorHeap {
    pub fn new(
        device: &ID3D12Device,
        heap_type: D3D12_DESCRIPTOR_HEAP_TYPE,
        descriptor_count: usize,
        shader_visible: bool,
    ) -> Result<Self> {
        let description = D3D12_DESCRIPTOR_HEAP_DESC {
            Type: heap_type,
            NumDescriptors: descriptor_count as u32,
            Flags: if shader_visible {
                D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE
            } else {
                D3D12_DESCRIPTOR_HEAP_FLAG_NONE
            },
            NodeMask: 0,
        };
        let heap = unsafe { device.CreateDescriptorHeap(&description)? };
        let descriptor_size =
            unsafe { device.GetDescriptorHandleIncrementSize(heap_type) as usize };
        Ok(Self {
            heap,
            descriptor_size,
            descriptor_count,
            shader_visible,
        })
    }

    pub fn heap(&self) -> &ID3D12DescriptorHeap {
        &self.heap
    }

    pub fn cpu_handle(&self, index: usize) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        assert!(index < self.descriptor_count, "描述符索引越界");
        let start = unsafe { self.heap.GetCPUDescriptorHandleForHeapStart() };
        D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: start.ptr + index * self.descriptor_size,
        }
    }

    pub fn gpu_handle(&self, index: usize) -> D3D12_GPU_DESCRIPTOR_HANDLE {
        assert!(self.shader_visible, "非 Shader 可见堆没有 GPU 句柄");
        assert!(index < self.descriptor_count, "描述符索引越界");
        let start = unsafe { self.heap.GetGPUDescriptorHandleForHeapStart() };
        D3D12_GPU_DESCRIPTOR_HANDLE {
            ptr: start.ptr + (index * self.descriptor_size) as u64,
        }
    }
}
