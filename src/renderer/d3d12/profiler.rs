use std::ffi::c_void;

use windows::{
    Win32::Graphics::{Direct3D12::*, Dxgi::Common::*},
    core::Result,
};

#[derive(Clone, Copy)]
#[repr(usize)]
pub enum GpuPass {
    Total = 0,
    PathTrace = 1,
    Temporal = 2,
    Atrous = 3,
    ToneMap = 4,
}

const PASS_COUNT: usize = 5;
const TIMESTAMPS_PER_FRAME: usize = PASS_COUNT * 2;

/// Timestamp profiler with an independent begin/end pair for every GPU pass.
/// Results are read only after the owning frame context fence has completed.
pub struct GpuProfiler {
    query_heap: ID3D12QueryHeap,
    readback: ID3D12Resource,
    timestamp_frequency: u64,
    frame_count: usize,
    last_times_ms: [f64; PASS_COUNT],
}

impl GpuProfiler {
    pub fn new(
        device: &ID3D12Device,
        command_queue: &ID3D12CommandQueue,
        frame_count: usize,
    ) -> Result<Self> {
        let query_description = D3D12_QUERY_HEAP_DESC {
            Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
            Count: (frame_count * TIMESTAMPS_PER_FRAME) as u32,
            NodeMask: 0,
        };
        let mut query_heap = None;
        unsafe { device.CreateQueryHeap(&query_description, &mut query_heap)? };

        let heap_properties = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_READBACK,
            ..Default::default()
        };
        let resource_description = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: (frame_count * TIMESTAMPS_PER_FRAME * size_of::<u64>()) as u64,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_UNKNOWN,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
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
            last_times_ms: [0.0; PASS_COUNT],
        })
    }

    pub fn begin(
        &self,
        command_list: &ID3D12GraphicsCommandList,
        frame_index: usize,
        pass: GpuPass,
    ) {
        self.write_timestamp(command_list, frame_index, pass, 0);
    }

    pub fn end(&self, command_list: &ID3D12GraphicsCommandList, frame_index: usize, pass: GpuPass) {
        self.write_timestamp(command_list, frame_index, pass, 1);
    }

    fn write_timestamp(
        &self,
        command_list: &ID3D12GraphicsCommandList,
        frame_index: usize,
        pass: GpuPass,
        endpoint: usize,
    ) {
        assert!(frame_index < self.frame_count);
        let query = frame_index * TIMESTAMPS_PER_FRAME + pass as usize * 2 + endpoint;
        unsafe {
            command_list.EndQuery(&self.query_heap, D3D12_QUERY_TYPE_TIMESTAMP, query as u32);
        }
    }

    pub fn resolve_frame(&self, command_list: &ID3D12GraphicsCommandList, frame_index: usize) {
        let query_start = frame_index * TIMESTAMPS_PER_FRAME;
        unsafe {
            command_list.ResolveQueryData(
                &self.query_heap,
                D3D12_QUERY_TYPE_TIMESTAMP,
                query_start as u32,
                TIMESTAMPS_PER_FRAME as u32,
                &self.readback,
                (query_start * size_of::<u64>()) as u64,
            );
        }
    }

    pub fn collect(&mut self, frame_index: usize) -> Result<()> {
        let timestamp_start = frame_index * TIMESTAMPS_PER_FRAME;
        let byte_start = timestamp_start * size_of::<u64>();
        let read_range = D3D12_RANGE {
            Begin: byte_start,
            End: byte_start + TIMESTAMPS_PER_FRAME * size_of::<u64>(),
        };
        let mut mapped = std::ptr::null_mut::<c_void>();
        unsafe {
            self.readback.Map(0, Some(&read_range), Some(&mut mapped))?;
            let timestamps = std::slice::from_raw_parts(
                mapped.cast::<u64>().add(timestamp_start),
                TIMESTAMPS_PER_FRAME,
            );
            for pass in 0..PASS_COUNT {
                let begin = timestamps[pass * 2];
                let end = timestamps[pass * 2 + 1];
                if end >= begin && begin != 0 {
                    self.last_times_ms[pass] =
                        (end - begin) as f64 * 1000.0 / self.timestamp_frequency as f64;
                }
            }
            self.readback
                .Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
        }
        Ok(())
    }

    pub fn time_ms(&self, pass: GpuPass) -> f64 {
        self.last_times_ms[pass as usize]
    }
}
