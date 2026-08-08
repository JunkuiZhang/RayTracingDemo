use std::{collections::HashMap, ffi::c_void, mem::ManuallyDrop, ptr::NonNull};

use windows::{
    Win32::Graphics::{Direct3D12::*, Dxgi::Common::*},
    core::{PCWSTR, Result},
};

use crate::scene::{ImageAsset, MaterialAsset, TextureBindingAsset};

/// t5 的 bindless 纹理数组预留的描述符数量。
pub const MAX_TEXTURE_VIEWS: usize = 128;
/// DXR 描述符表中 t5 纹理数组的起始位置。
pub const DXR_TEXTURE_BASE: usize = 4;
/// DXR UAV 紧跟在纹理数组之后，避免破坏阶段 6 的表布局。
pub const DXR_UAV_BASE: usize = DXR_TEXTURE_BASE + MAX_TEXTURE_VIEWS;

const FALLBACK_COUNT: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TextureView {
    image_index: usize,
    srgb: bool,
}

struct GpuImage {
    resource: ID3D12Resource,
    upload: Option<ID3D12Resource>,
}

/// glTF 图片的 GPU 资源和 SRV 视图。
///
/// 一个 glTF image 只有一个底层 typeless 资源，sRGB/线性差异只通过
/// SRV format 表达，避免为同一张图片复制 GPU 资源。上传资源在初始化
/// fence 完成前保留，之后由 `release_uploads` 释放。
pub struct TextureSet {
    images: Vec<GpuImage>,
    views: Vec<TextureView>,
    lookup: HashMap<TextureView, usize>,
    fallback_base_color: usize,
    fallback_metallic_roughness: usize,
    fallback_normal: usize,
    fallback_emissive: usize,
}

impl TextureSet {
    pub fn new(
        device: &ID3D12Device,
        command_list: &ID3D12GraphicsCommandList,
        images: &[ImageAsset],
        materials: &[MaterialAsset],
    ) -> Result<Self> {
        let fallback_images = [
            ("Fallback BaseColor sRGB", [255, 255, 255, 255]),
            // glTF metallic-roughness: G=roughness=1, B=metallic=0.
            ("Fallback MetallicRoughness 线性", [0, 255, 0, 255]),
            ("Fallback Normal 线性", [128, 128, 255, 255]),
            ("Fallback Emissive sRGB", [0, 0, 0, 255]),
        ];
        let mut gpu_images = Vec::with_capacity(FALLBACK_COUNT + images.len());
        for (name, rgba8) in fallback_images {
            gpu_images.push(upload_image(
                device,
                command_list,
                &ImageAsset {
                    name: name.to_string(),
                    width: 1,
                    height: 1,
                    rgba8: rgba8.to_vec(),
                },
            )?);
        }
        for image in images {
            gpu_images.push(upload_image(device, command_list, image)?);
        }

        let mut texture_set = Self {
            images: gpu_images,
            views: Vec::with_capacity(MAX_TEXTURE_VIEWS),
            lookup: HashMap::new(),
            fallback_base_color: 0,
            fallback_metallic_roughness: 0,
            fallback_normal: 0,
            fallback_emissive: 0,
        };
        texture_set.fallback_base_color = texture_set.ensure_view(0, true)?;
        texture_set.fallback_metallic_roughness = texture_set.ensure_view(1, false)?;
        texture_set.fallback_normal = texture_set.ensure_view(2, false)?;
        texture_set.fallback_emissive = texture_set.ensure_view(3, true)?;
        for material in materials {
            texture_set.ensure_material_views(material)?;
        }
        Ok(texture_set)
    }

    fn ensure_material_views(&mut self, material: &MaterialAsset) -> Result<()> {
        if let Some(binding) = material.base_color_texture {
            self.ensure_image_view(binding.image_index, true)?;
        }
        if let Some(binding) = material.metallic_roughness_texture {
            self.ensure_image_view(binding.image_index, false)?;
        }
        if let Some(binding) = material.normal_texture {
            self.ensure_image_view(binding.image_index, false)?;
        }
        if let Some(binding) = material.emissive_texture {
            self.ensure_image_view(binding.image_index, true)?;
        }
        Ok(())
    }

    fn ensure_image_view(&mut self, image_index: usize, srgb: bool) -> Result<usize> {
        // 前四个资源是角色 fallback，glTF image 从 FALLBACK_COUNT 开始。
        let gpu_image_index = image_index.checked_add(FALLBACK_COUNT).ok_or_else(|| {
            windows::core::Error::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                format!("纹理索引 {image_index} 溢出"),
            )
        })?;
        if gpu_image_index >= self.images.len() {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                format!("纹理索引 {image_index} 超出场景图片数量"),
            ));
        }
        self.ensure_view(gpu_image_index, srgb)
    }

    fn ensure_view(&mut self, image_index: usize, srgb: bool) -> Result<usize> {
        let view = TextureView { image_index, srgb };
        if let Some(&index) = self.lookup.get(&view) {
            return Ok(index);
        }
        if self.views.len() >= MAX_TEXTURE_VIEWS {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                format!("场景需要超过 {MAX_TEXTURE_VIEWS} 个纹理 SRV 视图，已拒绝继续创建"),
            ));
        }
        let index = self.views.len();
        self.views.push(view);
        self.lookup.insert(view, index);
        Ok(index)
    }

    pub fn material_texture_indices(&self, material: &MaterialAsset) -> [u32; 4] {
        [
            self.lookup_or_fallback(material.base_color_texture, true, self.fallback_base_color),
            self.lookup_or_fallback(
                material.metallic_roughness_texture,
                false,
                self.fallback_metallic_roughness,
            ),
            self.lookup_or_fallback(material.normal_texture, false, self.fallback_normal),
            self.lookup_or_fallback(material.emissive_texture, true, self.fallback_emissive),
        ]
    }

    fn lookup_or_fallback(
        &self,
        binding: Option<TextureBindingAsset>,
        srgb: bool,
        fallback: usize,
    ) -> u32 {
        binding
            .and_then(|binding| {
                self.lookup
                    .get(&TextureView {
                        image_index: binding.image_index + FALLBACK_COUNT,
                        srgb,
                    })
                    .copied()
            })
            .unwrap_or(fallback) as u32
    }

    /// Populate the complete t5 texture array, including unused entries.
    /// Unused descriptors deliberately point to linear white so an invalid
    /// material index cannot read an uninitialized descriptor.
    pub unsafe fn write_srvs(
        &self,
        device: &ID3D12Device,
        heap: &crate::renderer::d3d12::descriptor::DescriptorHeap,
    ) {
        let unused = TextureView {
            image_index: 0,
            srgb: false,
        };
        for index in 0..MAX_TEXTURE_VIEWS {
            let view = self.views.get(index).copied().unwrap_or(unused);
            let format = if view.srgb {
                DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
            } else {
                DXGI_FORMAT_R8G8B8A8_UNORM
            };
            let description = D3D12_SHADER_RESOURCE_VIEW_DESC {
                Format: format,
                ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D12_TEX2D_SRV {
                        MostDetailedMip: 0,
                        MipLevels: 1,
                        PlaneSlice: 0,
                        ResourceMinLODClamp: 0.0,
                    },
                },
            };
            unsafe {
                device.CreateShaderResourceView(
                    Some(&self.images[view.image_index].resource),
                    Some(&description),
                    heap.cpu_handle(DXR_TEXTURE_BASE + index),
                );
            }
        }
    }

    /// Initialization upload resources are no longer needed after the init fence.
    pub fn release_uploads(&mut self) {
        for image in &mut self.images {
            image.upload = None;
        }
    }
}

fn upload_image(
    device: &ID3D12Device,
    command_list: &ID3D12GraphicsCommandList,
    image: &ImageAsset,
) -> Result<GpuImage> {
    if image.width == 0 || image.height == 0 {
        return Err(windows::core::Error::new(
            windows::core::HRESULT(0x80004005_u32 as i32),
            format!("图片 {} 的尺寸不能为 0", image.name),
        ));
    }
    let expected_size = image.width as usize * image.height as usize * 4;
    if image.rgba8.len() != expected_size {
        return Err(windows::core::Error::new(
            windows::core::HRESULT(0x80004005_u32 as i32),
            format!(
                "图片 {} 的 RGBA8 数据大小 {} 不匹配 {}x{}",
                image.name,
                image.rgba8.len(),
                image.width,
                image.height
            ),
        ));
    }

    let description = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: image.width as u64,
        Height: image.height,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_R8G8B8A8_TYPELESS,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: D3D12_RESOURCE_FLAG_NONE,
    };
    let default_heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
        MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
        CreationNodeMask: 0,
        VisibleNodeMask: 0,
    };
    let mut resource = None;
    unsafe {
        device.CreateCommittedResource(
            &default_heap,
            D3D12_HEAP_FLAG_NONE,
            &description,
            D3D12_RESOURCE_STATE_COPY_DEST,
            None,
            &mut resource,
        )?;
    }
    let resource = resource.unwrap();
    set_resource_name(&resource, &image.name)?;

    let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
    let mut row_count = 0;
    let mut row_size = 0;
    let mut total_bytes = 0;
    unsafe {
        device.GetCopyableFootprints(
            &description,
            0,
            1,
            0,
            Some(&mut footprint),
            Some(&mut row_count),
            Some(&mut row_size),
            Some(&mut total_bytes),
        );
    }
    let upload = create_upload_buffer(
        device,
        total_bytes as usize,
        &format!("{} Upload", image.name),
    )?;
    let mut mapped = std::ptr::null_mut::<c_void>();
    unsafe { upload.Map(0, None, Some(&mut mapped))? };
    let mapped = NonNull::new(mapped.cast::<u8>()).unwrap();
    let row_pitch = footprint.Footprint.RowPitch as usize;
    let source_row_pitch = image.width as usize * 4;
    for row in 0..image.height as usize {
        unsafe {
            std::ptr::copy_nonoverlapping(
                image.rgba8.as_ptr().add(row * source_row_pitch),
                mapped
                    .as_ptr()
                    .add(footprint.Offset as usize + row * row_pitch),
                source_row_pitch,
            );
        }
    }
    unsafe { upload.Unmap(0, None) };

    let mut source = D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(upload.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            PlacedFootprint: footprint,
        },
    };
    let mut destination = D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(resource.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            SubresourceIndex: 0,
        },
    };
    unsafe {
        command_list.CopyTextureRegion(&destination, 0, 0, 0, &source, None);
        ManuallyDrop::drop(&mut source.pResource);
        ManuallyDrop::drop(&mut destination.pResource);
    }
    transition_texture(
        command_list,
        &resource,
        D3D12_RESOURCE_STATE_COPY_DEST,
        D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
    );
    Ok(GpuImage {
        resource,
        upload: Some(upload),
    })
}

fn create_upload_buffer(device: &ID3D12Device, size: usize, name: &str) -> Result<ID3D12Resource> {
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
        Width: size.max(1) as u64,
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
    let resource = resource.unwrap();
    set_resource_name(&resource, name)?;
    Ok(resource)
}

fn transition_texture(
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
    let wide = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    unsafe { resource.SetName(PCWSTR(wide.as_ptr())) }
}
