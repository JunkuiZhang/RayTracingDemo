use std::mem::ManuallyDrop;

use windows::{
    Win32::Graphics::{Direct3D12::*, Dxgi::Common::*},
    core::Result,
};

#[cfg(debug_assertions)]
use windows::core::Interface;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarrierSubmissionMode {
    Immediate,
    Batched,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BarrierSubmitStats {
    pub api_calls: u32,
    pub transitions: u32,
}

/// Reusable transition storage for one producer/consumer boundary.
///
/// Each barrier owns one temporary COM clone in the Windows union. The
/// explicit clear/drop path is therefore required; `Vec::clear` alone would
/// leak the `ManuallyDrop` field.
pub struct TransitionBatch {
    barriers: Vec<D3D12_RESOURCE_BARRIER>,
}

impl TransitionBatch {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            barriers: Vec::with_capacity(capacity),
        }
    }

    pub fn submit(
        &mut self,
        command_list: &ID3D12GraphicsCommandList,
        mode: BarrierSubmissionMode,
    ) -> BarrierSubmitStats {
        let transitions = self.barriers.len() as u32;
        if transitions == 0 {
            return BarrierSubmitStats::default();
        }

        let submit_stats = submission_stats(mode, transitions as usize);
        match mode {
            BarrierSubmissionMode::Immediate => {
                for barrier in &self.barriers {
                    unsafe { command_list.ResourceBarrier(std::slice::from_ref(barrier)) };
                }
            }
            BarrierSubmissionMode::Batched => {
                unsafe { command_list.ResourceBarrier(&self.barriers) };
            }
        }
        self.clear();
        submit_stats
    }

    fn push(
        &mut self,
        resource: ID3D12Resource,
        before: D3D12_RESOURCE_STATES,
        after: D3D12_RESOURCE_STATES,
    ) {
        #[cfg(debug_assertions)]
        debug_assert!(
            !self.barriers.iter().any(|barrier| {
                unsafe { barrier_resource_key(barrier) == Some(resource.as_raw() as usize) }
            }),
            "同一 TransitionBatch 不得重复收集同一资源"
        );
        self.barriers.push(D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: ManuallyDrop::new(Some(resource)),
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
        });
    }

    fn clear(&mut self) {
        for mut barrier in self.barriers.drain(..) {
            unsafe {
                ManuallyDrop::drop(&mut (*barrier.Anonymous.Transition).pResource);
            }
        }
    }
}

impl Drop for TransitionBatch {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(debug_assertions)]
unsafe fn barrier_resource_key(barrier: &D3D12_RESOURCE_BARRIER) -> Option<usize> {
    unsafe {
        (*barrier.Anonymous.Transition)
            .pResource
            .as_ref()
            .map(|resource| resource.as_raw() as usize)
    }
}

/// 记录资源及其当前状态，所有状态转换统一从这里发出。
#[allow(dead_code)]
pub struct TrackedResource {
    resource: ID3D12Resource,
    state: D3D12_RESOURCE_STATES,
    format: DXGI_FORMAT,
    width: u32,
    height: u32,
    name: String,
}

#[allow(dead_code)]
impl TrackedResource {
    pub fn create_texture_2d(
        device: &ID3D12Device,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
        flags: D3D12_RESOURCE_FLAGS,
        state: D3D12_RESOURCE_STATES,
        name: impl Into<String>,
    ) -> Result<Self> {
        let heap_properties = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 0,
            VisibleNodeMask: 0,
        };
        let description = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Alignment: 0,
            Width: width as u64,
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: flags,
        };
        let mut resource = None;
        unsafe {
            device.CreateCommittedResource(
                &heap_properties,
                D3D12_HEAP_FLAG_NONE,
                &description,
                state,
                None,
                &mut resource,
            )?;
        }
        Ok(Self::new(
            resource.unwrap(),
            state,
            format,
            width,
            height,
            name,
        ))
    }

    pub fn new(
        resource: ID3D12Resource,
        state: D3D12_RESOURCE_STATES,
        format: DXGI_FORMAT,
        width: u32,
        height: u32,
        name: impl Into<String>,
    ) -> Self {
        Self {
            resource,
            state,
            format,
            width,
            height,
            name: name.into(),
        }
    }

    pub fn resource(&self) -> &ID3D12Resource {
        &self.resource
    }

    pub fn format(&self) -> DXGI_FORMAT {
        self.format
    }

    pub fn state(&self) -> D3D12_RESOURCE_STATES {
        self.state
    }

    /// Synchronize the CPU tracker after an external recorder (such as NRD)
    /// has emitted its own barriers on this resource.
    pub fn set_known_state_after_external_recording(&mut self, state: D3D12_RESOURCE_STATES) {
        self.state = state;
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn transition(
        &mut self,
        command_list: &ID3D12GraphicsCommandList,
        new_state: D3D12_RESOURCE_STATES,
    ) {
        if !transition_needed(self.state, new_state) {
            return;
        }

        let mut barrier = D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: ManuallyDrop::new(Some(self.resource.clone())),
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: self.state,
                    StateAfter: new_state,
                }),
            },
        };
        unsafe {
            command_list.ResourceBarrier(std::slice::from_ref(&barrier));
            // 联合体和资源字段均为 ManuallyDrop，提交后释放临时 COM 引用。
            ManuallyDrop::drop(&mut (*barrier.Anonymous.Transition).pResource);
        }
        self.state = new_state;
    }

    pub fn collect_transition(
        &mut self,
        batch: &mut TransitionBatch,
        new_state: D3D12_RESOURCE_STATES,
    ) {
        if !transition_needed(self.state, new_state) {
            return;
        }
        let before = self.state;
        batch.push(self.resource.clone(), before, new_state);
        self.state = new_state;
    }
}

fn transition_needed(before: D3D12_RESOURCE_STATES, after: D3D12_RESOURCE_STATES) -> bool {
    before != after
}

fn submission_stats(mode: BarrierSubmissionMode, transitions: usize) -> BarrierSubmitStats {
    if transitions == 0 {
        return BarrierSubmitStats::default();
    }
    BarrierSubmitStats {
        api_calls: match mode {
            BarrierSubmissionMode::Immediate => transitions as u32,
            BarrierSubmissionMode::Batched => 1,
        },
        transitions: transitions as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_op_transition_is_skipped_without_a_submission() {
        assert!(!transition_needed(
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS
        ));
        assert_eq!(
            submission_stats(BarrierSubmissionMode::Batched, 0),
            BarrierSubmitStats::default()
        );
    }

    #[test]
    fn empty_batch_has_zero_submission_stats() {
        assert_eq!(
            submission_stats(BarrierSubmissionMode::Immediate, 0),
            BarrierSubmitStats::default()
        );
    }

    #[test]
    fn baseline_and_batched_submission_stats_count_calls_and_elements() {
        assert_eq!(
            submission_stats(BarrierSubmissionMode::Immediate, 3),
            BarrierSubmitStats {
                api_calls: 3,
                transitions: 3,
            }
        );
        assert_eq!(
            submission_stats(BarrierSubmissionMode::Batched, 3),
            BarrierSubmitStats {
                api_calls: 1,
                transitions: 3,
            }
        );
    }
}
