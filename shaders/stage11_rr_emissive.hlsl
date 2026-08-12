// Directly visible emission has deterministic binary coverage and is therefore
// a poor fit for RR's stochastic radiance reconstruction. Resolve it on an
// unjittered output grid and keep a bounded, motion-reprojected history. The
// RGB channels hold resolved emission; alpha stores the history sample count.
Texture2D<float4> PrimaryEmissive : register(t0);
Texture2D<float2> Motion : register(t1);
Texture2D<float4> PreviousHistory : register(t2);
RWTexture2D<float4> CurrentHistory : register(u0);

cbuffer EmissiveConstants : register(b0)
{
    float2 CameraJitterPx;
    uint ResetHistory;
};

static const float MaxHistorySamples = 64.0;

bool Finite3(float3 value)
{
    return all(isfinite(value));
}

void CurrentBilinearCoordinates(
    uint2 outputPixel,
    uint2 outputSize,
    uint2 renderSize,
    out int2 p00,
    out int2 p10,
    out int2 p01,
    out int2 p11,
    out float2 fraction)
{
    // Path-traced samples live at pixelCenter + CameraJitterPx. Subtracting
    // that offset maps them back to the stable output-pixel lattice.
    float2 source = (float2(outputPixel) + 0.5)
        * float2(renderSize) / float2(outputSize)
        - 0.5 - CameraJitterPx;
    int2 base = int2(floor(source));
    fraction = frac(source);
    int2 maximum = int2(renderSize) - 1;
    p00 = clamp(base, int2(0, 0), maximum);
    p10 = clamp(base + int2(1, 0), int2(0, 0), maximum);
    p01 = clamp(base + int2(0, 1), int2(0, 0), maximum);
    p11 = clamp(base + int2(1, 1), int2(0, 0), maximum);
}

float3 ResolveCurrentEmission(
    uint2 outputPixel,
    uint2 outputSize,
    uint2 renderSize,
    out float3 neighborhoodMinimum,
    out float3 neighborhoodMaximum,
    out float2 motion)
{
    int2 p00, p10, p01, p11;
    float2 fraction;
    CurrentBilinearCoordinates(
        outputPixel,
        outputSize,
        renderSize,
        p00,
        p10,
        p01,
        p11,
        fraction);

    float3 e00 = max(PrimaryEmissive.Load(int3(p00, 0)).xyz, 0.0);
    float3 e10 = max(PrimaryEmissive.Load(int3(p10, 0)).xyz, 0.0);
    float3 e01 = max(PrimaryEmissive.Load(int3(p01, 0)).xyz, 0.0);
    float3 e11 = max(PrimaryEmissive.Load(int3(p11, 0)).xyz, 0.0);
    e00 = Finite3(e00) ? e00 : 0.0;
    e10 = Finite3(e10) ? e10 : 0.0;
    e01 = Finite3(e01) ? e01 : 0.0;
    e11 = Finite3(e11) ? e11 : 0.0;

    neighborhoodMinimum = min(min(e00, e10), min(e01, e11));
    neighborhoodMaximum = max(max(e00, e10), max(e01, e11));
    float3 row0 = lerp(e00, e10, fraction.x);
    float3 row1 = lerp(e01, e11, fraction.x);

    float2 m00 = Motion.Load(int3(p00, 0));
    float2 m10 = Motion.Load(int3(p10, 0));
    float2 m01 = Motion.Load(int3(p01, 0));
    float2 m11 = Motion.Load(int3(p11, 0));
    float2 motionRow0 = lerp(m00, m10, fraction.x);
    float2 motionRow1 = lerp(m01, m11, fraction.x);
    motion = lerp(motionRow0, motionRow1, fraction.y);
    motion = all(isfinite(motion)) ? motion : 0.0;
    return lerp(row0, row1, fraction.y);
}

float4 LoadPreviousBilinear(float2 position, uint2 size)
{
    int2 base = int2(floor(position));
    float2 fraction = frac(position);
    int2 maximum = int2(size) - 1;
    int2 p00 = clamp(base, int2(0, 0), maximum);
    int2 p10 = clamp(base + int2(1, 0), int2(0, 0), maximum);
    int2 p01 = clamp(base + int2(0, 1), int2(0, 0), maximum);
    int2 p11 = clamp(base + int2(1, 1), int2(0, 0), maximum);
    float4 row0 = lerp(
        PreviousHistory.Load(int3(p00, 0)),
        PreviousHistory.Load(int3(p10, 0)),
        fraction.x);
    float4 row1 = lerp(
        PreviousHistory.Load(int3(p01, 0)),
        PreviousHistory.Load(int3(p11, 0)),
        fraction.x);
    return lerp(row0, row1, fraction.y);
}

[numthreads(8, 8, 1)]
void main(uint3 dispatchId : SV_DispatchThreadID)
{
    uint2 outputSize;
    CurrentHistory.GetDimensions(outputSize.x, outputSize.y);
    if (any(dispatchId.xy >= outputSize))
    {
        return;
    }

    uint2 renderSize;
    PrimaryEmissive.GetDimensions(renderSize.x, renderSize.y);
    float3 neighborhoodMinimum;
    float3 neighborhoodMaximum;
    float2 motion;
    float3 current = ResolveCurrentEmission(
        dispatchId.xy,
        outputSize,
        renderSize,
        neighborhoodMinimum,
        neighborhoodMaximum,
        motion);

    // DLSS motion is previous-current in render pixels. History is stored at
    // output resolution, so scale the vector before reprojecting it.
    float2 outputMotion = motion * float2(outputSize) / float2(renderSize);
    float2 previousPosition = float2(dispatchId.xy) + outputMotion;
    bool previousInBounds = all(previousPosition >= 0.0)
        && all(previousPosition <= float2(outputSize - 1u));
    float4 previous = (ResetHistory == 0u && previousInBounds)
        ? LoadPreviousBilinear(previousPosition, outputSize)
        : 0.0;

    bool previousValid = Finite3(previous.xyz)
        && isfinite(previous.w) && previous.w > 0.0;
    float historyCount = previousValid ? min(previous.w, MaxHistorySamples) : 0.0;
    float motionMagnitude = length(outputMotion);
    if (motionMagnitude > 2.0)
    {
        historyCount = 0.0;
    }
    else if (motionMagnitude > 0.5)
    {
        // Moving edges retain only a short history to avoid a persistent
        // emissive trail while still receiving basic temporal antialiasing.
        historyCount = min(historyCount, 4.0);
    }

    bool stationaryProjection = motionMagnitude <= 0.01;
    // A changing jitter sample is not a disocclusion. Clamping a stationary
    // silhouette to the current binary footprint would erase the accumulated
    // subpixel coverage and reintroduce the exact edge flicker this pass owns.
    // Moving projections still need the clamp to prevent emissive trails.
    float3 acceptedPrevious = previousValid ? previous.xyz : current;
    if (!stationaryProjection)
    {
        acceptedPrevious = clamp(
            acceptedPrevious,
            neighborhoodMinimum,
            neighborhoodMaximum);
    }
    // Once a static edge has enough samples, copy the previous solution
    // exactly. This also synchronizes both ping-pong histories and makes a
    // truly static lamp bit-stable instead of retaining a perpetual 1/64 EMA.
    bool converged = stationaryProjection && historyCount >= MaxHistorySamples;
    float historyWeight = converged ? 1.0 : historyCount / (historyCount + 1.0);
    float3 resolved = lerp(current, acceptedPrevious, historyWeight);
    CurrentHistory[dispatchId.xy] = float4(
        max(Finite3(resolved) ? resolved : current, 0.0),
        min(historyCount + 1.0, MaxHistorySamples));
}
