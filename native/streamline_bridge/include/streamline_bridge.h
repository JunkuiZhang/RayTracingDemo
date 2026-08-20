#ifndef RAYTRACINGDEMO_STREAMLINE_BRIDGE_H
#define RAYTRACINGDEMO_STREAMLINE_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define STREAMLINE_BRIDGE_ABI_VERSION UINT32_C(5)

typedef struct StreamlineBridge StreamlineBridge;

typedef uint32_t StreamlineBridgeStatus;
#define STREAMLINE_BRIDGE_STATUS_OK UINT32_C(0)
#define STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT UINT32_C(1)
#define STREAMLINE_BRIDGE_STATUS_SDK_ERROR UINT32_C(2)
#define STREAMLINE_BRIDGE_STATUS_EXCEPTION UINT32_C(3)
#define STREAMLINE_BRIDGE_STATUS_NOT_INITIALIZED UINT32_C(4)
#define STREAMLINE_BRIDGE_STATUS_UNSUPPORTED UINT32_C(5)
#define STREAMLINE_BRIDGE_STATUS_ALREADY_UPGRADED UINT32_C(6)

typedef enum StreamlineBridgeDlssMode {
    STREAMLINE_BRIDGE_DLSS_NATIVE = 0,
    STREAMLINE_BRIDGE_DLSS_DLAA = 1,
    STREAMLINE_BRIDGE_DLSS_QUALITY = 2,
    STREAMLINE_BRIDGE_DLSS_BALANCED = 3,
    STREAMLINE_BRIDGE_DLSS_PERFORMANCE = 4
} StreamlineBridgeDlssMode;

typedef enum StreamlineBridgeReflexMode {
    STREAMLINE_BRIDGE_REFLEX_OFF = 0,
    STREAMLINE_BRIDGE_REFLEX_ON = 1,
    STREAMLINE_BRIDGE_REFLEX_ON_BOOST = 2
} StreamlineBridgeReflexMode;

typedef struct StreamlineBridgeInitDesc {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t development;
    uint32_t enable_dlss;
    uint32_t application_id;
    uint32_t enable_dlss_rr;
    uint32_t enable_dlss_fg;
    const wchar_t* plugin_path;
    const wchar_t* log_path;
    const char* project_id;
    const char* engine_version;
} StreamlineBridgeInitDesc;

typedef struct StreamlineBridgeSupport {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t dlss_supported;
    uint32_t reflex_supported;
    uint32_t pcl_supported;
    uint32_t rr_supported;
    uint32_t fg_supported;
    uint32_t dlss_result;
    uint32_t reflex_result;
    uint32_t pcl_result;
    uint32_t rr_result;
    uint32_t fg_result;
    uint64_t adapter_luid;
    char sdk_version[32];
} StreamlineBridgeSupport;

typedef struct StreamlineBridgeOptimalSettings {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t optimal_render_width;
    uint32_t optimal_render_height;
    uint32_t render_width_min;
    uint32_t render_height_min;
    uint32_t render_width_max;
    uint32_t render_height_max;
    float optimal_sharpness;
} StreamlineBridgeOptimalSettings;

typedef struct StreamlineBridgeFrameToken {
    uint32_t struct_size;
    uint32_t abi_version;
    void* token;
    uint32_t frame_index;
    uint32_t reserved;
} StreamlineBridgeFrameToken;

typedef struct StreamlineBridgeViewport {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t id;
    uint32_t reserved;
} StreamlineBridgeViewport;

typedef struct StreamlineBridgeDlssOptions {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t mode;
    uint32_t output_width;
    uint32_t output_height;
    float sharpness;
    float pre_exposure;
    float exposure_scale;
    uint32_t color_buffers_hdr;
    uint32_t use_auto_exposure;
    uint32_t alpha_upscaling_enabled;
} StreamlineBridgeDlssOptions;

typedef struct StreamlineBridgeRrOptions {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t mode;
    uint32_t output_width;
    uint32_t output_height;
    float sharpness;
    float pre_exposure;
    float exposure_scale;
    uint32_t color_buffers_hdr;
    uint32_t indicator_invert_axis_x;
    uint32_t indicator_invert_axis_y;
    uint32_t normal_roughness_mode;
    float world_to_camera_view[16];
    float camera_view_to_world[16];
    uint32_t alpha_upscaling_enabled;
    uint32_t dlaa_preset;
    uint32_t quality_preset;
    uint32_t balanced_preset;
    uint32_t performance_preset;
    uint32_t ultra_performance_preset;
    uint32_t ultra_quality_preset;
} StreamlineBridgeRrOptions;

typedef struct StreamlineBridgeRrOptimalSettings {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t optimal_render_width;
    uint32_t optimal_render_height;
    uint32_t render_width_min;
    uint32_t render_height_min;
    uint32_t render_width_max;
    uint32_t render_height_max;
    float optimal_sharpness;
} StreamlineBridgeRrOptimalSettings;

typedef struct StreamlineBridgeRrState {
    uint32_t struct_size;
    uint32_t abi_version;
    uint64_t estimated_vram_usage_bytes;
} StreamlineBridgeRrState;

typedef enum StreamlineBridgeFrameGenerationMode {
    STREAMLINE_BRIDGE_FRAME_GENERATION_OFF = 0,
    STREAMLINE_BRIDGE_FRAME_GENERATION_ON = 1
} StreamlineBridgeFrameGenerationMode;

typedef struct StreamlineBridgeFrameGenerationOptions {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t mode;
    uint32_t num_frames_to_generate;
    uint32_t flags;
    uint32_t num_back_buffers;
    uint32_t mvec_depth_width;
    uint32_t mvec_depth_height;
    uint32_t color_width;
    uint32_t color_height;
    uint32_t color_buffer_format;
    uint32_t mvec_buffer_format;
    uint32_t depth_buffer_format;
} StreamlineBridgeFrameGenerationOptions;

typedef struct StreamlineBridgeFrameGenerationState {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t status_raw;
    uint32_t min_width_or_height;
    uint32_t num_frames_actually_presented;
    uint32_t num_frames_to_generate_max;
    uint64_t estimated_vram_usage_bytes;
    uint32_t vsync_support_available;
    uint32_t dynamic_mfg_supported;
} StreamlineBridgeFrameGenerationState;

typedef struct StreamlineBridgeConstants {
    uint32_t struct_size;
    uint32_t abi_version;
    float camera_view_to_clip[16];
    float clip_to_camera_view[16];
    float clip_to_prev_clip[16];
    float prev_clip_to_clip[16];
    float jitter_offset[2];
    float mvec_scale[2];
    float camera_position[3];
    float camera_up[3];
    float camera_right[3];
    float camera_forward[3];
    float camera_near;
    float camera_far;
    float camera_fov;
    float camera_aspect_ratio;
    uint32_t depth_inverted;
    uint32_t camera_motion_included;
    uint32_t motion_vectors_3d;
    uint32_t reset;
    uint32_t motion_vectors_jittered;
} StreamlineBridgeConstants;

typedef struct StreamlineBridgeResourceTag {
    uint32_t struct_size;
    uint32_t abi_version;
    void* resource;
    uint32_t state;
    uint32_t buffer_type;
    uint32_t lifecycle;
    uint32_t top;
    uint32_t left;
    uint32_t width;
    uint32_t height;
} StreamlineBridgeResourceTag;

typedef struct StreamlineBridgeReflexState {
    uint32_t struct_size;
    uint32_t abi_version;
    uint32_t low_latency_available;
    uint32_t latency_report_available;
    uint32_t flash_indicator_driver_controlled;
} StreamlineBridgeReflexState;

StreamlineBridgeStatus streamline_bridge_create(
    const StreamlineBridgeInitDesc* desc,
    StreamlineBridge** out_bridge);
StreamlineBridgeStatus streamline_bridge_set_d3d_device(
    StreamlineBridge* bridge,
    void* d3d_device,
    const uint8_t* adapter_luid,
    size_t adapter_luid_size);
StreamlineBridgeStatus streamline_bridge_query_support(
    StreamlineBridge* bridge,
    StreamlineBridgeSupport* out_support);
StreamlineBridgeStatus streamline_bridge_get_optimal_settings(
    StreamlineBridge* bridge,
    const StreamlineBridgeDlssOptions* options,
    StreamlineBridgeOptimalSettings* out_settings);
StreamlineBridgeStatus streamline_bridge_dlss_set_options(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeDlssOptions* options);
StreamlineBridgeStatus streamline_bridge_rr_get_optimal_settings(
    StreamlineBridge* bridge,
    const StreamlineBridgeRrOptions* options,
    StreamlineBridgeRrOptimalSettings* out_settings);
StreamlineBridgeStatus streamline_bridge_rr_set_options(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeRrOptions* options);
StreamlineBridgeStatus streamline_bridge_rr_get_state(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    StreamlineBridgeRrState* out_state);
StreamlineBridgeStatus streamline_bridge_allocate_resources(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    void* command_list);
StreamlineBridgeStatus streamline_bridge_free_resources(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport);
StreamlineBridgeStatus streamline_bridge_rr_free_resources(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport);
StreamlineBridgeStatus streamline_bridge_fg_set_options(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeFrameGenerationOptions* options);
StreamlineBridgeStatus streamline_bridge_fg_get_state(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeFrameGenerationOptions* estimate_options,
    StreamlineBridgeFrameGenerationState* out_state);
StreamlineBridgeStatus streamline_bridge_get_frame_token(
    StreamlineBridge* bridge,
    uint32_t frame_index,
    StreamlineBridgeFrameToken* out_token);
StreamlineBridgeStatus streamline_bridge_set_constants(
    StreamlineBridge* bridge,
    const StreamlineBridgeFrameToken* token,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeConstants* constants);
StreamlineBridgeStatus streamline_bridge_set_tags(
    StreamlineBridge* bridge,
    const StreamlineBridgeFrameToken* token,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeResourceTag* tags,
    uint32_t tag_count,
    void* command_list);
StreamlineBridgeStatus streamline_bridge_evaluate_dlss(
    StreamlineBridge* bridge,
    const StreamlineBridgeFrameToken* token,
    const StreamlineBridgeViewport* viewport,
    void* command_list);
StreamlineBridgeStatus streamline_bridge_evaluate_rr(
    StreamlineBridge* bridge,
    const StreamlineBridgeFrameToken* token,
    const StreamlineBridgeViewport* viewport,
    void* command_list);
StreamlineBridgeStatus streamline_bridge_reflex_set_mode(
    StreamlineBridge* bridge,
    uint32_t mode);
StreamlineBridgeStatus streamline_bridge_reflex_sleep(
    StreamlineBridge* bridge,
    const StreamlineBridgeFrameToken* token);
StreamlineBridgeStatus streamline_bridge_pcl_marker(
    StreamlineBridge* bridge,
    const StreamlineBridgeFrameToken* token,
    uint32_t marker);
StreamlineBridgeStatus streamline_bridge_reflex_get_state(
    StreamlineBridge* bridge,
    StreamlineBridgeReflexState* out_state);
StreamlineBridgeStatus streamline_bridge_upgrade_interface(
    StreamlineBridge* bridge,
    void** interface_ptr);
StreamlineBridgeStatus streamline_bridge_get_native_interface(
    StreamlineBridge* bridge,
    void* proxy_interface,
    void** out_native_interface);
size_t streamline_bridge_copy_last_error(
    const StreamlineBridge* bridge,
    char* destination,
    size_t capacity);
StreamlineBridgeStatus streamline_bridge_shutdown(StreamlineBridge* bridge);

#ifdef __cplusplus
}
#endif

#endif
