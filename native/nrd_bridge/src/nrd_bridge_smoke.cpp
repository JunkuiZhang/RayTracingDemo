#include "nrd_bridge.h"

#include <d3d12.h>

#include <cstdio>

int main() {
    ID3D12Device* device = nullptr;
    HRESULT hr = D3D12CreateDevice(nullptr, D3D_FEATURE_LEVEL_12_0, IID_PPV_ARGS(&device));
    if (FAILED(hr)) {
        std::fprintf(stderr, "D3D12CreateDevice failed: 0x%08lx\n", static_cast<unsigned long>(hr));
        return 2;
    }

    NrdBridgeVersion version = {};
    NrdBridgeStatus status = nrd_bridge_query_version(&version);
    if (status != NRD_BRIDGE_STATUS_OK) {
        std::fprintf(stderr, "nrd_bridge_query_version failed: %u\n", status);
        device->Release();
        return 3;
    }

    NrdBridgeCreateDesc desc = {};
    desc.abi_version = NRD_BRIDGE_ABI_VERSION;
    desc.resource_width = 1280;
    desc.resource_height = 720;
    desc.queued_frames = NRD_BRIDGE_QUEUED_FRAMES;
    desc.device = device;

    NrdBridge* bridge = nullptr;
    status = nrd_bridge_create(&desc, &bridge);
    if (status != NRD_BRIDGE_STATUS_OK || bridge == nullptr) {
        std::fprintf(stderr, "nrd_bridge_create failed: %u\n", status);
        device->Release();
        return 4;
    }

    nrd_bridge_destroy(bridge);
    device->Release();
    std::printf("nrd_bridge_smoke ok version=%u.%u.%u commit=%s\n", version.major, version.minor, version.build, version.commit);
    return 0;
}
