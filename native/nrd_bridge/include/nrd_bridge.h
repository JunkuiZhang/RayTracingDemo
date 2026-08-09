#ifndef RAYTRACINGDEMO_NRD_BRIDGE_H
#define RAYTRACINGDEMO_NRD_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define NRD_BRIDGE_ABI_VERSION UINT32_C(1)
#define NRD_BRIDGE_QUEUED_FRAMES UINT32_C(3)

typedef struct ID3D12Device ID3D12Device;
typedef struct ID3D12GraphicsCommandList ID3D12GraphicsCommandList;
typedef struct ID3D12Resource ID3D12Resource;
typedef struct NrdBridge NrdBridge;

typedef uint32_t NrdBridgeStatus;
#define NRD_BRIDGE_STATUS_OK UINT32_C(0)
#define NRD_BRIDGE_STATUS_INVALID_ARGUMENT UINT32_C(1)
#define NRD_BRIDGE_STATUS_NOT_READY UINT32_C(2)
#define NRD_BRIDGE_STATUS_VERSION_MISMATCH UINT32_C(3)
#define NRD_BRIDGE_STATUS_RUNTIME_ERROR UINT32_C(4)

typedef struct NrdBridgeVersion {
    uint32_t abi_version;
    uint32_t major;
    uint32_t minor;
    uint32_t build;
    uint32_t normal_encoding;
    uint32_t roughness_encoding;
    char commit[41];
} NrdBridgeVersion;

typedef struct NrdBridgeCreateDesc {
    uint32_t abi_version;
    uint32_t resource_width;
    uint32_t resource_height;
    uint32_t queued_frames;
    ID3D12Device* device;
} NrdBridgeCreateDesc;

typedef struct NrdBridgeFrameState {
    float world_to_view[16];
    float world_to_view_prev[16];
    float view_to_clip[16];
    float view_to_clip_prev[16];
    float camera_position[3];
    uint32_t frame_index;
    uint32_t render_width;
    uint32_t render_height;
    uint32_t previous_render_width;
    uint32_t previous_render_height;
    float camera_jitter_px[2];
    float camera_jitter_prev_px[2];
    uint32_t reset;
    float delta_time_ms;
} NrdBridgeFrameState;

typedef struct NrdBridgeFrameDesc {
    const NrdBridgeFrameState* state;
    uint32_t enable_validation;
} NrdBridgeFrameDesc;

typedef struct NrdBridgeResource {
    ID3D12Resource* resource;
    // D3D12_RESOURCE_STATES bitmask supplied by the renderer.
    uint32_t state;
} NrdBridgeResource;

typedef struct NrdBridgeResources {
    NrdBridgeResource motion;
    NrdBridgeResource normal_roughness;
    NrdBridgeResource view_z;
    NrdBridgeResource diffuse_radiance_hit_distance;
    NrdBridgeResource specular_radiance_hit_distance;
    NrdBridgeResource diffuse_output;
    NrdBridgeResource specular_output;
    NrdBridgeResource validation_output;
} NrdBridgeResources;

NrdBridgeStatus nrd_bridge_query_version(NrdBridgeVersion* out_version);
NrdBridgeStatus nrd_bridge_create(
    const NrdBridgeCreateDesc* desc,
    NrdBridge** out_bridge);
NrdBridgeStatus nrd_bridge_denoise(
    NrdBridge* bridge,
    const NrdBridgeFrameDesc* frame,
    const NrdBridgeResources* resources,
    ID3D12GraphicsCommandList* command_list);
void nrd_bridge_destroy(NrdBridge* bridge);
size_t nrd_bridge_copy_last_error(
    const NrdBridge* bridge,
    char* destination,
    size_t capacity);

#ifdef __cplusplus
}
#endif

#endif
