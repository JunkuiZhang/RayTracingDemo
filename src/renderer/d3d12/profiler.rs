use std::ffi::c_void;

use windows::{
    Win32::Graphics::{Direct3D12::*, Dxgi::Common::*},
    core::Result,
};

/// 使用 Timestamp Query 统计 GPU Pass 耗时，结果在 Frame Context 再次复用时读取。
pub struct GpuProfiler {
    query_heap: ID3D12QueryHeap,
    readback: ID3D12Resource,
    timestamp_frequency: u64,
    frame_count: usize,
    last_time_ms: f64,
}

impl GpuProfiler {
    pub fn new(
        device: &ID3D12Device,
        command_queue: &ID3D12CommandQueue,
        frame_count: usize,
    ) -> Result<Self> {
        let query_description = D3D12_QUERY_HEAP_DESC {
            Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
            Count: (frame_count * 2) as u32,
            NodeMask: 0,
        };
        let mut query_heap = None;
        unsafe { device.CreateQueryHeap(&query_description, &mut query_heap)? };

        let heap_properties = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_READBACK,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 0,
            VisibleNodeMask: 0,
        };
        let resource_description = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0,
            Width: (frame_count * 2 * size_of::<u64>()) as u64,
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
        let mut readback = None;
        unsafe {
            device.CreateCommittedResource(
                &heap_properties,
                D3D12_HEAP_FLAG_NONE,
                &resource_description,
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut readback,
            )?;
        }
        Ok(Self {
            query_heap: query_heap.unwrap(),
            readback: readback.unwrap(),
            timestamp_frequency: unsafe { command_queue.GetTimestampFrequency()? },
            frame_count,
            last_time_ms: 0.0,
        })
    }

    pub fn begin(&self, command_list: &ID3D12GraphicsCommandList, frame_index: usize) {
        assert!(frame_index < self.frame_count, "GPU 计时帧索引越界");
        unsafe {
            command_list.EndQuery(
                &self.query_heap,
                D3D12_QUERY_TYPE_TIMESTAMP,
                (frame_index * 2) as u32,
            );
        }
    }

    pub fn end(&self, command_list: &ID3D12GraphicsCommandList, frame_index: usize) {
        let query_index = (frame_index * 2) as u32;
        unsafe {
            command_list.EndQuery(
                &self.query_heap,
                D3D12_QUERY_TYPE_TIMESTAMP,
                query_index + 1,
            );
            command_list.ResolveQueryData(
                &self.query_heap,
                D3D12_QUERY_TYPE_TIMESTAMP,
                query_index,
                2,
                &self.readback,
                query_index as u64 * size_of::<u64>() as u64,
            );
        }
    }

    pub fn collect(&mut self, frame_index: usize) -> Result<()> {
        let byte_offset = frame_index * 2 * size_of::<u64>();
        let read_range = D3D12_RANGE {
            Begin: byte_offset,
            End: byte_offset + 2 * size_of::<u64>(),
        };
        let mut mapped = std::ptr::null_mut::<c_void>();
        unsafe {
            self.readback.Map(0, Some(&read_range), Some(&mut mapped))?;
            let timestamps =
                std::slice::from_raw_parts(mapped.cast::<u64>().add(frame_index * 2), 2);
            if timestamps[1] >= timestamps[0] && timestamps[0] != 0 {
                self.last_time_ms = (timestamps[1] - timestamps[0]) as f64 * 1000.0
                    / self.timestamp_frequency as f64;
            }
            self.readback
                .Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
        }
        Ok(())
    }

    pub fn last_time_ms(&self) -> f64 {
        self.last_time_ms
    }
}
