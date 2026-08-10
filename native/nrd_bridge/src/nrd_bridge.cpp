#include "nrd_bridge.h"

#include <algorithm>
#include <cstdlib>
#include <cstring>
#include <malloc.h>
#include <memory>
#include <new>
#include <string>

#include <d3d12.h>

#include <NRI.h>
#include <Extensions/NRIHelper.h>
#include <Extensions/NRIWrapperD3D12.h>

#include <NRD.h>
#include <NRDIntegration.h>
#include <NRDIntegration.hpp>

static_assert(sizeof(NrdBridgeVersion) == 68, "NRD bridge version ABI changed");
static_assert(sizeof(NrdBridgeCreateDesc) == 24, "NRD bridge create ABI changed");
static_assert(sizeof(NrdBridgeFrameState) == 312, "NRD bridge frame ABI changed");
static_assert(sizeof(NrdBridgeFrameDesc) == 16, "NRD bridge frame description ABI changed");
static_assert(sizeof(NrdBridgeResource) == 16, "NRD bridge resource ABI changed");

struct NrdBridge {
    nrd::Integration integration;
    nrd::Identifier denoiser_identifier = 1;
    std::string last_error;
};

namespace {

constexpr uint32_t kDenoiserIdentifier = 1;
constexpr uint32_t kD3D12ResourceStateUnorderedAccess = 0x00000008u;
constexpr uint32_t kD3D12ResourceStateNonPixelShaderResource = 0x00000040u;
constexpr uint32_t kD3D12ResourceStatePixelShaderResource = 0x00000080u;

uint16_t extent16(uint32_t value) {
    return static_cast<uint16_t>(std::min<uint32_t>(value, UINT16_MAX));
}

void* NRD_CALL nrd_allocate(void*, size_t size, size_t alignment) {
    (void)alignment;
#if defined(_MSC_VER)
    return _aligned_malloc(size, std::max<size_t>(alignment, alignof(void*)));
#else
    return std::aligned_alloc(std::max<size_t>(alignment, alignof(void*)), size);
#endif
}

void* NRD_CALL nrd_reallocate(void*, void* memory, size_t size, size_t alignment) {
#if defined(_MSC_VER)
    return _aligned_realloc(memory, size, std::max<size_t>(alignment, alignof(void*)));
#else
    (void)memory;
    (void)alignment;
    return nullptr;
#endif
}

void NRD_CALL nrd_free(void*, void* memory) {
#if defined(_MSC_VER)
    _aligned_free(memory);
#else
    std::free(memory);
#endif
}

nrd::AllocationCallbacks allocation_callbacks() {
    return {nrd_allocate, nrd_reallocate, nrd_free, nullptr};
}

void set_error(NrdBridge* bridge, const char* message) {
    if (bridge != nullptr)
        bridge->last_error = message != nullptr ? message : "unknown NRD bridge error";
}

NrdBridgeStatus map_result(nrd::Result result) {
    switch (result) {
        case nrd::Result::SUCCESS:
            return NRD_BRIDGE_STATUS_OK;
        case nrd::Result::INVALID_ARGUMENT:
            return NRD_BRIDGE_STATUS_INVALID_ARGUMENT;
        case nrd::Result::UNSUPPORTED:
        case nrd::Result::NON_UNIQUE_IDENTIFIER:
            return NRD_BRIDGE_STATUS_VERSION_MISMATCH;
        default:
            return NRD_BRIDGE_STATUS_RUNTIME_ERROR;
    }
}

nri::AccessLayoutStage state_from_d3d12(uint32_t state) {
    if (state == kD3D12ResourceStateUnorderedAccess) {
        return {
            nri::AccessBits::SHADER_RESOURCE_STORAGE,
            nri::Layout::SHADER_RESOURCE_STORAGE,
            nri::StageBits::COMPUTE_SHADER,
        };
    }

    if ((state & (kD3D12ResourceStateNonPixelShaderResource | kD3D12ResourceStatePixelShaderResource)) != 0) {
        return {
            nri::AccessBits::SHADER_RESOURCE,
            nri::Layout::SHADER_RESOURCE,
            nri::StageBits::COMPUTE_SHADER,
        };
    }

    // A COMMON/unknown state is intentionally represented as an unknown NRI
    // state. The integration will not emit a speculative transition for it;
    // the renderer must provide the real state before a future 9D dispatch.
    (void)state;
    return {nri::AccessBits::NONE, nri::Layout::UNDEFINED, nri::StageBits::ALL};
}

nrd::Resource make_resource(const NrdBridgeResource& input) {
    nrd::Resource resource = {};
    resource.d3d12.resource = input.resource;
    resource.d3d12.format = static_cast<DXGI_FORMAT>(input.format);
    resource.state = state_from_d3d12(input.state);
    return resource;
}

uint32_t state_to_d3d12(const nri::AccessLayoutStage& state) {
    if (state.access == nri::AccessBits::SHADER_RESOURCE_STORAGE)
        return kD3D12ResourceStateUnorderedAccess;
    if (state.access == nri::AccessBits::SHADER_RESOURCE)
        return kD3D12ResourceStateNonPixelShaderResource | kD3D12ResourceStatePixelShaderResource;
    return 0;
}

void copy_matrix(float* destination, const float* source) {
    std::memcpy(destination, source, sizeof(float) * 16);
}

} // namespace

NrdBridgeStatus nrd_bridge_query_version(NrdBridgeVersion* out_version) {
    if (out_version == nullptr)
        return NRD_BRIDGE_STATUS_INVALID_ARGUMENT;

    const nrd::LibraryDesc* library = nrd::GetLibraryDesc();
    if (library == nullptr)
        return NRD_BRIDGE_STATUS_RUNTIME_ERROR;

    *out_version = {};
    out_version->abi_version = NRD_BRIDGE_ABI_VERSION;
    out_version->major = library->versionMajor;
    out_version->minor = library->versionMinor;
    out_version->build = library->versionBuild;
    out_version->normal_encoding = static_cast<uint32_t>(library->normalEncoding);
    out_version->roughness_encoding = static_cast<uint32_t>(library->roughnessEncoding);
    std::strncpy(out_version->commit, NRD_BRIDGE_NRD_COMMIT, sizeof(out_version->commit) - 1);

    if (library->versionMajor != 4 || library->versionMinor != 17 || library->versionBuild != 3 ||
        library->normalEncoding != nrd::NormalEncoding::R10_G10_B10_A2_UNORM ||
        library->roughnessEncoding != nrd::RoughnessEncoding::LINEAR) {
        return NRD_BRIDGE_STATUS_VERSION_MISMATCH;
    }
    return NRD_BRIDGE_STATUS_OK;
}

NrdBridgeStatus nrd_bridge_create(const NrdBridgeCreateDesc* desc, NrdBridge** out_bridge) {
    if (out_bridge == nullptr)
        return NRD_BRIDGE_STATUS_INVALID_ARGUMENT;
    *out_bridge = nullptr;

    if (desc == nullptr || desc->abi_version != NRD_BRIDGE_ABI_VERSION || desc->device == nullptr ||
        desc->resource_width == 0 || desc->resource_height == 0 ||
        desc->queued_frames != NRD_BRIDGE_QUEUED_FRAMES) {
        return NRD_BRIDGE_STATUS_INVALID_ARGUMENT;
    }

    try {
        NrdBridgeVersion version = {};
        const NrdBridgeStatus version_status = nrd_bridge_query_version(&version);
        if (version_status != NRD_BRIDGE_STATUS_OK)
            return version_status;

        std::unique_ptr<NrdBridge> bridge(new (std::nothrow) NrdBridge());
        if (!bridge)
            return NRD_BRIDGE_STATUS_RUNTIME_ERROR;

        nrd::DenoiserDesc denoiser = {kDenoiserIdentifier, nrd::Denoiser::REBLUR_DIFFUSE_SPECULAR};
        nrd::InstanceCreationDesc instance_desc = {};
        instance_desc.allocationCallbacks = allocation_callbacks();
        instance_desc.denoisers = &denoiser;
        instance_desc.denoisersNum = 1;

        nrd::IntegrationCreationDesc integration_desc = {};
        std::strncpy(integration_desc.name, "RayTracingDemo-REBLUR", sizeof(integration_desc.name) - 1);
        integration_desc.resourceWidth = extent16(desc->resource_width);
        integration_desc.resourceHeight = extent16(desc->resource_height);
        integration_desc.queuedFrameNum = static_cast<uint8_t>(desc->queued_frames);
        integration_desc.enableWholeLifetimeDescriptorCaching = true;
        integration_desc.autoWaitForIdle = false;

        nri::DeviceCreationD3D12Desc device_desc = {};
        device_desc.d3d12Device = desc->device;
        const nrd::Result result = bridge->integration.RecreateD3D12(integration_desc, instance_desc, device_desc);
        if (result != nrd::Result::SUCCESS) {
            set_error(bridge.get(), "NRD Integration::RecreateD3D12 failed");
            return map_result(result);
        }

        *out_bridge = bridge.release();
        return NRD_BRIDGE_STATUS_OK;
    } catch (...) {
        return NRD_BRIDGE_STATUS_RUNTIME_ERROR;
    }
}

NrdBridgeStatus nrd_bridge_denoise(
    NrdBridge* bridge,
    const NrdBridgeFrameDesc* frame,
    NrdBridgeResources* resources,
    ID3D12GraphicsCommandList* command_list) {
    if (bridge == nullptr || frame == nullptr || frame->state == nullptr || resources == nullptr || command_list == nullptr)
        return NRD_BRIDGE_STATUS_INVALID_ARGUMENT;

    try {
        const NrdBridgeFrameState& state = *frame->state;
        nrd::CommonSettings common = {};
        copy_matrix(common.worldToViewMatrix, state.world_to_view);
        copy_matrix(common.worldToViewMatrixPrev, state.world_to_view_prev);
        copy_matrix(common.viewToClipMatrix, state.view_to_clip);
        copy_matrix(common.viewToClipMatrixPrev, state.view_to_clip_prev);
        common.resourceSize[0] = extent16(state.render_width);
        common.resourceSize[1] = extent16(state.render_height);
        common.resourceSizePrev[0] = extent16(state.previous_render_width);
        common.resourceSizePrev[1] = extent16(state.previous_render_height);
        common.rectSize[0] = common.resourceSize[0];
        common.rectSize[1] = common.resourceSize[1];
        common.rectSizePrev[0] = common.resourceSizePrev[0];
        common.rectSizePrev[1] = common.resourceSizePrev[1];
        common.cameraJitter[0] = state.camera_jitter_px[0] / std::max(1u, state.render_width);
        common.cameraJitter[1] = state.camera_jitter_px[1] / std::max(1u, state.render_height);
        common.cameraJitterPrev[0] = state.camera_jitter_prev_px[0] / std::max(1u, state.previous_render_width);
        common.cameraJitterPrev[1] = state.camera_jitter_prev_px[1] / std::max(1u, state.previous_render_height);
        common.viewZScale = 1.0f;
        common.timeDeltaBetweenFrames = std::max(0.0f, state.delta_time_ms);
        common.denoisingRange = 1000.0f;
        common.frameIndex = state.frame_index;
        common.accumulationMode = state.reset != 0 ? nrd::AccumulationMode::RESTART : nrd::AccumulationMode::CONTINUE;
        common.isMotionVectorInWorldSpace = false;
        common.motionVectorScale[0] = 1.0f / std::max(1u, state.render_width);
        common.motionVectorScale[1] = 1.0f / std::max(1u, state.render_height);
        common.motionVectorScale[2] = 1.0f;
        common.enableValidation = frame->enable_validation != 0;

        // NRD owns frame-indexed descriptor pools and requires NewFrame once
        // before any per-frame settings are submitted.
        bridge->integration.NewFrame();
        const nrd::Result common_result = bridge->integration.SetCommonSettings(common);
        if (common_result != nrd::Result::SUCCESS) {
            set_error(bridge, "NRD SetCommonSettings failed");
            return map_result(common_result);
        }

        nrd::ReblurSettings reblur_settings = {};
        // The renderer feeds one white-noise path sample per pixel. NRD's own
        // integration guidance recommends a 60-frame history for this case;
        // keep fast/stabilized histories proportional and maximize isolated
        // firefly suppression without changing the locked hit-distance model.
        reblur_settings.maxAccumulatedFrameNum = 60;
        reblur_settings.maxFastAccumulatedFrameNum = 10;
        reblur_settings.maxStabilizedFrameNum = 60;
        reblur_settings.fireflySuppressorMinRelativeScale = 1.0f;
        // The NRD path tracer uses a Bayer-stratified probabilistic
        // diffuse/specular split at the primary hit. Skipped lobes export zero
        // hitT, so REBLUR must reconstruct a valid in-lobe distance before its
        // non-zero default pre-pass performs specular motion tracking.
        reblur_settings.hitDistanceReconstructionMode = nrd::HitDistanceReconstructionMode::AREA_3X3;
        const nrd::Result settings_result = bridge->integration.SetDenoiserSettings(bridge->denoiser_identifier, &reblur_settings);
        if (settings_result != nrd::Result::SUCCESS) {
            set_error(bridge, "NRD SetDenoiserSettings failed");
            return map_result(settings_result);
        }

        nrd::ResourceSnapshot snapshot;
        // Keep NRD's final states so the caller can update its tracked state
        // and issue only the next consumer's explicit transition. Restoring
        // every resource after every dispatch would add hidden barriers.
        snapshot.restoreInitialState = false;
        const nrd::Resource motion = make_resource(resources->motion);
        const nrd::Resource normal_roughness = make_resource(resources->normal_roughness);
        const nrd::Resource view_z = make_resource(resources->view_z);
        const nrd::Resource diffuse_input = make_resource(resources->diffuse_radiance_hit_distance);
        const nrd::Resource specular_input = make_resource(resources->specular_radiance_hit_distance);
        const nrd::Resource diffuse_output = make_resource(resources->diffuse_output);
        const nrd::Resource specular_output = make_resource(resources->specular_output);
        snapshot.SetResource(nrd::ResourceType::IN_MV, motion);
        snapshot.SetResource(nrd::ResourceType::IN_NORMAL_ROUGHNESS, normal_roughness);
        snapshot.SetResource(nrd::ResourceType::IN_VIEWZ, view_z);
        snapshot.SetResource(nrd::ResourceType::IN_DIFF_RADIANCE_HITDIST, diffuse_input);
        snapshot.SetResource(nrd::ResourceType::IN_SPEC_RADIANCE_HITDIST, specular_input);
        snapshot.SetResource(nrd::ResourceType::OUT_DIFF_RADIANCE_HITDIST, diffuse_output);
        snapshot.SetResource(nrd::ResourceType::OUT_SPEC_RADIANCE_HITDIST, specular_output);
        if (resources->validation_output.resource != nullptr)
            snapshot.SetResource(nrd::ResourceType::OUT_VALIDATION, make_resource(resources->validation_output));

        if (frame->enable_validation != 0 && resources->validation_output.resource == nullptr) {
            set_error(bridge, "NRD validation was requested without OUT_VALIDATION");
            return NRD_BRIDGE_STATUS_INVALID_ARGUMENT;
        }

        const nrd::Identifier identifier = bridge->denoiser_identifier;
        nri::CommandBufferD3D12Desc command_desc = {};
        command_desc.d3d12CommandList = command_list;
        bridge->integration.DenoiseD3D12(&identifier, 1, command_desc, snapshot);

        resources->motion.state = state_to_d3d12(snapshot.slots[static_cast<size_t>(nrd::ResourceType::IN_MV)]->state);
        resources->normal_roughness.state = state_to_d3d12(snapshot.slots[static_cast<size_t>(nrd::ResourceType::IN_NORMAL_ROUGHNESS)]->state);
        resources->view_z.state = state_to_d3d12(snapshot.slots[static_cast<size_t>(nrd::ResourceType::IN_VIEWZ)]->state);
        resources->diffuse_radiance_hit_distance.state = state_to_d3d12(snapshot.slots[static_cast<size_t>(nrd::ResourceType::IN_DIFF_RADIANCE_HITDIST)]->state);
        resources->specular_radiance_hit_distance.state = state_to_d3d12(snapshot.slots[static_cast<size_t>(nrd::ResourceType::IN_SPEC_RADIANCE_HITDIST)]->state);
        resources->diffuse_output.state = state_to_d3d12(snapshot.slots[static_cast<size_t>(nrd::ResourceType::OUT_DIFF_RADIANCE_HITDIST)]->state);
        resources->specular_output.state = state_to_d3d12(snapshot.slots[static_cast<size_t>(nrd::ResourceType::OUT_SPEC_RADIANCE_HITDIST)]->state);
        if (resources->validation_output.resource != nullptr)
            resources->validation_output.state = state_to_d3d12(snapshot.slots[static_cast<size_t>(nrd::ResourceType::OUT_VALIDATION)]->state);
        return NRD_BRIDGE_STATUS_OK;
    } catch (...) {
        set_error(bridge, "exception caught inside NRD bridge");
        return NRD_BRIDGE_STATUS_RUNTIME_ERROR;
    }
}

void nrd_bridge_destroy(NrdBridge* bridge) {
    if (bridge == nullptr)
        return;
    try {
        delete bridge;
    } catch (...) {
        // Destruction is noexcept at the C ABI boundary. The integration is
        // configured with autoWaitForIdle=false; callers retire it by fence.
    }
}

size_t nrd_bridge_copy_last_error(const NrdBridge* bridge, char* destination, size_t capacity) {
    if (bridge == nullptr)
        return 0;
    const size_t required = bridge->last_error.size() + 1;
    if (destination != nullptr && capacity != 0) {
        const size_t copied = std::min(capacity - 1, bridge->last_error.size());
        std::memcpy(destination, bridge->last_error.data(), copied);
        destination[copied] = '\0';
    }
    return required;
}
