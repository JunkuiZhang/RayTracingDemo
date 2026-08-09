#include "streamline_bridge.h"

#include <cstdio>

int main() {
    StreamlineBridge* bridge = nullptr;
    const StreamlineBridgeStatus status = streamline_bridge_create(nullptr, &bridge);
    std::printf("streamline_bridge_invalid_create=%u\n", status);
    if (bridge != nullptr)
        streamline_bridge_shutdown(bridge);
    return status == STREAMLINE_BRIDGE_STATUS_INVALID_ARGUMENT ? 0 : 1;
}
