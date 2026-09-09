#include "streamline_bridge.h"

#include <algorithm>
#include <array>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <io.h>
#include <memory>
#include <new>
#include <optional>
#include <string>
#include <vector>

#include <unknwn.h>

#include <sl.h>
#include <sl_dlss.h>
#if STREAMLINE_ENABLE_RR
#include <sl_dlss_d.h>
#endif
#if STREAMLINE_ENABLE_FG
#include <sl_dlss_g.h>
#endif
#include <sl_pcl.h>
#include <sl_reflex.h>
#include <sl_version.h>

// Keep the compiled bridge and the Rust-side lock on the same Streamline ABI.
// A stale include path must fail at compile time instead of loading mixed SDK
// binaries and reporting an apparently valid runtime version.
static_assert(SL_VERSION_MAJOR == 2 && SL_VERSION_MINOR == 14 && SL_VERSION_PATCH == 1,
              "Streamline bridge must be built against SDK 2.14.1");
#if STREAMLINE_ENABLE_RR
static_assert(static_cast<uint32_t>(sl::DLSSDPreset::ePresetF) == 6,
              "DLSS RR Preset F numeric ABI changed");
#endif

static_assert(sizeof(StreamlineBridgeInitDesc) == 64, "Streamline init ABI changed");
static_assert(offsetof(StreamlineBridgeInitDesc, enable_dlss_fg) == 24, "Streamline init FG offset changed");
static_assert(offsetof(StreamlineBridgeInitDesc, plugin_path) == 32, "Streamline init pointer offset changed");
static_assert(sizeof(StreamlineBridgeSupport) == 88, "Streamline support ABI changed");
static_assert(offsetof(StreamlineBridgeSupport, fg_supported) == 24, "Streamline support FG offset changed");
static_assert(offsetof(StreamlineBridgeSupport, fg_result) == 44, "Streamline support FG result offset changed");
static_assert(offsetof(StreamlineBridgeSupport, adapter_luid) == 48, "Streamline support LUID offset changed");
static_assert(sizeof(StreamlineBridgeOptimalSettings) == 36, "Streamline optimal ABI changed");
static_assert(sizeof(StreamlineBridgeFrameToken) == 24, "Streamline token ABI changed");
static_assert(sizeof(StreamlineBridgeViewport) == 16, "Streamline viewport ABI changed");
static_assert(sizeof(StreamlineBridgeDlssOptions) == 44, "Streamline options ABI changed");
static_assert(sizeof(StreamlineBridgeRrOptions) == 204, "Streamline RR options ABI changed");
static_assert(sizeof(StreamlineBridgeRrOptimalSettings) == 36, "Streamline RR optimal ABI changed");
static_assert(sizeof(StreamlineBridgeRrState) == 16, "Streamline RR state ABI changed");
static_assert(sizeof(StreamlineBridgeFrameGenerationOptions) == 56, "Streamline FG options ABI changed");
static_assert(offsetof(StreamlineBridgeFrameGenerationOptions, flags) == 16, "Streamline FG flags offset changed");
static_assert(offsetof(StreamlineBridgeFrameGenerationOptions, color_width) == 32, "Streamline FG color offset changed");
static_assert(sizeof(StreamlineBridgeFrameGenerationState) == 40, "Streamline FG state ABI changed");
static_assert(offsetof(StreamlineBridgeFrameGenerationState, estimated_vram_usage_bytes) == 24, "Streamline FG state VRAM offset changed");
static_assert(sizeof(StreamlineBridgeConstants) == 376, "Streamline constants ABI changed");
static_assert(sizeof(StreamlineBridgeResourceTag) == 48, "Streamline resource tag ABI changed");
static_assert(sizeof(StreamlineBridgeReflexState) == 20, "Streamline Reflex ABI changed");

class ScopedStdoutToStderr {
public:
    ScopedStdoutToStderr() noexcept {
        std::fflush(stdout);
        saved_fd_ = _dup(_fileno(stdout));
        if (saved_fd_ >= 0) {
            _dup2(_fileno(stderr), _fileno(stdout));
            SetStdHandle(
                STD_OUTPUT_HANDLE,
                reinterpret_cast<HANDLE>(_get_osfhandle(_fileno(stdout))));
        }
    }

    ~ScopedStdoutToStderr() noexcept {
        std::fflush(stdout);
        if (saved_fd_ >= 0) {
            _dup2(saved_fd_, _fileno(stdout));
            _close(saved_fd_);
            SetStdHandle(
                STD_OUTPUT_HANDLE,
                reinterpret_cast<HANDLE>(_get_osfhandle(_fileno(stdout))));
        }
    }

private:
    int saved_fd_ = -1;
};

struct StreamlineBridge {
    bool initialized = false;
    bool device_set = false;
    bool rr_requested = false;
    bool fg_requested = false;
    uint64_t adapter_luid = 0;
    struct {
        uint32_t dlss = 0;
        uint32_t reflex = 0;
        uint32_t pcl = 0;
        uint32_t rr = 0;
        uint32_t fg = 0;
    } support_results;
    std::string last_error;
    std::unique_ptr<ScopedStdoutToStderr> stdout_redirect;
};

namespace {

constexpr uint32_t kExpectedVersion = STREAMLINE_BRIDGE_ABI_VERSION;

StreamlineBridgeStatus set_error(StreamlineBridge* bridge, const char* message, sl::Result result) noexcept {
    if (bridge != nullptr) {
        bridge->last_error = message != nullptr ? message : "Streamline SDK error";
        bridge->last_error += " (result=";
        bridge->last_error += std::to_string(static_cast<uint32_t>(result));
        bridge->last_error += ")";
    }
    return STREAMLINE_BRIDGE_STATUS_SDK_ERROR;
}

StreamlineBridgeStatus set_error(StreamlineBridge* bridge, const char* message) noexcept {
    if (bridge != nullptr)
        bridge->last_error = message != nullptr ? message : "Streamline bridge error";
    return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
}

bool valid_header(uint32_t size, uint32_t version, size_t minimum) noexcept {
    return version == kExpectedVersion && size >= minimum;
}

bool valid_buffer_type(uint32_t type) noexcept {
    // Keep the bridge ABI narrow: these are the resource tags used by the
    // existing SR/RR paths plus the two DLSS-G UI release tags.
    switch (type) {
        case 0: case 1: case 2: case 3: case 4: case 7: case 8: case 10:
        case 13: case 14: case 23: case 53: case 69:
            return true;
        default:
            return false;
    }
}

bool valid_lifecycle(uint32_t lifecycle) noexcept {
    return lifecycle <= 2;
}

StreamlineBridgeStatus check_bridge(StreamlineBridge* bridge) noexcept {
    return bridge != nullptr && bridge->initialized ? STREAMLINE_BRIDGE_STATUS_OK
                                                      : STREAMLINE_BRIDGE_STATUS_NOT_INITIALIZED;
}

sl::DLSSMode dlss_mode(uint32_t mode) noexcept {
    switch (mode) {
        case STREAMLINE_BRIDGE_DLSS_DLAA:
            return sl::DLSSMode::eDLAA;
        case STREAMLINE_BRIDGE_DLSS_QUALITY:
            return sl::DLSSMode::eMaxQuality;
        case STREAMLINE_BRIDGE_DLSS_BALANCED:
            return sl::DLSSMode::eBalanced;
        case STREAMLINE_BRIDGE_DLSS_PERFORMANCE:
            return sl::DLSSMode::eMaxPerformance;
        default:
            return sl::DLSSMode::eOff;
    }
}

bool valid_dlss_mode(uint32_t mode) noexcept {
    return mode >= STREAMLINE_BRIDGE_DLSS_DLAA && mode <= STREAMLINE_BRIDGE_DLSS_PERFORMANCE;
}

sl::DLSSOptions make_dlss_options(const StreamlineBridgeDlssOptions& input) noexcept {
    sl::DLSSOptions options{};
    options.mode = dlss_mode(input.mode);
    options.outputWidth = input.output_width;
    options.outputHeight = input.output_height;
    options.sharpness = input.sharpness;
    options.preExposure = input.pre_exposure;
    options.exposureScale = input.exposure_scale;
    options.colorBuffersHDR = input.color_buffers_hdr != 0 ? sl::eTrue : sl::eFalse;
    options.useAutoExposure = input.use_auto_exposure != 0 ? sl::eTrue : sl::eFalse;
    options.alphaUpscalingEnabled = input.alpha_upscaling_enabled != 0 ? sl::eTrue : sl::eFalse;
    return options;
}

StreamlineBridgeStatus check_token_and_viewport(
    StreamlineBridge* bridge,
    const StreamlineBridgeFrameToken* token,
    const StreamlineBridgeViewport* viewport) noexcept {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK)
        return STREAMLINE_BRIDGE_STATUS_NOT_INITIALIZED;
    if (token == nullptr || viewport == nullptr || token->token == nullptr ||
        !valid_header(token->struct_size, token->abi_version, sizeof(*token)) ||
        !valid_header(viewport->struct_size, viewport->abi_version, sizeof(*viewport))) {
        return set_error(bridge, "invalid frame token or viewport");
    }
    return STREAMLINE_BRIDGE_STATUS_OK;
}

void copy_matrix(sl::float4x4& destination, const float* source) noexcept {
    std::memcpy(destination.row, source, sizeof(destination.row));
}

sl::Boolean bool_value(uint32_t value) noexcept {
    return value != 0 ? sl::eTrue : sl::eFalse;
}

#if STREAMLINE_ENABLE_RR
sl::DLSSDOptions make_rr_options(const StreamlineBridgeRrOptions& input) noexcept {
    sl::DLSSDOptions options{};
    options.mode = dlss_mode(input.mode);
    options.outputWidth = input.output_width;
    options.outputHeight = input.output_height;
    options.sharpness = input.sharpness;
    options.preExposure = input.pre_exposure;
    options.exposureScale = input.exposure_scale;
    options.colorBuffersHDR = bool_value(input.color_buffers_hdr);
    options.indicatorInvertAxisX = bool_value(input.indicator_invert_axis_x);
    options.indicatorInvertAxisY = bool_value(input.indicator_invert_axis_y);
    options.normalRoughnessMode = static_cast<sl::DLSSDNormalRoughnessMode>(input.normal_roughness_mode);
    copy_matrix(options.worldToCameraView, input.world_to_camera_view);
    copy_matrix(options.cameraViewToWorld, input.camera_view_to_world);
    options.alphaUpscalingEnabled = bool_value(input.alpha_upscaling_enabled);
    options.dlaaPreset = static_cast<sl::DLSSDPreset>(input.dlaa_preset);
    options.qualityPreset = static_cast<sl::DLSSDPreset>(input.quality_preset);
    options.balancedPreset = static_cast<sl::DLSSDPreset>(input.balanced_preset);
    options.performancePreset = static_cast<sl::DLSSDPreset>(input.performance_preset);
    options.ultraPerformancePreset = static_cast<sl::DLSSDPreset>(input.ultra_performance_preset);
    options.ultraQualityPreset = static_cast<sl::DLSSDPreset>(input.ultra_quality_preset);
    return options;
}

bool valid_rr_options(const StreamlineBridgeRrOptions& input) noexcept {
    return valid_dlss_mode(input.mode) && input.output_width != 0 && input.output_height != 0 &&
           input.normal_roughness_mode <= 1;
}
#endif

#if STREAMLINE_ENABLE_FG
bool valid_fg_resource_description(const StreamlineBridgeFrameGenerationOptions& input) noexcept {
    return input.num_back_buffers != 0 && input.mvec_depth_width != 0 &&
           input.mvec_depth_height != 0 && input.color_width != 0 &&
           input.color_height != 0 && input.color_buffer_format != 0 &&
           input.mvec_buffer_format != 0 && input.depth_buffer_format != 0 &&
           input.hud_less_buffer_format != 0;
}

bool valid_fg_options(const StreamlineBridgeFrameGenerationOptions& input) noexcept {
    if (input.mode != STREAMLINE_BRIDGE_FRAME_GENERATION_OFF &&
        input.mode != STREAMLINE_BRIDGE_FRAME_GENERATION_ON)
        return false;
    if (input.num_frames_to_generate != 1 || input.flags != 0)
        return false;
    if (input.mode == STREAMLINE_BRIDGE_FRAME_GENERATION_ON)
        return valid_fg_resource_description(input);
    return true;
}

bool valid_fg_estimate_options(const StreamlineBridgeFrameGenerationOptions& input) noexcept {
    // SetOptions(eOff) may omit resource metadata, but a non-null GetState
    // options pointer requests a VRAM estimate and NVIDIA requires a complete
    // target swap-chain/input description even while interpolation is off.
    return valid_fg_options(input) && valid_fg_resource_description(input);
}

sl::DLSSGOptions make_fg_options(
    const StreamlineBridgeFrameGenerationOptions& input,
    bool request_vram_estimate) noexcept {
    sl::DLSSGOptions options{};
    options.mode = input.mode == STREAMLINE_BRIDGE_FRAME_GENERATION_ON
        ? sl::DLSSGMode::eOn
        : sl::DLSSGMode::eOff;
    options.numFramesToGenerate = 1;
    options.flags = request_vram_estimate
        ? sl::DLSSGFlags::eRequestVRAMEstimate
        : sl::DLSSGFlags{};
    options.numBackBuffers = input.num_back_buffers;
    options.mvecDepthWidth = input.mvec_depth_width;
    options.mvecDepthHeight = input.mvec_depth_height;
    options.colorWidth = input.color_width;
    options.colorHeight = input.color_height;
    options.colorBufferFormat = input.color_buffer_format;
    options.mvecBufferFormat = input.mvec_buffer_format;
    options.depthBufferFormat = input.depth_buffer_format;
    options.uiBufferFormat = 0;
    options.hudLessBufferFormat = input.hud_less_buffer_format;
    // Let the SDK choose queue parallelism; forcing a mode changes the
    // application's Present contract and is outside this first FG package.
    options.queueParallelismMode = sl::DLSSGQueueParallelismMode{};
    options.enableUserInterfaceRecomposition = sl::eFalse;
    return options;
}
#endif

} // namespace

StreamlineBridgeStatus streamline_bridge_create(
    const StreamlineBridgeInitDesc* desc,
    StreamlineBridge** out_bridge) {
    if (out_bridge == nullptr)
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    *out_bridge = nullptr;
    if (desc == nullptr || !valid_header(desc->struct_size, desc->abi_version, sizeof(*desc)))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;

    try {
        std::unique_ptr<StreamlineBridge> bridge(new (std::nothrow) StreamlineBridge());
        if (!bridge)
            return STREAMLINE_BRIDGE_STATUS_EXCEPTION;

        const bool has_application_id = desc->application_id != 0;
        const bool has_project_identity =
            desc->project_id != nullptr && desc->project_id[0] != '\0' &&
            desc->engine_version != nullptr && desc->engine_version[0] != '\0';
        const bool ngx_requested =
            desc->enable_dlss != 0 || desc->enable_dlss_rr != 0 || desc->enable_dlss_fg != 0;
        if (ngx_requested && !has_application_id && !has_project_identity)
            return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;

        std::array<sl::Feature, 5> features{};
        size_t feature_count = 0;
        features[feature_count++] = sl::kFeatureReflex;
        features[feature_count++] = sl::kFeaturePCL;
        if (desc->enable_dlss != 0)
            features[feature_count++] = sl::kFeatureDLSS;
#if STREAMLINE_ENABLE_RR
        if (desc->enable_dlss_rr != 0)
            features[feature_count++] = sl::kFeatureDLSS_RR;
#endif
#if STREAMLINE_ENABLE_FG
        if (desc->enable_dlss_fg != 0)
            features[feature_count++] = sl::kFeatureDLSS_G;
#endif
#if !STREAMLINE_ENABLE_RR
        if (desc->enable_dlss_rr != 0)
            return STREAMLINE_BRIDGE_STATUS_UNSUPPORTED;
#endif
#if !STREAMLINE_ENABLE_FG
        if (desc->enable_dlss_fg != 0)
            return STREAMLINE_BRIDGE_STATUS_UNSUPPORTED;
#endif
        bridge->rr_requested = desc->enable_dlss_rr != 0;
        bridge->fg_requested = desc->enable_dlss_fg != 0;
        sl::Preferences preferences{};
        preferences.showConsole = desc->development != 0;
        // Production diagnostics must not contaminate the benchmark's
        // machine-readable stdout contract. Errors still propagate through
        // Streamline return codes and the bridge's explicit error channel.
        preferences.logLevel = desc->development != 0
            ? sl::LogLevel::eDefault
            : sl::LogLevel::eOff;
        preferences.flags = sl::PreferenceFlags::eDisableCLStateTracking |
                            sl::PreferenceFlags::eDisableDebugText |
                            sl::PreferenceFlags::eUseManualHooking |
                            sl::PreferenceFlags::eUseFrameBasedResourceTagging |
                            // NVIDIA recommends OTA so new drivers can select
                            // compatible signed NGX/Streamline revisions.
                            sl::PreferenceFlags::eAllowOTA |
                            sl::PreferenceFlags::eLoadDownloadedPlugins;
        preferences.featuresToLoad = features.data();
        // Build the list explicitly so RR and FG are independently opt-in;
        // truncating one optional feature from a shared tail could silently
        // load the other plugin in a feature-off build.
        preferences.numFeaturesToLoad = static_cast<uint32_t>(feature_count);
        preferences.applicationId = desc->application_id;
        if (!has_application_id && has_project_identity) {
            preferences.engine = sl::EngineType::eCustom;
            preferences.engineVersion = desc->engine_version;
            preferences.projectId = desc->project_id;
        }
        const wchar_t* plugin_paths[] = {desc->plugin_path};
        preferences.pathsToPlugins = desc->plugin_path == nullptr ? nullptr : plugin_paths;
        preferences.numPathsToPlugins = desc->plugin_path == nullptr ? 0 : 1;
        preferences.pathToLogsAndData = desc->log_path;
        preferences.renderAPI = sl::RenderAPI::eD3D12;
        // Install the redirect before Streamline loads any plugin because its
        // verifier may cache the process stdout handle until plugin shutdown.
        bridge->stdout_redirect = std::make_unique<ScopedStdoutToStderr>();
        const sl::Result result = slInit(preferences, sl::kSDKVersion);
        if (result != sl::Result::eOk)
            return set_error(bridge.get(), "slInit failed", result);
        bridge->initialized = true;
        *out_bridge = bridge.release();
        return STREAMLINE_BRIDGE_STATUS_OK;
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_set_d3d_device(
    StreamlineBridge* bridge,
    void* d3d_device,
    const uint8_t* adapter_luid,
    size_t adapter_luid_size) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || d3d_device == nullptr ||
        adapter_luid == nullptr || adapter_luid_size != sizeof(uint64_t))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        const sl::Result result = slSetD3DDevice(d3d_device);
        if (result != sl::Result::eOk)
            return set_error(bridge, "slSetD3DDevice failed", result);
        std::memcpy(&bridge->adapter_luid, adapter_luid, sizeof(bridge->adapter_luid));
        sl::AdapterInfo adapter{};
        adapter.deviceLUID = const_cast<uint8_t*>(adapter_luid);
        adapter.deviceLUIDSizeInBytes = static_cast<uint32_t>(adapter_luid_size);
        bridge->support_results.dlss = static_cast<uint32_t>(
            slIsFeatureSupported(sl::kFeatureDLSS, adapter));
        bridge->support_results.reflex = static_cast<uint32_t>(
            slIsFeatureSupported(sl::kFeatureReflex, adapter));
        bridge->support_results.pcl = static_cast<uint32_t>(
            slIsFeatureSupported(sl::kFeaturePCL, adapter));
#if STREAMLINE_ENABLE_RR
        bridge->support_results.rr = bridge->rr_requested
            ? static_cast<uint32_t>(slIsFeatureSupported(sl::kFeatureDLSS_RR, adapter))
            : static_cast<uint32_t>(sl::Result::eErrorFeatureNotSupported);
#else
        bridge->support_results.rr = static_cast<uint32_t>(sl::Result::eErrorFeatureNotSupported);
#endif
#if STREAMLINE_ENABLE_FG
        bridge->support_results.fg = bridge->fg_requested
            ? static_cast<uint32_t>(slIsFeatureSupported(sl::kFeatureDLSS_G, adapter))
            : static_cast<uint32_t>(sl::Result::eErrorFeatureNotSupported);
#else
        bridge->support_results.fg = static_cast<uint32_t>(sl::Result::eErrorFeatureNotSupported);
#endif
        bridge->device_set = true;
        return STREAMLINE_BRIDGE_STATUS_OK;
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_query_support(
    StreamlineBridge* bridge,
    StreamlineBridgeSupport* out_support) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || out_support == nullptr ||
        !valid_header(out_support->struct_size, out_support->abi_version, sizeof(*out_support)))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    if (!bridge->device_set)
        return STREAMLINE_BRIDGE_STATUS_NOT_INITIALIZED;
    *out_support = {};
    out_support->struct_size = sizeof(*out_support);
    out_support->abi_version = STREAMLINE_BRIDGE_ABI_VERSION;
    out_support->dlss_result = bridge->support_results.dlss;
    out_support->reflex_result = bridge->support_results.reflex;
    out_support->pcl_result = bridge->support_results.pcl;
    out_support->rr_result = bridge->support_results.rr;
    out_support->fg_result = bridge->support_results.fg;
    out_support->dlss_supported = out_support->dlss_result == 0;
    out_support->reflex_supported = out_support->reflex_result == 0;
    out_support->pcl_supported = out_support->pcl_result == 0;
    out_support->rr_supported = out_support->rr_result == 0;
    out_support->fg_supported = out_support->fg_result == 0;
    out_support->adapter_luid = bridge->adapter_luid;
    std::strncpy(out_support->sdk_version, "2.14.1", sizeof(out_support->sdk_version) - 1);
    return STREAMLINE_BRIDGE_STATUS_OK;
}

StreamlineBridgeStatus streamline_bridge_get_optimal_settings(
    StreamlineBridge* bridge,
    const StreamlineBridgeDlssOptions* input,
    StreamlineBridgeOptimalSettings* out_settings) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || input == nullptr || out_settings == nullptr ||
        !valid_header(input->struct_size, input->abi_version, sizeof(*input)) ||
        !valid_header(out_settings->struct_size, out_settings->abi_version, sizeof(*out_settings)) ||
        !valid_dlss_mode(input->mode) || input->output_width == 0 || input->output_height == 0)
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        const sl::DLSSOptions options = make_dlss_options(*input);
        sl::DLSSOptimalSettings settings{};
        const sl::Result result = slDLSSGetOptimalSettings(options, settings);
        if (result != sl::Result::eOk)
            return set_error(bridge, "slDLSSGetOptimalSettings failed", result);
        *out_settings = {};
        out_settings->struct_size = sizeof(*out_settings);
        out_settings->abi_version = STREAMLINE_BRIDGE_ABI_VERSION;
        out_settings->optimal_render_width = settings.optimalRenderWidth;
        out_settings->optimal_render_height = settings.optimalRenderHeight;
        out_settings->render_width_min = settings.renderWidthMin;
        out_settings->render_height_min = settings.renderHeightMin;
        out_settings->render_width_max = settings.renderWidthMax;
        out_settings->render_height_max = settings.renderHeightMax;
        out_settings->optimal_sharpness = settings.optimalSharpness;
        return STREAMLINE_BRIDGE_STATUS_OK;
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_dlss_set_options(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeDlssOptions* input) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || input == nullptr || viewport == nullptr ||
        !valid_header(input->struct_size, input->abi_version, sizeof(*input)) ||
        !valid_header(viewport->struct_size, viewport->abi_version, sizeof(*viewport)) ||
        !valid_dlss_mode(input->mode))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        const sl::Result result = slDLSSSetOptions(sl::ViewportHandle(viewport->id), make_dlss_options(*input));
        return result == sl::Result::eOk ? STREAMLINE_BRIDGE_STATUS_OK
                                         : set_error(bridge, "slDLSSSetOptions failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

#if STREAMLINE_ENABLE_RR
StreamlineBridgeStatus streamline_bridge_rr_get_optimal_settings(
    StreamlineBridge* bridge,
    const StreamlineBridgeRrOptions* input,
    StreamlineBridgeRrOptimalSettings* out_settings) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || input == nullptr || out_settings == nullptr ||
        !valid_header(input->struct_size, input->abi_version, sizeof(*input)) ||
        !valid_header(out_settings->struct_size, out_settings->abi_version, sizeof(*out_settings)) ||
        !valid_rr_options(*input))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        const sl::DLSSDOptions options = make_rr_options(*input);
        sl::DLSSDOptimalSettings settings{};
        const sl::Result result = slDLSSDGetOptimalSettings(options, settings);
        if (result != sl::Result::eOk)
            return set_error(bridge, "slDLSSDGetOptimalSettings failed", result);
        *out_settings = {};
        out_settings->struct_size = sizeof(*out_settings);
        out_settings->abi_version = STREAMLINE_BRIDGE_ABI_VERSION;
        out_settings->optimal_render_width = settings.optimalRenderWidth;
        out_settings->optimal_render_height = settings.optimalRenderHeight;
        out_settings->render_width_min = settings.renderWidthMin;
        out_settings->render_height_min = settings.renderHeightMin;
        out_settings->render_width_max = settings.renderWidthMax;
        out_settings->render_height_max = settings.renderHeightMax;
        out_settings->optimal_sharpness = settings.optimalSharpness;
        return STREAMLINE_BRIDGE_STATUS_OK;
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_rr_set_options(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeRrOptions* input) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || input == nullptr || viewport == nullptr ||
        !valid_header(input->struct_size, input->abi_version, sizeof(*input)) ||
        !valid_header(viewport->struct_size, viewport->abi_version, sizeof(*viewport)) ||
        !valid_rr_options(*input))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        const sl::Result result = slDLSSDSetOptions(
            sl::ViewportHandle(viewport->id), make_rr_options(*input));
        return result == sl::Result::eOk ? STREAMLINE_BRIDGE_STATUS_OK
                                         : set_error(bridge, "slDLSSDSetOptions failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_rr_get_state(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    StreamlineBridgeRrState* out_state) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || viewport == nullptr || out_state == nullptr ||
        !valid_header(viewport->struct_size, viewport->abi_version, sizeof(*viewport)) ||
        !valid_header(out_state->struct_size, out_state->abi_version, sizeof(*out_state)))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        sl::DLSSDState state{};
        const sl::Result result = slDLSSDGetState(sl::ViewportHandle(viewport->id), state);
        if (result != sl::Result::eOk)
            return set_error(bridge, "slDLSSDGetState failed", result);
        *out_state = {};
        out_state->struct_size = sizeof(*out_state);
        out_state->abi_version = STREAMLINE_BRIDGE_ABI_VERSION;
        out_state->estimated_vram_usage_bytes = state.estimatedVRAMUsageInBytes;
        return STREAMLINE_BRIDGE_STATUS_OK;
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}
#endif

#if STREAMLINE_ENABLE_FG
StreamlineBridgeStatus streamline_bridge_fg_set_options(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeFrameGenerationOptions* input) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || viewport == nullptr || input == nullptr ||
        !valid_header(viewport->struct_size, viewport->abi_version, sizeof(*viewport)) ||
        !valid_header(input->struct_size, input->abi_version, sizeof(*input)) ||
        !valid_fg_options(*input))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        const sl::Result result = slDLSSGSetOptions(
            sl::ViewportHandle(viewport->id), make_fg_options(*input, false));
        return result == sl::Result::eOk
            ? STREAMLINE_BRIDGE_STATUS_OK
            : set_error(bridge, "slDLSSGSetOptions failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_fg_get_state(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeFrameGenerationOptions* estimate_input,
    StreamlineBridgeFrameGenerationState* out_state) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || viewport == nullptr || out_state == nullptr ||
        !valid_header(viewport->struct_size, viewport->abi_version, sizeof(*viewport)) ||
        !valid_header(out_state->struct_size, out_state->abi_version, sizeof(*out_state)))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    if (estimate_input != nullptr &&
        (!valid_header(estimate_input->struct_size, estimate_input->abi_version, sizeof(*estimate_input)) ||
         !valid_fg_estimate_options(*estimate_input)))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        sl::DLSSGState state{};
        std::optional<sl::DLSSGOptions> estimate;
        if (estimate_input != nullptr)
            estimate = make_fg_options(*estimate_input, true);
        const sl::Result result = slDLSSGGetState(
            sl::ViewportHandle(viewport->id), state, estimate ? &*estimate : nullptr);
        if (result != sl::Result::eOk)
            return set_error(bridge, "slDLSSGGetState failed", result);
        *out_state = {};
        out_state->struct_size = sizeof(*out_state);
        out_state->abi_version = STREAMLINE_BRIDGE_ABI_VERSION;
        out_state->status_raw = static_cast<uint32_t>(state.status);
        out_state->min_width_or_height = state.minWidthOrHeight;
        out_state->num_frames_actually_presented = state.numFramesActuallyPresented;
        out_state->num_frames_to_generate_max = state.numFramesToGenerateMax;
        out_state->estimated_vram_usage_bytes = state.estimatedVRAMUsageInBytes;
        out_state->vsync_support_available = state.bIsVsyncSupportAvailable == sl::eTrue;
        out_state->dynamic_mfg_supported = state.bIsDynamicMFGSupported == sl::eTrue;
        return STREAMLINE_BRIDGE_STATUS_OK;
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}
#else
StreamlineBridgeStatus streamline_bridge_fg_set_options(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeFrameGenerationOptions* input) {
    (void)bridge;
    (void)viewport;
    (void)input;
    return STREAMLINE_BRIDGE_STATUS_UNSUPPORTED;
}

StreamlineBridgeStatus streamline_bridge_fg_get_state(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeFrameGenerationOptions* estimate_input,
    StreamlineBridgeFrameGenerationState* out_state) {
    (void)bridge;
    (void)viewport;
    (void)estimate_input;
    (void)out_state;
    return STREAMLINE_BRIDGE_STATUS_UNSUPPORTED;
}
#endif

#if STREAMLINE_ENABLE_FG
StreamlineBridgeStatus streamline_bridge_fg_set_loaded(
    StreamlineBridge* bridge,
    uint32_t loaded) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK)
        return STREAMLINE_BRIDGE_STATUS_NOT_INITIALIZED;
    if (loaded > 1)
        return set_error(bridge, "invalid DLSS-G loaded value");
    if (!bridge->fg_requested)
        return STREAMLINE_BRIDGE_STATUS_UNSUPPORTED;
    if (!bridge->device_set)
        return STREAMLINE_BRIDGE_STATUS_NOT_INITIALIZED;
    try {
        const sl::Result result = slSetFeatureLoaded(
            sl::kFeatureDLSS_G, loaded != 0);
        if (result != sl::Result::eOk)
            return set_error(bridge, "slSetFeatureLoaded(kFeatureDLSS_G) failed", result);

        bool verified_loaded = false;
        const sl::Result verify_result = slIsFeatureLoaded(
            sl::kFeatureDLSS_G, verified_loaded);
        if (verify_result != sl::Result::eOk)
            return set_error(bridge, "slIsFeatureLoaded(kFeatureDLSS_G) failed", verify_result);
        if (verified_loaded != (loaded != 0)) {
            bridge->last_error = "slSetFeatureLoaded(kFeatureDLSS_G) verification mismatch";
            return set_error(
                bridge,
                "slSetFeatureLoaded(kFeatureDLSS_G) verification mismatch",
                sl::Result::eErrorInvalidState);
        }
        return STREAMLINE_BRIDGE_STATUS_OK;
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_fg_is_loaded(
    StreamlineBridge* bridge,
    uint32_t* out_loaded) {
    if (out_loaded != nullptr)
        *out_loaded = 0;
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || out_loaded == nullptr)
        return out_loaded == nullptr
            ? STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT
            : STREAMLINE_BRIDGE_STATUS_NOT_INITIALIZED;
    if (!bridge->fg_requested)
        return STREAMLINE_BRIDGE_STATUS_UNSUPPORTED;
    if (!bridge->device_set)
        return STREAMLINE_BRIDGE_STATUS_NOT_INITIALIZED;
    try {
        bool loaded = false;
        const sl::Result result = slIsFeatureLoaded(sl::kFeatureDLSS_G, loaded);
        if (result != sl::Result::eOk)
            return set_error(bridge, "slIsFeatureLoaded(kFeatureDLSS_G) failed", result);
        *out_loaded = loaded ? 1 : 0;
        return STREAMLINE_BRIDGE_STATUS_OK;
    } catch (...) {
        *out_loaded = 0;
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}
#else
StreamlineBridgeStatus streamline_bridge_fg_set_loaded(
    StreamlineBridge* bridge,
    uint32_t loaded) {
    (void)bridge;
    (void)loaded;
    return STREAMLINE_BRIDGE_STATUS_UNSUPPORTED;
}

StreamlineBridgeStatus streamline_bridge_fg_is_loaded(
    StreamlineBridge* bridge,
    uint32_t* out_loaded) {
    (void)bridge;
    (void)out_loaded;
    return STREAMLINE_BRIDGE_STATUS_UNSUPPORTED;
}
#endif

StreamlineBridgeStatus streamline_bridge_allocate_resources(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport,
    void* command_list) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || viewport == nullptr ||
        !valid_header(viewport->struct_size, viewport->abi_version, sizeof(*viewport)))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        const sl::Result result = slAllocateResources(
            static_cast<sl::CommandBuffer*>(command_list), sl::kFeatureDLSS, sl::ViewportHandle(viewport->id));
        return result == sl::Result::eOk ? STREAMLINE_BRIDGE_STATUS_OK
                                         : set_error(bridge, "slAllocateResources failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_free_resources(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || viewport == nullptr ||
        !valid_header(viewport->struct_size, viewport->abi_version, sizeof(*viewport)))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        const sl::Result result = slFreeResources(sl::kFeatureDLSS, sl::ViewportHandle(viewport->id));
        return result == sl::Result::eOk ? STREAMLINE_BRIDGE_STATUS_OK
                                         : set_error(bridge, "slFreeResources failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_rr_free_resources(
    StreamlineBridge* bridge,
    const StreamlineBridgeViewport* viewport) {
#if STREAMLINE_ENABLE_RR
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || viewport == nullptr ||
        !valid_header(viewport->struct_size, viewport->abi_version, sizeof(*viewport)))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        const sl::Result result = slFreeResources(sl::kFeatureDLSS_RR, sl::ViewportHandle(viewport->id));
        return result == sl::Result::eOk ? STREAMLINE_BRIDGE_STATUS_OK
                                         : set_error(bridge, "slFreeResources(DLSS RR) failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
#else
    (void)bridge;
    (void)viewport;
    return STREAMLINE_BRIDGE_STATUS_UNSUPPORTED;
#endif
}

StreamlineBridgeStatus streamline_bridge_get_frame_token(
    StreamlineBridge* bridge,
    uint32_t frame_index,
    StreamlineBridgeFrameToken* out_token) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || out_token == nullptr ||
        !valid_header(out_token->struct_size, out_token->abi_version, sizeof(*out_token)))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        sl::FrameToken* token = nullptr;
        const uint32_t index = frame_index;
        const sl::Result result = slGetNewFrameToken(token, &index);
        if (result != sl::Result::eOk || token == nullptr)
            return result == sl::Result::eOk ? STREAMLINE_BRIDGE_STATUS_SDK_ERROR
                                               : set_error(bridge, "slGetNewFrameToken failed", result);
        out_token->token = token;
        out_token->frame_index = frame_index;
        return STREAMLINE_BRIDGE_STATUS_OK;
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_set_constants(
    StreamlineBridge* bridge,
    const StreamlineBridgeFrameToken* token,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeConstants* input) {
    if (input == nullptr || !valid_header(input->struct_size, input->abi_version, sizeof(*input)))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    const auto valid = check_token_and_viewport(bridge, token, viewport);
    if (valid != STREAMLINE_BRIDGE_STATUS_OK)
        return valid;
    try {
        sl::Constants constants{};
        copy_matrix(constants.cameraViewToClip, input->camera_view_to_clip);
        copy_matrix(constants.clipToCameraView, input->clip_to_camera_view);
        copy_matrix(constants.clipToPrevClip, input->clip_to_prev_clip);
        copy_matrix(constants.prevClipToClip, input->prev_clip_to_clip);
        std::memcpy(&constants.jitterOffset, input->jitter_offset, sizeof(input->jitter_offset));
        std::memcpy(&constants.mvecScale, input->mvec_scale, sizeof(input->mvec_scale));
        std::memcpy(
            &constants.cameraPinholeOffset,
            input->camera_pinhole_offset,
            sizeof(input->camera_pinhole_offset));
        std::memcpy(&constants.cameraPos, input->camera_position, sizeof(input->camera_position));
        std::memcpy(&constants.cameraUp, input->camera_up, sizeof(input->camera_up));
        std::memcpy(&constants.cameraRight, input->camera_right, sizeof(input->camera_right));
        std::memcpy(&constants.cameraFwd, input->camera_forward, sizeof(input->camera_forward));
        constants.cameraNear = input->camera_near;
        constants.cameraFar = input->camera_far;
        constants.cameraFOV = input->camera_fov;
        constants.cameraAspectRatio = input->camera_aspect_ratio;
        constants.depthInverted = bool_value(input->depth_inverted);
        constants.cameraMotionIncluded = bool_value(input->camera_motion_included);
        constants.motionVectors3D = bool_value(input->motion_vectors_3d);
        constants.reset = bool_value(input->reset);
        constants.motionVectorsJittered = bool_value(input->motion_vectors_jittered);
        constants.minRelativeLinearDepthObjectSeparation =
            input->min_relative_linear_depth_object_separation;
        const sl::Result result = slSetConstants(
            constants, *static_cast<sl::FrameToken*>(token->token), sl::ViewportHandle(viewport->id));
        return result == sl::Result::eOk ? STREAMLINE_BRIDGE_STATUS_OK
                                         : set_error(bridge, "slSetConstants failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_set_tags(
    StreamlineBridge* bridge,
    const StreamlineBridgeFrameToken* token,
    const StreamlineBridgeViewport* viewport,
    const StreamlineBridgeResourceTag* input,
    uint32_t tag_count,
    void* command_list) {
    const auto valid = check_token_and_viewport(bridge, token, viewport);
    if (valid != STREAMLINE_BRIDGE_STATUS_OK || input == nullptr || tag_count == 0 || tag_count > 16)
        return valid == STREAMLINE_BRIDGE_STATUS_OK ? STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT : valid;
    try {
        std::vector<sl::Resource> resources;
        std::vector<sl::ResourceTag> tags;
        resources.reserve(tag_count);
        tags.reserve(tag_count);
        for (uint32_t i = 0; i < tag_count; ++i) {
            const auto& source = input[i];
            if (!valid_header(source.struct_size, source.abi_version, sizeof(source)) ||
                !valid_buffer_type(source.buffer_type) || !valid_lifecycle(source.lifecycle))
                return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
            const bool null_resource = source.resource == nullptr;
            const bool zero_extent = source.top == 0 && source.left == 0 &&
                source.width == 0 && source.height == 0;
            if (null_resource) {
                if (source.buffer_type == sl::kBufferTypeBackbuffer &&
                    source.state == 0 && source.width != 0 && source.height != 0 &&
                    source.lifecycle == sl::ResourceLifecycle::eOnlyValidNow) {
                    // Streamline already owns the intercepted backbuffer. A
                    // null resource plus a non-empty extent is the SDK's
                    // documented way to describe its FG region.
                    sl::Extent extent{source.top, source.left, source.width, source.height};
                    tags.emplace_back(
                        nullptr, source.buffer_type,
                        sl::ResourceLifecycle::eOnlyValidNow, &extent);
                    continue;
                }
                // Null tags are the SDK's explicit way to release resources
                // whose validity ends at Present.  Rejecting a non-zero
                // extent/state prevents an accidental unowned binding.
                if (source.state != 0 || !zero_extent || source.lifecycle != 1)
                    return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
                tags.emplace_back(
                    nullptr, source.buffer_type, sl::ResourceLifecycle::eValidUntilPresent, nullptr);
                continue;
            }
            if (source.lifecycle != 1 && command_list == nullptr)
                return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
            resources.emplace_back(sl::ResourceType::eTex2d, source.resource, source.state);
            resources.back().width = source.width;
            resources.back().height = source.height;
            sl::Extent extent{source.top, source.left, source.width, source.height};
            tags.emplace_back(
                &resources.back(), source.buffer_type,
                static_cast<sl::ResourceLifecycle>(source.lifecycle), &extent);
        }
        const sl::Result result = slSetTagForFrame(
            *static_cast<sl::FrameToken*>(token->token), sl::ViewportHandle(viewport->id), tags.data(), tag_count,
            static_cast<sl::CommandBuffer*>(command_list));
        return result == sl::Result::eOk ? STREAMLINE_BRIDGE_STATUS_OK
                                         : set_error(bridge, "slSetTagForFrame failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_evaluate_dlss(
    StreamlineBridge* bridge,
    const StreamlineBridgeFrameToken* token,
    const StreamlineBridgeViewport* viewport,
    void* command_list) {
    const auto valid = check_token_and_viewport(bridge, token, viewport);
    if (valid != STREAMLINE_BRIDGE_STATUS_OK || command_list == nullptr)
        return valid == STREAMLINE_BRIDGE_STATUS_OK ? STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT : valid;
    try {
        sl::ViewportHandle handle(viewport->id);
        const sl::BaseStructure* inputs[] = {&handle};
        const sl::Result result = slEvaluateFeature(
            sl::kFeatureDLSS, *static_cast<sl::FrameToken*>(token->token), inputs, 1,
            static_cast<sl::CommandBuffer*>(command_list));
        return result == sl::Result::eOk ? STREAMLINE_BRIDGE_STATUS_OK
                                         : set_error(bridge, "slEvaluateFeature(DLSS) failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_evaluate_rr(
    StreamlineBridge* bridge,
    const StreamlineBridgeFrameToken* token,
    const StreamlineBridgeViewport* viewport,
    void* command_list) {
#if STREAMLINE_ENABLE_RR
    const auto valid = check_token_and_viewport(bridge, token, viewport);
    if (valid != STREAMLINE_BRIDGE_STATUS_OK || command_list == nullptr)
        return valid == STREAMLINE_BRIDGE_STATUS_OK ? STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT : valid;
    try {
        sl::ViewportHandle handle(viewport->id);
        const sl::BaseStructure* inputs[] = {&handle};
        const sl::Result result = slEvaluateFeature(
            sl::kFeatureDLSS_RR, *static_cast<sl::FrameToken*>(token->token), inputs, 1,
            static_cast<sl::CommandBuffer*>(command_list));
        return result == sl::Result::eOk ? STREAMLINE_BRIDGE_STATUS_OK
                                         : set_error(bridge, "slEvaluateFeature(DLSS RR) failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
#else
    (void)bridge;
    (void)token;
    (void)viewport;
    (void)command_list;
    return STREAMLINE_BRIDGE_STATUS_UNSUPPORTED;
#endif
}

StreamlineBridgeStatus streamline_bridge_reflex_set_mode(StreamlineBridge* bridge, uint32_t mode) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || mode > STREAMLINE_BRIDGE_REFLEX_ON_BOOST)
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        sl::ReflexOptions options{};
        options.mode = static_cast<sl::ReflexMode>(mode);
        const sl::Result result = slReflexSetOptions(options);
        return result == sl::Result::eOk ? STREAMLINE_BRIDGE_STATUS_OK
                                         : set_error(bridge, "slReflexSetOptions failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_reflex_sleep(
    StreamlineBridge* bridge,
    const StreamlineBridgeFrameToken* token) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || token == nullptr || token->token == nullptr ||
        !valid_header(token->struct_size, token->abi_version, sizeof(*token)))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        const sl::Result result = slReflexSleep(*static_cast<sl::FrameToken*>(token->token));
        return result == sl::Result::eOk ? STREAMLINE_BRIDGE_STATUS_OK
                                         : set_error(bridge, "slReflexSleep failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_pcl_marker(
    StreamlineBridge* bridge,
    const StreamlineBridgeFrameToken* token,
    uint32_t marker) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || token == nullptr || token->token == nullptr ||
        marker > static_cast<uint32_t>(sl::PCLMarker::eNumPresentsInBatch) ||
        !valid_header(token->struct_size, token->abi_version, sizeof(*token)))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        const sl::Result result = slPCLSetMarker(
            static_cast<sl::PCLMarker>(marker), *static_cast<sl::FrameToken*>(token->token));
        return result == sl::Result::eOk ? STREAMLINE_BRIDGE_STATUS_OK
                                         : set_error(bridge, "slPCLSetMarker failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_reflex_get_state(
    StreamlineBridge* bridge,
    StreamlineBridgeReflexState* out_state) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || out_state == nullptr ||
        !valid_header(out_state->struct_size, out_state->abi_version, sizeof(*out_state)))
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        sl::ReflexState state{};
        const sl::Result result = slReflexGetState(state);
        if (result != sl::Result::eOk)
            return set_error(bridge, "slReflexGetState failed", result);
        *out_state = {};
        out_state->struct_size = sizeof(*out_state);
        out_state->abi_version = STREAMLINE_BRIDGE_ABI_VERSION;
        out_state->low_latency_available = state.lowLatencyAvailable ? 1 : 0;
        out_state->latency_report_available = state.latencyReportAvailable ? 1 : 0;
        out_state->flash_indicator_driver_controlled = state.flashIndicatorDriverControlled ? 1 : 0;
        return STREAMLINE_BRIDGE_STATUS_OK;
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_upgrade_interface(
    StreamlineBridge* bridge,
    void** interface_ptr) {
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK || interface_ptr == nullptr || *interface_ptr == nullptr)
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    try {
        void* native_interface = nullptr;
        const sl::Result native_result = slGetNativeInterface(*interface_ptr, &native_interface);
        if (native_result != sl::Result::eOk)
            return set_error(bridge, "slGetNativeInterface failed", native_result);
        if (native_interface == nullptr)
            return set_error(bridge, "slGetNativeInterface returned null");
        const bool already_upgraded = native_interface != *interface_ptr;
        static_cast<IUnknown*>(native_interface)->Release();
        if (already_upgraded)
            return STREAMLINE_BRIDGE_STATUS_ALREADY_UPGRADED;

        const sl::Result result = slUpgradeInterface(interface_ptr);
        if (result == sl::Result::eOk)
            return STREAMLINE_BRIDGE_STATUS_OK;
        return set_error(bridge, "slUpgradeInterface failed", result);
    } catch (...) {
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

StreamlineBridgeStatus streamline_bridge_get_native_interface(
    StreamlineBridge* bridge,
    void* proxy_interface,
    void** out_native_interface) {
    if (out_native_interface != nullptr)
        *out_native_interface = nullptr;
    if (check_bridge(bridge) != STREAMLINE_BRIDGE_STATUS_OK ||
        proxy_interface == nullptr || out_native_interface == nullptr)
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    void* native_interface = nullptr;
    try {
        const sl::Result result = slGetNativeInterface(proxy_interface, &native_interface);
        if (result != sl::Result::eOk) {
            if (native_interface != nullptr)
                static_cast<IUnknown*>(native_interface)->Release();
            return set_error(bridge, "slGetNativeInterface failed", result);
        }
        if (native_interface == nullptr)
            return set_error(bridge, "slGetNativeInterface returned null");
        *out_native_interface = native_interface;
        return STREAMLINE_BRIDGE_STATUS_OK;
    } catch (...) {
        if (native_interface != nullptr)
            static_cast<IUnknown*>(native_interface)->Release();
        *out_native_interface = nullptr;
        return STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
}

size_t streamline_bridge_copy_last_error(
    const StreamlineBridge* bridge,
    char* destination,
    size_t capacity) {
    if (destination == nullptr || capacity == 0)
        return 0;
    const char* source = bridge == nullptr ? "invalid Streamline bridge" : bridge->last_error.c_str();
    const size_t length = std::strlen(source);
    const size_t copied = std::min(length, capacity - 1);
    std::memcpy(destination, source, copied);
    destination[copied] = '\0';
    return copied;
}

StreamlineBridgeStatus streamline_bridge_shutdown(StreamlineBridge* bridge) {
    if (bridge == nullptr)
        return STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT;
    StreamlineBridgeStatus status = STREAMLINE_BRIDGE_STATUS_OK;
    try {
        if (bridge->initialized) {
            // NVIDIA's signature verifier writes informational messages to
            // stdout during shutdown even when Streamline logging is off.
            // Keep those diagnostics, but route them to stderr so benchmark
            // stdout remains one machine-readable JSON record.
            const sl::Result result = slShutdown();
            if (result != sl::Result::eOk)
                status = set_error(bridge, "slShutdown failed", result);
        }
        bridge->stdout_redirect.reset();
    } catch (...) {
        status = STREAMLINE_BRIDGE_STATUS_EXCEPTION;
    }
    delete bridge;
    return status;
}
