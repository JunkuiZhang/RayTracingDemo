// Resolve only the pixels whose stable, unjittered primary visibility says
// that a real opaque surface boundary is nearby. Non-boundary pixels are
// copied from the current RR HDR value exactly; a full-screen temporal blend
// would mix neighboring materials and reintroduce the seam this pass isolates.

Texture2D<float4> CurrentRrHdr : register(t0);
Texture2D<uint> CurrentSurfaceId : register(t1);
Texture2D<float4> CurrentSurfaceMeta : register(t2);
Texture2D<float2> CurrentMotion : register(t3);
Texture2D<uint> PreviousSurfaceId : register(t4);
Texture2D<float4> PreviousSurfaceMeta : register(t5);
Texture2D<float4> PreviousBoundaryHistory : register(t6);

RWTexture2D<float4> CurrentBoundaryHistory : register(u0);
RWTexture2D<uint> BoundaryMask : register(u1);

cbuffer BoundaryConstants : register(b0)
{
    uint ResetHistory;
};

static const uint INVALID_SURFACE_ID = 0xFFFFFFFFu;
static const uint BOUNDARY_MASK_EDGE = 1u;
static const uint BOUNDARY_MASK_NO_HISTORY = 2u;
static const uint BOUNDARY_MASK_GUIDE_REJECT = 4u;
static const uint BOUNDARY_MASK_SHADING_REJECT = 8u;
static const uint BOUNDARY_MASK_RESET_OR_INVALID = 16u;
static const float NORMAL_DOT_THRESHOLD = 0.95;
static const float DEPTH_RELATIVE_THRESHOLD = 0.01;
static const float DEPTH_ABSOLUTE_THRESHOLD = 0.01;
static const float MIN_HISTORY_WEIGHT = 0.25;
static const float MAX_HISTORY_COUNT = 32.0;
static const float MOVING_HISTORY_COUNT = 4.0;
static const float MOTION_CLAMP_PIXELS = 0.5;
static const float MOTION_REJECT_PIXELS = 2.0;
// The threshold is in pre-exposed HDR luminance. It rejects lighting changes
// after same-surface clamping without using exposure or ToneMap to hide them.
static const float SHADING_REJECTION_RELATIVE = 0.5;
static const float SHADING_REJECTION_ABSOLUTE = 0.05;
// A small relative expansion prevents half-precision guide quantization from
// rejecting a valid tap while keeping the clamp inside the same surface.
static const float YCOCG_CLAMP_RELATIVE_EXPANSION = 0.02;

bool FiniteValue(float value)
{
    return isfinite(value);
}

bool FiniteColor(float3 value)
{
    return all(isfinite(value));
}

float3 DecodeOctNormal(float2 encoded)
{
    float3 normal = float3(encoded * 2.0 - 1.0, 1.0 - abs(encoded.x * 2.0 - 1.0)
        - abs(encoded.y * 2.0 - 1.0));
    if (normal.z < 0.0)
        normal.xy = (1.0 - abs(normal.yx)) * sign(normal.xy);
    return normalize(normal);
}

float3 RgbToYCoCg(float3 color)
{
    return float3(
        0.25 * color.r + 0.5 * color.g + 0.25 * color.b,
        0.5 * color.r - 0.5 * color.b,
        -0.25 * color.r + 0.5 * color.g - 0.25 * color.b);
}

float3 YCoCgToRgb(float3 color)
{
    return float3(
        color.x + color.y - color.z,
        color.x + color.z,
        color.x - color.y - color.z);
}

bool ValidSurface(uint surfaceId, float4 meta)
{
    return surfaceId != INVALID_SURFACE_ID
        && all(isfinite(meta))
        && meta.z > 0.0
        && meta.w > 0.0;
}

bool SameGuide(float4 currentMeta, float4 previousMeta)
{
    if (!all(isfinite(currentMeta)) || !all(isfinite(previousMeta)))
        return false;
    float3 currentNormal = DecodeOctNormal(currentMeta.xy);
    float3 previousNormal = DecodeOctNormal(previousMeta.xy);
    if (!FiniteColor(currentNormal) || !FiniteColor(previousNormal)
        || dot(currentNormal, currentNormal) < 0.5
        || dot(previousNormal, previousNormal) < 0.5
        || dot(currentNormal, previousNormal) < NORMAL_DOT_THRESHOLD)
        return false;
    float depthTolerance = max(
        DEPTH_ABSOLUTE_THRESHOLD,
        abs(currentMeta.w) * DEPTH_RELATIVE_THRESHOLD);
    return abs(previousMeta.z - currentMeta.w) <= depthTolerance;
}

bool IsBoundary(
    uint2 pixel,
    uint2 size,
    uint centerId,
    float4 centerMeta)
{
    if (!ValidSurface(centerId, centerMeta))
    {
        // A large continuous miss region is not a geometry boundary. Only a
        // miss directly touching a valid primary surface needs a mask bit;
        // otherwise the debug view would make the background look like a
        // full-screen rejection region.
        for (int y = -1; y <= 1; ++y)
        {
            for (int x = -1; x <= 1; ++x)
            {
                int2 neighbor = int2(pixel) + int2(x, y);
                if (neighbor.x < 0 || neighbor.y < 0
                    || neighbor.x >= int(size.x) || neighbor.y >= int(size.y))
                    continue;
                if (ValidSurface(
                        CurrentSurfaceId.Load(int3(neighbor, 0)),
                        CurrentSurfaceMeta.Load(int3(neighbor, 0))))
                    return true;
            }
        }
        return false;
    }
    float3 centerNormal = DecodeOctNormal(centerMeta.xy);
    for (int y = -1; y <= 1; ++y)
    {
        for (int x = -1; x <= 1; ++x)
        {
            int2 neighbor = int2(pixel) + int2(x, y);
            if (neighbor.x < 0 || neighbor.y < 0
                || neighbor.x >= int(size.x) || neighbor.y >= int(size.y))
                return true;
            uint neighborId = CurrentSurfaceId.Load(int3(neighbor, 0));
            float4 neighborMeta = CurrentSurfaceMeta.Load(int3(neighbor, 0));
            if (neighborId != centerId)
                return true;
            if (!ValidSurface(neighborId, neighborMeta)
                || dot(centerNormal, DecodeOctNormal(neighborMeta.xy))
                    < NORMAL_DOT_THRESHOLD)
                return true;
        }
    }
    return false;
}

void WriteCurrent(
    uint2 pixel,
    float3 currentColor,
    uint mask,
    float count)
{
    CurrentBoundaryHistory[pixel] = float4(currentColor, count);
    BoundaryMask[pixel] = mask;
}

[numthreads(8, 8, 1)]
void main(uint3 dispatchId : SV_DispatchThreadID)
{
    uint2 size;
    CurrentRrHdr.GetDimensions(size.x, size.y);
    if (any(dispatchId.xy >= size))
        return;

    uint2 pixel = dispatchId.xy;
    float4 currentSample = CurrentRrHdr.Load(int3(pixel, 0));
    uint currentId = CurrentSurfaceId.Load(int3(pixel, 0));
    float4 currentMeta = CurrentSurfaceMeta.Load(int3(pixel, 0));
    float2 motion = CurrentMotion.Load(int3(pixel, 0));
    float3 currentColor = currentSample.rgb;
    if (!FiniteColor(currentColor))
    {
        currentColor = 0.0;
        WriteCurrent(pixel, currentColor, BOUNDARY_MASK_RESET_OR_INVALID, 1.0);
        return;
    }

    bool boundary = IsBoundary(pixel, size, currentId, currentMeta);
    if (!boundary)
    {
        // Exact RGB passthrough is the key invariant for interior pixels.
        WriteCurrent(pixel, currentColor, 0u, 1.0);
        return;
    }

    uint mask = BOUNDARY_MASK_EDGE;
    if (ResetHistory != 0u || currentId == INVALID_SURFACE_ID
        || !ValidSurface(currentId, currentMeta)
        || !all(isfinite(motion)))
    {
        WriteCurrent(pixel, currentColor, mask | BOUNDARY_MASK_RESET_OR_INVALID, 1.0);
        return;
    }

    float motionLength = length(motion);
    if (!FiniteValue(motionLength) || motionLength > MOTION_REJECT_PIXELS)
    {
        WriteCurrent(pixel, currentColor, mask | BOUNDARY_MASK_GUIDE_REJECT, 1.0);
        return;
    }

    float2 previousPosition = float2(pixel) + motion;
    int2 base = int2(floor(previousPosition));
    float2 fraction = previousPosition - float2(base);
    float3 historyColor = 0.0;
    float historyCount = 0.0;
    float weightSum = 0.0;
    for (int y = 0; y <= 1; ++y)
    {
        for (int x = 0; x <= 1; ++x)
        {
            int2 samplePosition = base + int2(x, y);
            if (samplePosition.x < 0 || samplePosition.y < 0
                || samplePosition.x >= int(size.x) || samplePosition.y >= int(size.y))
                continue;
            float tapWeight = (x == 0 ? 1.0 - fraction.x : fraction.x)
                * (y == 0 ? 1.0 - fraction.y : fraction.y);
            uint previousId = PreviousSurfaceId.Load(int3(samplePosition, 0));
            float4 previousMeta = PreviousSurfaceMeta.Load(int3(samplePosition, 0));
            float4 previousHistory = PreviousBoundaryHistory.Load(int3(samplePosition, 0));
            if (previousId != currentId
                || !ValidSurface(previousId, previousMeta)
                || !SameGuide(currentMeta, previousMeta)
                || !all(isfinite(previousHistory))
                || !FiniteColor(previousHistory.rgb)
                || previousHistory.a <= 0.0)
                continue;
            historyColor += previousHistory.rgb * tapWeight;
            historyCount += previousHistory.a * tapWeight;
            weightSum += tapWeight;
        }
    }
    if (!FiniteValue(weightSum) || weightSum < MIN_HISTORY_WEIGHT)
    {
        WriteCurrent(pixel, currentColor, mask | BOUNDARY_MASK_NO_HISTORY, 1.0);
        return;
    }
    historyColor /= weightSum;
    historyCount /= weightSum;

    float3 yCoCgMin = float3(1.0e30, 1.0e30, 1.0e30);
    float3 yCoCgMax = float3(-1.0e30, -1.0e30, -1.0e30);
    bool hasSameSurfaceColor = false;
    for (int y = -1; y <= 1; ++y)
    {
        for (int x = -1; x <= 1; ++x)
        {
            int2 neighbor = int2(pixel) + int2(x, y);
            if (neighbor.x < 0 || neighbor.y < 0
                || neighbor.x >= int(size.x) || neighbor.y >= int(size.y))
                continue;
            uint neighborId = CurrentSurfaceId.Load(int3(neighbor, 0));
            float3 neighborColor = CurrentRrHdr.Load(int3(neighbor, 0)).rgb;
            if (neighborId != currentId || !FiniteColor(neighborColor))
                continue;
            float3 yCoCg = RgbToYCoCg(neighborColor);
            yCoCgMin = min(yCoCgMin, yCoCg);
            yCoCgMax = max(yCoCgMax, yCoCg);
            hasSameSurfaceColor = true;
        }
    }
    if (!hasSameSurfaceColor)
    {
        WriteCurrent(pixel, currentColor, mask | BOUNDARY_MASK_SHADING_REJECT, 1.0);
        return;
    }
    float3 margin = max(abs(yCoCgMin), abs(yCoCgMax))
        * YCOCG_CLAMP_RELATIVE_EXPANSION + 0.001;
    float3 clampedYCoCg = clamp(
        RgbToYCoCg(historyColor),
        yCoCgMin - margin,
        yCoCgMax + margin);
    float3 clampedHistory = YCoCgToRgb(clampedYCoCg);
    float currentLuma = RgbToYCoCg(currentColor).x;
    float historyLuma = clampedYCoCg.x;
    float shadingTolerance = max(
        SHADING_REJECTION_ABSOLUTE,
        max(abs(currentLuma), abs(historyLuma)) * SHADING_REJECTION_RELATIVE);
    if (!FiniteColor(clampedHistory)
        || abs(historyLuma - currentLuma) > shadingTolerance)
    {
        WriteCurrent(pixel, currentColor, mask | BOUNDARY_MASK_SHADING_REJECT, 1.0);
        return;
    }

    float maxCount = motionLength > MOTION_CLAMP_PIXELS
        ? MOVING_HISTORY_COUNT
        : MAX_HISTORY_COUNT;
    historyCount = clamp(historyCount, 1.0, maxCount);
    float historyWeight = historyCount / (historyCount + 1.0);
    float3 resolved = lerp(currentColor, clampedHistory, historyWeight);
    if (!FiniteColor(resolved))
    {
        WriteCurrent(pixel, currentColor, mask | BOUNDARY_MASK_SHADING_REJECT, 1.0);
        return;
    }
    WriteCurrent(pixel, resolved, mask, min(maxCount, historyCount + 1.0));
}
