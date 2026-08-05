use std::mem::ManuallyDrop;

use windows::Win32::Graphics::{Direct3D12::*, Dxgi::Common::DXGI_FORMAT};

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
        if self.state == new_state {
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
}
