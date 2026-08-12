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

#[cfg(feature = "streamline")]
use super::{DLSS_COMPOSE_TABLE_BASES, DLSS_TONEMAP_TABLE_BASE, create_null_texture_srv};
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
pub(super) struct NrdDenoiserResources {
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

#[cfg(feature = "nrd")]
pub(super) struct NrdGenerationResources {
    pub(super) primary: NrdDenoiserResources,
    pub(super) transmission: NrdDenoiserResources,
    // The refracted layer mirrors the primary reconstruction contract so its
    // material factors and temporal guides describe the surface behind glass,
    // never the glass interface itself.
    pub(super) transmission_raw_diffuse: TrackedResource,
    pub(super) transmission_raw_specular: TrackedResource,
    pub(super) transmission_base_color: TrackedResource,
    pub(super) transmission_normal_roughness: TrackedResource,
    pub(super) transmission_view_z: TrackedResource,
    pub(super) transmission_motion: TrackedResource,
    pub(super) transmission_diffuse_hit_distance: TrackedResource,
    pub(super) transmission_specular_hit_distance: TrackedResource,
    pub(super) transmission_primary_emissive: TrackedResource,
    pub(super) transmission_diffuse_albedo: TrackedResource,
    pub(super) transmission_view_proxy: TrackedResource,
}

#[cfg(feature = "streamline")]
#[allow(dead_code)] // 10D consumes the generation-owned resources in the DLSS pass.
pub(super) struct DlssGenerationResources {
    /// HDR signal composed from the active SVGF/NRD diffuse and specular
    /// outputs at the input extent.
    pub(super) input_hdr: TrackedResource,
    /// Streamline's output-resolution DLSS result before ToneMap.
    pub(super) output_hdr: TrackedResource,
    pub(super) exposure: TrackedResource,
    pub(super) depth: TrackedResource,
    pub(super) motion: TrackedResource,
}

#[cfg(feature = "streamline-rr")]
pub(super) struct RrGenerationResources {
    /// RR consumes the unfiltered noisy HDR signal directly. This output is
    /// the only HDR signal handed to ToneMap when the RR path is active.
    pub(super) output_hdr: TrackedResource,
    /// Adapter output: signed normalized world normal in RGB and linear
    /// roughness in A, matching DLSSD's packed normal/roughness contract.
    pub(super) normal_roughness: TrackedResource,
    /// Dense DLSS motion/depth are written by the DXR pass under the same
    /// guide contract as Stage 10, but remain generation-owned for RR.
    pub(super) depth: TrackedResource,
    pub(super) motion: TrackedResource,
    /// Dense motion of virtually reflected geometry. Supplying it directly
    /// avoids asking RR to reconstruct reflection motion from a jittered
    /// primary hit and one scalar hit distance.
    pub(super) specular_motion: TrackedResource,
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
    #[cfg(feature = "streamline")]
    #[allow(dead_code)] // 10D consumes the DLSS generation from its pass.
    pub(super) dlss: Option<DlssGenerationResources>,
    #[cfg(feature = "streamline-rr")]
    pub(super) rr: Option<RrGenerationResources>,
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
    pub(super) with_dlss_sr: bool,
    pub(super) with_dlss_rr: bool,
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
            with_dlss_sr,
            with_dlss_rr,
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
        #[cfg(feature = "streamline")]
        let dlss = if with_dlss_sr {
            Some(DlssGenerationResources {
                input_hdr: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} DLSS HDR 输入"),
                )?,
                output_hdr: create_uav_texture(
                    device,
                    output_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} DLSS HDR 输出"),
                )?,
                exposure: create_uav_texture(
                    device,
                    Extent2D {
                        width: 1,
                        height: 1,
                    },
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} DLSS exposure"),
                )?,
                depth: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R32_FLOAT,
                    format!("代际 {id} DLSS device depth"),
                )?,
                motion: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16_FLOAT,
                    format!("代际 {id} DLSS pixel motion"),
                )?,
            })
        } else {
            None
        };
        #[cfg(feature = "streamline")]
        let _ = with_dlss_rr;
        #[cfg(not(feature = "streamline"))]
        let _ = (with_dlss_sr, with_dlss_rr);
        #[cfg(feature = "streamline-rr")]
        let rr = if with_dlss_rr {
            Some(RrGenerationResources {
                output_hdr: create_uav_texture(
                    device,
                    output_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} DLSS RR HDR 输出"),
                )?,
                normal_roughness: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} DLSS RR packed normal roughness"),
                )?,
                depth: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R32_FLOAT,
                    format!("代际 {id} DLSS RR device depth"),
                )?,
                motion: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16_FLOAT,
                    format!("代际 {id} DLSS RR dense pixel motion"),
                )?,
                specular_motion: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16_FLOAT,
                    format!("代际 {id} DLSS RR dense specular motion"),
                )?,
            })
        } else {
            None
        };
        #[cfg(not(feature = "streamline-rr"))]
        let _ = with_dlss_rr;
        #[cfg(feature = "nrd")]
        let nrd = if with_nrd {
            Some(NrdGenerationResources {
                primary: create_nrd_denoiser_resources(device, render_extent, id, "primary")?,
                transmission: create_nrd_denoiser_resources(
                    device,
                    render_extent,
                    id,
                    "transmission",
                )?,
                transmission_raw_diffuse: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} 透射层原始漫反射"),
                )?,
                transmission_raw_specular: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} 透射层原始镜面"),
                )?,
                transmission_base_color: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} 透射层基础色和材质类别"),
                )?,
                transmission_normal_roughness: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} 透射层法线和粗糙度"),
                )?,
                transmission_view_z: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R32_FLOAT,
                    format!("代际 {id} 透射层 viewZ"),
                )?,
                transmission_motion: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} 透射层 2.5D motion"),
                )?,
                transmission_diffuse_hit_distance: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R32_FLOAT,
                    format!("代际 {id} 透射层 diffuse hit distance"),
                )?,
                transmission_specular_hit_distance: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R32_FLOAT,
                    format!("代际 {id} 透射层 specular hit distance"),
                )?,
                transmission_primary_emissive: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} 透射层 primary emissive"),
                )?,
                transmission_diffuse_albedo: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} 透射层 diffuse albedo"),
                )?,
                transmission_view_proxy: create_uav_texture(
                    device,
                    render_extent,
                    DXGI_FORMAT_R16G16B16A16_FLOAT,
                    format!("代际 {id} 透射层 view direction proxy"),
                )?,
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
            #[cfg(feature = "streamline")]
            dlss,
            #[cfg(feature = "streamline-rr")]
            rr,
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
        debug_assert_eq!(dxr_uavs.len(), super::DLSS_DEPTH_UAV_REGISTER);
        for (register, resource) in dxr_uavs {
            unsafe {
                create_texture_uav(device, &self.shader_heap, DXR_UAV_BASE + register, resource)
            };
        }
        // The DXR table remains ABI-compatible at u0..u19. Native generations
        // bind existing guides as inert fallback descriptors and the shader's
        // uniform DlssEnabled guard guarantees there are no extra UAV writes.
        #[cfg(feature = "streamline-rr")]
        let (dlss_depth, dlss_motion) = if let Some(rr) = self.rr.as_ref() {
            (&rr.depth, &rr.motion)
        } else if let Some(dlss) = self.dlss.as_ref() {
            (&dlss.depth, &dlss.motion)
        } else {
            (depth, motion)
        };
        #[cfg(all(feature = "streamline", not(feature = "streamline-rr")))]
        let (dlss_depth, dlss_motion) = self
            .dlss
            .as_ref()
            .map_or((depth, motion), |dlss| (&dlss.depth, &dlss.motion));
        #[cfg(not(feature = "streamline"))]
        let (dlss_depth, dlss_motion) = (depth, motion);
        unsafe {
            create_texture_uav(
                device,
                &self.shader_heap,
                DXR_UAV_BASE + super::DLSS_DEPTH_UAV_REGISTER,
                dlss_depth,
            );
            create_texture_uav(
                device,
                &self.shader_heap,
                DXR_UAV_BASE + super::DLSS_MOTION_UAV_REGISTER,
                dlss_motion,
            );
        }
        #[cfg(feature = "streamline-rr")]
        let dlss_specular_motion = self.rr.as_ref().map_or(motion, |rr| &rr.specular_motion);
        #[cfg(not(feature = "streamline-rr"))]
        let dlss_specular_motion = motion;
        unsafe {
            create_texture_uav(
                device,
                &self.shader_heap,
                DXR_UAV_BASE + super::DLSS_SPECULAR_MOTION_UAV_REGISTER,
                dlss_specular_motion,
            );
        }
        #[cfg(feature = "nrd")]
        let transmission_uavs = self.nrd.as_ref().map_or(
            [
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
            ],
            |nrd| {
                [
                    &nrd.transmission_raw_diffuse,
                    &nrd.transmission_raw_specular,
                    &nrd.transmission_base_color,
                    &nrd.transmission_normal_roughness,
                    &nrd.transmission_view_z,
                    &nrd.transmission_motion,
                    &nrd.transmission_diffuse_hit_distance,
                    &nrd.transmission_specular_hit_distance,
                    &nrd.transmission_primary_emissive,
                    &nrd.transmission_diffuse_albedo,
                    &nrd.transmission_view_proxy,
                ]
            },
        );
        #[cfg(not(feature = "nrd"))]
        let transmission_uavs = [
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
        for (offset, resource) in transmission_uavs.into_iter().enumerate() {
            unsafe {
                create_texture_uav(
                    device,
                    &self.shader_heap,
                    DXR_UAV_BASE + super::TRANSMISSION_RAW_DIFFUSE_UAV_REGISTER + offset,
                    resource,
                )
            };
        }
        debug_assert_eq!(
            super::TRANSMISSION_VIEW_PROXY_UAV_REGISTER + 1,
            super::DLSS_SPECULAR_MOTION_UAV_REGISTER
        );
        debug_assert_eq!(
            super::DLSS_SPECULAR_MOTION_UAV_REGISTER + 1,
            DXR_UAV_REGISTER_COUNT
        );

        #[cfg(feature = "nrd")]
        if let Some(nrd) = self.nrd.as_ref() {
            let primary = &nrd.primary;
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
                &primary.diffuse_input,
                &primary.specular_input,
                &primary.normal_roughness,
                &primary.motion,
                &primary.view_z,
                &primary.diffuse_factor,
                &primary.specular_factor,
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

            let transmission = &nrd.transmission;
            let transmission_prep_srvs = [
                &nrd.transmission_raw_diffuse,
                &nrd.transmission_raw_specular,
                &nrd.transmission_base_color,
                &nrd.transmission_normal_roughness,
                &nrd.transmission_view_z,
                &nrd.transmission_motion,
                &nrd.transmission_diffuse_hit_distance,
                &nrd.transmission_specular_hit_distance,
                &nrd.transmission_primary_emissive,
                &nrd.transmission_diffuse_albedo,
                &nrd.transmission_view_proxy,
            ];
            let transmission_prep_uavs = [
                &transmission.diffuse_input,
                &transmission.specular_input,
                &transmission.normal_roughness,
                &transmission.motion,
                &transmission.view_z,
                &transmission.diffuse_factor,
                &transmission.specular_factor,
            ];
            unsafe {
                populate_texture_table(
                    device,
                    &self.shader_heap,
                    super::NRD_TRANSMISSION_PREP_TABLE_BASE,
                    &transmission_prep_srvs,
                    &transmission_prep_uavs,
                )
            };

            let compose_srvs = [
                &primary.diffuse_output,
                &primary.specular_output,
                &primary.diffuse_factor,
                &primary.specular_factor,
                reconstruction_primary_emissive,
                &transmission.diffuse_output,
                &transmission.specular_output,
                &transmission.diffuse_factor,
                &transmission.specular_factor,
                &nrd.transmission_primary_emissive,
                &nrd.transmission_base_color,
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

            #[cfg(feature = "streamline")]
            if let Some(dlss) = self.dlss.as_ref() {
                let compose_srvs = [diffuse_pong, specular_pong];
                let compose_uavs = [&dlss.input_hdr, &dlss.exposure];
                unsafe {
                    populate_texture_table(
                        device,
                        &self.shader_heap,
                        DLSS_COMPOSE_TABLE_BASES[current_index],
                        &compose_srvs,
                        &compose_uavs,
                    )
                };
                let dlss_tonemap_srvs = [
                    &dlss.output_hdr,
                    // t1 is unused in composed-HDR mode. It is overwritten by
                    // a typed null SRV below so an accidental shader read is
                    // energy-neutral rather than a second copy of the output.
                    raw_specular,
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
                        DLSS_TONEMAP_TABLE_BASE,
                        &dlss_tonemap_srvs,
                        &[display_output],
                    );
                    create_null_texture_srv(
                        device,
                        &self.shader_heap,
                        DLSS_TONEMAP_TABLE_BASE + 1,
                        DXGI_FORMAT_R16G16B16A16_FLOAT,
                    );
                };
            }
        }
        #[cfg(feature = "streamline-rr")]
        if let Some(rr) = self.rr.as_ref() {
            let input_srvs = [reconstruction_normal_roughness];
            let input_uavs = [&rr.normal_roughness];
            unsafe {
                populate_texture_table(
                    device,
                    &self.shader_heap,
                    super::RR_INPUT_TABLE_BASE,
                    &input_srvs,
                    &input_uavs,
                )
            };
            let rr_tonemap_srvs = [
                &rr.output_hdr,
                raw_specular,
                raw_diffuse,
                raw_specular,
                albedo,
                normal,
                depth,
                motion,
                &self.histories[0].moments,
                rejection,
                &self.histories[0].length,
                id,
                reconstruction_specular_hit_distance,
                // t13 is NRD validation on conventional paths and the
                // explicit reflected-geometry guide on RR. InputMode keeps
                // the two debug interpretations unambiguous.
                &rr.specular_motion,
            ];
            unsafe {
                populate_texture_table(
                    device,
                    &self.shader_heap,
                    super::RR_TONEMAP_TABLE_BASE,
                    &rr_tonemap_srvs,
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

#[cfg(feature = "nrd")]
fn create_nrd_denoiser_resources(
    device: &ID3D12Device,
    extent: Extent2D,
    generation: u64,
    layer: &str,
) -> Result<NrdDenoiserResources> {
    let label = |value: &str| format!("代际 {generation} NRD {layer} {value}");
    Ok(NrdDenoiserResources {
        diffuse_input: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            label("diffuse radiance hit distance"),
        )?,
        specular_input: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            label("specular radiance hit distance"),
        )?,
        normal_roughness: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R10G10B10A2_UNORM,
            label("packed normal roughness"),
        )?,
        motion: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            label("2.5D motion"),
        )?,
        view_z: create_uav_texture(device, extent, DXGI_FORMAT_R32_FLOAT, label("viewZ"))?,
        diffuse_factor: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            label("diffuse material factor"),
        )?,
        specular_factor: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            label("specular material factor"),
        )?,
        diffuse_output: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            label("diffuse output"),
        )?,
        specular_output: create_uav_texture(
            device,
            extent,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            label("specular output"),
        )?,
        backend: super::NrdBackend::new(device, extent)?,
    })
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
