use windows::{
    Win32::Graphics::{Direct3D12::*, Dxgi::Common::*},
    core::Result,
};

use crate::resolution::Extent2D;

use super::{
    ATROUS_HISTORY_TABLE_BASES, ATROUS_PING_TO_PONG_BASES, ATROUS_PONG_TO_PING_BASES,
    DXR_UAV_REGISTER_COUNT, RECONSTRUCTION_DIFFUSE_HIT_DISTANCE_UAV_REGISTER,
    RECONSTRUCTION_PRIMARY_EMISSIVE_UAV_REGISTER,
    RECONSTRUCTION_SPECULAR_HIT_DISTANCE_UAV_REGISTER, SHADER_DESCRIPTOR_COUNT,
    TEMPORAL_TABLE_BASES, TONEMAP_TABLE_BASES, create_structured_srv, create_texture_uav,
    descriptor::DescriptorHeap,
    populate_texture_table,
    raytracing::{AccelerationStructures, SceneGeometry},
    resource::{TrackedResource, TransitionBatch},
    texture::{DXR_UAV_BASE, TextureSet},
};

#[cfg(feature = "nrd")]
use super::{NRD_COMPOSE_TABLE_BASE, NRD_PREP_TABLE_BASE};

pub(super) struct DenoiseHistory {
    pub(super) diffuse: TrackedResource,
    pub(super) specular: TrackedResource,
    pub(super) moments: TrackedResource,
    pub(super) normal_roughness: TrackedResource,
    pub(super) depth: TrackedResource,
    pub(super) length: TrackedResource,
    pub(super) id: TrackedResource,
    pub(super) world_position: TrackedResource,
    pub(super) hit_distance: TrackedResource,
}

impl DenoiseHistory {
    pub(super) fn collect_all(
        &mut self,
        batch: &mut TransitionBatch,
        state: D3D12_RESOURCE_STATES,
    ) {
        self.diffuse.collect_transition(batch, state);
        self.specular.collect_transition(batch, state);
        self.moments.collect_transition(batch, state);
        self.normal_roughness.collect_transition(batch, state);
        self.depth.collect_transition(batch, state);
        self.length.collect_transition(batch, state);
        self.id.collect_transition(batch, state);
        self.world_position.collect_transition(batch, state);
        self.hit_distance.collect_transition(batch, state);
    }
}

#[cfg(feature = "nrd")]
pub(super) struct NrdGenerationResources {
    pub(super) diffuse_input: TrackedResource,
    pub(super) specular_input: TrackedResource,
    pub(super) normal_roughness: TrackedResource,
    pub(super) motion: TrackedResource,
    pub(super) view_z: TrackedResource,
    pub(super) diffuse_factor: TrackedResource,
    pub(super) specular_factor: TrackedResource,
    pub(super) diffuse_output: TrackedResource,
    pub(super) specular_output: TrackedResource,
    pub(super) backend: super::NrdBackend,
}

/// All resources whose descriptors or dimensions depend on the current render
/// extent. Every generation owns its complete shader-visible heap so an extent
/// switch never overwrites descriptors that an in-flight frame may still use.
pub(super) struct RenderResourceGeneration {
    pub(super) id: u64,
    pub(super) output_extent: Extent2D,
    pub(super) render_extent: Extent2D,
    pub(super) shader_heap: DescriptorHeap,
    pub(super) display_output: TrackedResource,
    pub(super) raw_diffuse: TrackedResource,
    pub(super) raw_specular: TrackedResource,
    pub(super) gbuffer_albedo: TrackedResource,
    pub(super) gbuffer_normal_roughness: TrackedResource,
    pub(super) gbuffer_depth: TrackedResource,
    pub(super) gbuffer_motion: TrackedResource,
    pub(super) gbuffer_id: TrackedResource,
    pub(super) gbuffer_world_position: TrackedResource,
    pub(super) gbuffer_hit_distance: TrackedResource,
    pub(super) reconstruction_noisy_hdr: TrackedResource,
    pub(super) reconstruction_diffuse_albedo: TrackedResource,
    pub(super) reconstruction_specular_albedo: TrackedResource,
    pub(super) reconstruction_normal_roughness: TrackedResource,
    pub(super) reconstruction_view_z: TrackedResource,
    pub(super) reconstruction_motion: TrackedResource,
    pub(super) reconstruction_diffuse_hit_distance: TrackedResource,
    pub(super) reconstruction_specular_hit_distance: TrackedResource,
    pub(super) reconstruction_primary_emissive: TrackedResource,
    #[cfg(feature = "nrd")]
    pub(super) nrd: Option<NrdGenerationResources>,
    pub(super) nrd_validation: TrackedResource,
    pub(super) histories: [DenoiseHistory; 2],
    pub(super) filter_diffuse_ping: TrackedResource,
    pub(super) filter_diffuse_pong: TrackedResource,
    pub(super) filter_specular_ping: TrackedResource,
    pub(super) filter_specular_pong: TrackedResource,
    pub(super) rejection_mask: TrackedResource,
    pub(super) last_used_fence: u64,
}

#[derive(Clone, Copy)]
pub(super) struct RenderGenerationDesc {
    pub(super) output_extent: Extent2D,
    pub(super) render_extent: Extent2D,
    pub(super) id: u64,
    pub(super) with_nrd: bool,
}

impl RenderResourceGeneration {
    pub(super) fn new(
        device: &ID3D12Device,
        textures: &TextureSet,
        scene_geometry: &SceneGeometry,
        acceleration_structures: &AccelerationStructures,
        description: RenderGenerationDesc,
    ) -> Result<Self> {
        let RenderGenerationDesc {
            output_extent,
            render_extent,
            id,
            with_nrd,
        } = description;
        let shader_heap = DescriptorHeap::new(
            device,
            D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
            SHADER_DESCRIPTOR_COUNT,
            true,
        )?;
        unsafe {
            create_structured_srv(
                device,
                &shader_heap,
                1,
                scene_geometry.vertex_buffer(),
                scene_geometry.vertex_count(),
                48,
            );
            create_structured_srv(
                device,
                &shader_heap,
                2,
                scene_geometry.index_buffer(),
                scene_geometry.index_count(),
                4,
            );
            create_structured_srv(
                device,
                &shader_heap,
                3,
                scene_geometry.material_buffer(),
                scene_geometry.material_count(),
                64,
            );
            textures.write_srvs(device, &shader_heap);
            let tlas_view = D3D12_SHADER_RESOURCE_VIEW_DESC {
                Format: DXGI_FORMAT_UNKNOWN,
                ViewDimension: D3D12_SRV_DIMENSION_RAYTRACING_ACCELERATION_STRUCTURE,
                Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                    RaytracingAccelerationStructure: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_SRV {
                        Location: acceleration_structures.tlas.GetGPUVirtualAddress(),
                    },
                },
            };
            device.CreateShaderResourceView(None, Some(&tlas_view), shader_heap.cpu_handle(0));
        }

        let display_output = create_uav_texture(
            device,
            output_extent,
            DXGI_FORMAT_R8G8B8A8_UNORM,
            format!("代际 {id} Tone Map 显示输出"),
        )?;
        let raw_diffuse = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} 原始漫反射辐射亮度"),
        )?;
        let raw_specular = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} 原始未调制镜面和自发光信号"),
        )?;
        let gbuffer_albedo = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} 第一交点反照率和材质类别"),
        )?;
        let gbuffer_normal_roughness = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} 第一交点世界法线和粗糙度"),
        )?;
        let gbuffer_depth = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R32_FLOAT,
            format!("代际 {id} 第一交点线性距离"),
        )?;
        let gbuffer_motion = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16_FLOAT,
            format!("代际 {id} 当前到上一帧像素运动矢量"),
        )?;
        let gbuffer_id = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R32_UINT,
            format!("代际 {id} 第一交点实例和材质 ID"),
        )?;
        let gbuffer_world_position = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R32G32B32A32_FLOAT,
            format!("代际 {id} 第一交点世界位置"),
        )?;
        let gbuffer_hit_distance = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R32_FLOAT,
            format!("代际 {id} 镜面反射命中距离"),
        )?;
        let reconstruction_noisy_hdr = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} Reconstruction noisy HDR"),
        )?;
        let reconstruction_diffuse_albedo = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} Reconstruction diffuse albedo"),
        )?;
        let reconstruction_specular_albedo = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} Reconstruction specular albedo"),
        )?;
        let reconstruction_normal_roughness = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} Reconstruction normal roughness"),
        )?;
        let reconstruction_view_z = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R32_FLOAT,
            format!("代际 {id} Reconstruction linear viewZ"),
        )?;
        let reconstruction_motion = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} Reconstruction 2.5D motion old=new+MV"),
        )?;
        let reconstruction_diffuse_hit_distance = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R32_FLOAT,
            format!("代际 {id} Reconstruction diffuse hit distance"),
        )?;
        let reconstruction_specular_hit_distance = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R32_FLOAT,
            format!("代际 {id} Reconstruction specular hit distance"),
        )?;
        let reconstruction_primary_emissive = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} Reconstruction primary emissive"),
        )?;
        #[cfg(feature = "nrd")]
        let nrd = if with_nrd {
            Some(NrdGenerationResources {
                diffuse_input: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} NRD diffuse radiance hit distance"),
                )?,
                specular_input: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} NRD specular radiance hit distance"),
                )?,
                normal_roughness: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R10G10B10A2_UNORM,
                    format!("代际 {id} NRD packed normal roughness"),
                )?,
                motion: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} NRD 2.5D motion"),
                )?,
                view_z: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R32_FLOAT,
                    format!("代际 {id} NRD viewZ"),
                )?,
                diffuse_factor: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} NRD diffuse material factor"),
                )?,
                specular_factor: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} NRD specular material factor"),
                )?,
                diffuse_output: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} NRD diffuse output"),
                )?,
                specular_output: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} NRD specular output"),
                )?,
                backend: super::NrdBackend::new(device, render_extent)?,
            })
        } else {
            None
        };
        #[cfg(not(feature = "nrd"))]
        let _ = with_nrd;
        let nrd_validation = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R8G8B8A8_UNORM,
            format!("代际 {id} NRD validation"),
        )?;
        let histories = [
            create_history(device, render_extent, 0, id)?,
            create_history(device, render_extent, 1, id)?,
        ];
        let filter_diffuse_ping = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} À-Trous 漫反射 Ping"),
        )?;
        let filter_diffuse_pong = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} À-Trous 漫反射 Pong"),
        )?;
        let filter_specular_ping = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} À-Trous 镜面 Ping"),
        )?;
        let filter_specular_pong = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            format!("代际 {id} À-Trous 镜面 Pong"),
        )?;
        let rejection_mask = create_uav_texture(
            device,
            render_extent,
            DXGI_FORMAT_R32_UINT,
            format!("代际 {id} 时域历史拒绝原因"),
        )?;

        let mut generation = Self {
            id,
            output_extent,
            render_extent,
            shader_heap,
            display_output,
            raw_diffuse,
            raw_specular,
            gbuffer_albedo,
            gbuffer_normal_roughness,
            gbuffer_depth,
            gbuffer_motion,
            gbuffer_id,
            gbuffer_world_position,
            gbuffer_hit_distance,
            reconstruction_noisy_hdr,
            reconstruction_diffuse_albedo,
            reconstruction_specular_albedo,
            reconstruction_normal_roughness,
            reconstruction_view_z,
            reconstruction_motion,
            reconstruction_diffuse_hit_distance,
            reconstruction_specular_hit_distance,
            reconstruction_primary_emissive,
            #[cfg(feature = "nrd")]
            nrd,
            nrd_validation,
            histories,
            filter_diffuse_ping,
            filter_diffuse_pong,
            filter_specular_ping,
            filter_specular_pong,
            rejection_mask,
            last_used_fence: 0,
        };
        unsafe { generation.write_shader_views(device, textures) };
        Ok(generation)
    }

    unsafe fn write_shader_views(&mut self, device: &ID3D12Device, textures: &TextureSet) {
        unsafe { textures.write_srvs(device, &self.shader_heap) };
        let raw_diffuse = &self.raw_diffuse;
        let raw_specular = &self.raw_specular;
        let albedo = &self.gbuffer_albedo;
        let normal = &self.gbuffer_normal_roughness;
        let depth = &self.gbuffer_depth;
        let motion = &self.gbuffer_motion;
        let id = &self.gbuffer_id;
        let world_position = &self.gbuffer_world_position;
        let hit_distance = &self.gbuffer_hit_distance;
        let reconstruction_noisy_hdr = &self.reconstruction_noisy_hdr;
        let reconstruction_diffuse_albedo = &self.reconstruction_diffuse_albedo;
        let reconstruction_specular_albedo = &self.reconstruction_specular_albedo;
        let reconstruction_normal_roughness = &self.reconstruction_normal_roughness;
        let reconstruction_view_z = &self.reconstruction_view_z;
        let reconstruction_motion = &self.reconstruction_motion;
        let reconstruction_diffuse_hit_distance = &self.reconstruction_diffuse_hit_distance;
        let reconstruction_specular_hit_distance = &self.reconstruction_specular_hit_distance;
        let reconstruction_primary_emissive = &self.reconstruction_primary_emissive;
        let rejection = &self.rejection_mask;
        let display_output = &self.display_output;
        let diffuse_ping = &self.filter_diffuse_ping;
        let diffuse_pong = &self.filter_diffuse_pong;
        let specular_ping = &self.filter_specular_ping;
        let specular_pong = &self.filter_specular_pong;

        let dxr_uavs = [
            (0, raw_diffuse),
            (1, raw_specular),
            (2, albedo),
            (3, normal),
            (4, depth),
            (5, motion),
            (6, id),
            (7, world_position),
            (8, hit_distance),
            (9, reconstruction_noisy_hdr),
            (10, reconstruction_diffuse_albedo),
            (11, reconstruction_specular_albedo),
            (12, reconstruction_normal_roughness),
            (13, reconstruction_view_z),
            (14, reconstruction_motion),
            (
                RECONSTRUCTION_DIFFUSE_HIT_DISTANCE_UAV_REGISTER,
                reconstruction_diffuse_hit_distance,
            ),
            (
                RECONSTRUCTION_SPECULAR_HIT_DISTANCE_UAV_REGISTER,
                reconstruction_specular_hit_distance,
            ),
            (
                RECONSTRUCTION_PRIMARY_EMISSIVE_UAV_REGISTER,
                reconstruction_primary_emissive,
            ),
        ];
        debug_assert_eq!(dxr_uavs.len(), DXR_UAV_REGISTER_COUNT);
        for (register, resource) in dxr_uavs {
            unsafe {
                create_texture_uav(device, &self.shader_heap, DXR_UAV_BASE + register, resource)
            };
        }

        #[cfg(feature = "nrd")]
        if let Some(nrd) = self.nrd.as_ref() {
            let prep_srvs = [
                raw_diffuse,
                raw_specular,
                albedo,
                reconstruction_normal_roughness,
                reconstruction_view_z,
                reconstruction_motion,
                reconstruction_diffuse_hit_distance,
                reconstruction_specular_hit_distance,
                reconstruction_primary_emissive,
                reconstruction_diffuse_albedo,
                world_position,
            ];
            let prep_uavs = [
                &nrd.diffuse_input,
                &nrd.specular_input,
                &nrd.normal_roughness,
                &nrd.motion,
                &nrd.view_z,
                &nrd.diffuse_factor,
                &nrd.specular_factor,
            ];
            unsafe {
                populate_texture_table(
                    device,
                    &self.shader_heap,
                    NRD_PREP_TABLE_BASE,
                    &prep_srvs,
                    &prep_uavs,
                )
            };

            let compose_srvs = [
                &nrd.diffuse_output,
                &nrd.specular_output,
                &nrd.diffuse_factor,
                &nrd.specular_factor,
                reconstruction_primary_emissive,
            ];
            let compose_uavs = [&self.filter_diffuse_pong, &self.filter_specular_pong];
            unsafe {
                populate_texture_table(
                    device,
                    &self.shader_heap,
                    NRD_COMPOSE_TABLE_BASE,
                    &compose_srvs,
                    &compose_uavs,
                )
            };
        }

        for current_index in 0..2 {
            let previous_index = 1 - current_index;
            let current = &self.histories[current_index];
            let previous = &self.histories[previous_index];
            let temporal_srvs = [
                raw_diffuse,
                raw_specular,
                albedo,
                normal,
                depth,
                motion,
                id,
                world_position,
                hit_distance,
                &previous.diffuse,
                &previous.specular,
                &previous.moments,
                &previous.normal_roughness,
                &previous.depth,
                &previous.length,
                &previous.id,
                &previous.world_position,
                &previous.hit_distance,
            ];
            let temporal_uavs = [
                &current.diffuse,
                &current.specular,
                &current.moments,
                &current.normal_roughness,
                &current.depth,
                &current.length,
                &current.id,
                &current.world_position,
                &current.hit_distance,
                rejection,
            ];
            unsafe {
                populate_texture_table(
                    device,
                    &self.shader_heap,
                    TEMPORAL_TABLE_BASES[current_index],
                    &temporal_srvs,
                    &temporal_uavs,
                )
            };

            let atrous_auxiliary = [
                normal,
                depth,
                &current.moments,
                id,
                &current.length,
                hit_distance,
            ];
            let write_atrous_table =
                |base: usize,
                 source_diffuse: &TrackedResource,
                 source_specular: &TrackedResource,
                 destination_diffuse: &TrackedResource,
                 destination_specular: &TrackedResource| {
                    let srvs = [
                        source_diffuse,
                        source_specular,
                        atrous_auxiliary[0],
                        atrous_auxiliary[1],
                        atrous_auxiliary[2],
                        atrous_auxiliary[3],
                        atrous_auxiliary[4],
                        atrous_auxiliary[5],
                    ];
                    let uavs = [destination_diffuse, destination_specular];
                    unsafe {
                        populate_texture_table(device, &self.shader_heap, base, &srvs, &uavs)
                    };
                };
            write_atrous_table(
                ATROUS_HISTORY_TABLE_BASES[current_index],
                &current.diffuse,
                &current.specular,
                diffuse_ping,
                specular_ping,
            );
            write_atrous_table(
                ATROUS_PING_TO_PONG_BASES[current_index],
                diffuse_ping,
                specular_ping,
                diffuse_pong,
                specular_pong,
            );
            write_atrous_table(
                ATROUS_PONG_TO_PING_BASES[current_index],
                diffuse_pong,
                specular_pong,
                diffuse_ping,
                specular_ping,
            );

            let tonemap_srvs = [
                diffuse_pong,
                specular_pong,
                raw_diffuse,
                raw_specular,
                albedo,
                normal,
                depth,
                motion,
                &current.moments,
                rejection,
                &current.length,
                id,
                hit_distance,
                &self.nrd_validation,
            ];
            unsafe {
                populate_texture_table(
                    device,
                    &self.shader_heap,
                    TONEMAP_TABLE_BASES[current_index],
                    &tonemap_srvs,
                    &[display_output],
                )
            };
        }
    }
}

fn create_uav_texture(
    device: &ID3D12Device,
    extent: Extent2D,
    format: DXGI_FORMAT,
    name: impl Into<String>,
) -> Result<TrackedResource> {
    TrackedResource::create_texture_2d(
        device,
        extent.width,
        extent.height,
        format,
        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        name,
    )
}

fn create_history(
    device: &ID3D12Device,
    extent: Extent2D,
    index: usize,
    generation: u64,
) -> Result<DenoiseHistory> {
    let label = |value: &str| format!("代际 {generation} 历史 {index} {value}");
    Ok(DenoiseHistory {
        diffuse: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            label("解调漫反射"),
        )?,
        specular: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            label("镜面信号"),
        )?,
        moments: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            label("漫反射和镜面一二阶矩"),
        )?,
        normal_roughness: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            label("法线和粗糙度"),
        )?,
        depth: create_uav_texture(device, extent, DXGI_FORMAT_R32_FLOAT, label("线性深度"))?,
        length: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R32G32_UINT,
            label("漫反射和镜面历史长度"),
        )?,
        id: create_uav_texture(device, extent, DXGI_FORMAT_R32_UINT, label("实例和材质 ID"))?,
        world_position: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R32G32B32A32_FLOAT,
            label("世界位置"),
        )?,
        hit_distance: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R32_FLOAT,
            label("镜面命中距离"),
        )?,
    })
}
