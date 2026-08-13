use std::{ffi::c_void, mem::ManuallyDrop};

use windows::{
    Win32::Graphics::{Direct3D::ID3DBlob, Direct3D12::*},
    core::{Interface, PCWSTR, Result},
};

/// Compute pipeline with one contiguous SRV/UAV descriptor table and a small
/// block of root constants. Descriptor ranges never alias in the table.
pub struct ComputePipeline {
    root_signature: ID3D12RootSignature,
    pipeline_state: ID3D12PipelineState,
    constant_count: usize,
    root_srv_parameter: Option<u32>,
}

impl ComputePipeline {
    pub fn new(
        device: &ID3D12Device,
        shader: &[u8],
        srv_count: u32,
        uav_count: u32,
        constant_count: usize,
        name: &str,
    ) -> Result<Self> {
        Self::new_internal(
            device,
            shader,
            srv_count,
            uav_count,
            constant_count,
            None,
            name,
        )
    }

    pub fn new_with_root_srv(
        device: &ID3D12Device,
        shader: &[u8],
        srv_count: u32,
        uav_count: u32,
        constant_count: usize,
        root_srv_register: u32,
        name: &str,
    ) -> Result<Self> {
        Self::new_internal(
            device,
            shader,
            srv_count,
            uav_count,
            constant_count,
            Some(root_srv_register),
            name,
        )
    }

    fn new_internal(
        device: &ID3D12Device,
        shader: &[u8],
        srv_count: u32,
        uav_count: u32,
        constant_count: usize,
        root_srv_register: Option<u32>,
        name: &str,
    ) -> Result<Self> {
        assert!(srv_count > 0 || uav_count > 0);
        assert!(constant_count > 0 && constant_count <= 64);
        let root_signature = create_root_signature(
            device,
            srv_count,
            uav_count,
            constant_count as u32,
            root_srv_register,
        )?;
        let mut description = D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: ManuallyDrop::new(Some(root_signature.clone())),
            CS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: shader.as_ptr().cast::<c_void>(),
                BytecodeLength: shader.len(),
            },
            ..Default::default()
        };
        let pipeline_state = unsafe { device.CreateComputePipelineState(&description)? };
        unsafe { ManuallyDrop::drop(&mut description.pRootSignature) };
        set_name(&root_signature, &format!("{name} Root Signature"))?;
        set_name(&pipeline_state, name)?;
        Ok(Self {
            root_signature,
            pipeline_state,
            constant_count,
            root_srv_parameter: root_srv_register.map(|_| 2),
        })
    }

    pub fn bind_pipeline(&self, command_list: &ID3D12GraphicsCommandList) {
        unsafe {
            command_list.SetPipelineState(&self.pipeline_state);
            command_list.SetComputeRootSignature(&self.root_signature);
        }
    }

    pub fn set_arguments(
        &self,
        command_list: &ID3D12GraphicsCommandList,
        descriptor_table: D3D12_GPU_DESCRIPTOR_HANDLE,
        constants: &[u32],
    ) {
        assert_eq!(constants.len(), self.constant_count);
        unsafe {
            command_list.SetComputeRootDescriptorTable(0, descriptor_table);
            command_list.SetComputeRoot32BitConstants(
                1,
                constants.len() as u32,
                constants.as_ptr().cast(),
                0,
            );
        }
    }

    pub fn bind(
        &self,
        command_list: &ID3D12GraphicsCommandList,
        descriptor_table: D3D12_GPU_DESCRIPTOR_HANDLE,
        constants: &[u32],
    ) {
        self.bind_pipeline(command_list);
        self.set_arguments(command_list, descriptor_table, constants);
    }

    pub fn set_root_shader_resource_view(
        &self,
        command_list: &ID3D12GraphicsCommandList,
        gpu_address: u64,
    ) {
        let parameter = self
            .root_srv_parameter
            .expect("compute pipeline was not created with a root SRV");
        unsafe {
            command_list.SetComputeRootShaderResourceView(parameter, gpu_address);
        }
    }
}

fn create_root_signature(
    device: &ID3D12Device,
    srv_count: u32,
    uav_count: u32,
    constant_count: u32,
    root_srv_register: Option<u32>,
) -> Result<ID3D12RootSignature> {
    let mut ranges = Vec::with_capacity(2);
    if srv_count > 0 {
        ranges.push(D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
            NumDescriptors: srv_count,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: 0,
        });
    }
    if uav_count > 0 {
        ranges.push(D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
            NumDescriptors: uav_count,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: srv_count,
        });
    }
    let mut parameters = vec![
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
                    Num32BitValues: constant_count,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
    ];
    if let Some(shader_register) = root_srv_register {
        parameters.push(D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_SRV,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Descriptor: D3D12_ROOT_DESCRIPTOR {
                    ShaderRegister: shader_register,
                    RegisterSpace: 0,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        });
    }
    let description = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: parameters.len() as u32,
        pParameters: parameters.as_ptr(),
        NumStaticSamplers: 0,
        pStaticSamplers: std::ptr::null(),
        Flags: D3D12_ROOT_SIGNATURE_FLAG_NONE,
    };
    let mut serialized: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    unsafe {
        D3D12SerializeRootSignature(
            &description,
            D3D_ROOT_SIGNATURE_VERSION_1,
            &mut serialized,
            Some(&mut errors),
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

fn set_name<T: Interface>(object: &T, name: &str) -> Result<()> {
    let object: ID3D12Object = object.cast()?;
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    unsafe { object.SetName(PCWSTR(wide.as_ptr())) }
}
