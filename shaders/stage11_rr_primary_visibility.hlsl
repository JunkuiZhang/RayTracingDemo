// Stable RR-only primary visibility. This pass deliberately does not trace
// radiance: its output-resolution, pixel-center ray owns surface identity and
// reprojection while the jittered path tracer remains responsible for RR
// shading quality. In particular, neither frame index nor projection jitter
// participates in this ray construction.
#include "stage11_camera.hlsli"

RaytracingAccelerationStructure Scene : register(t0);

struct Vertex
{
    float3 position;
    float3 normal;
    float4 tangent;
    float2 texcoord0;
};

struct InstanceGpu
{
    float4 previousObjectToWorldRow0;
    float4 previousObjectToWorldRow1;
    float4 previousObjectToWorldRow2;
    uint vertexOffset;
    uint indexOffset;
    uint materialIndex;
    uint stableSurfaceId;
};

StructuredBuffer<Vertex> Vertices : register(t1);
StructuredBuffer<uint> Indices : register(t2);
StructuredBuffer<InstanceGpu> Instances : register(t3);

RWTexture2D<uint> PrimarySurfaceId : register(u0);
RWTexture2D<float4> PrimarySurfaceMeta : register(u1);
RWTexture2D<float2> PrimaryMotion : register(u2);

cbuffer CameraConstants : register(b0)
{
    uint FrameIndex;
    float3 CameraPosition;
    float CameraYaw;
    float CameraPitch;
    // The jittered path uses these words, but this pass intentionally does not.
    float2 JitterPadding;
    float3 PreviousCameraPosition;
    float PreviousCameraYaw;
    float PreviousCameraPitch;
    uint DlssGuideMode;
    uint NrdEnabled;
    uint ResetHistory;
};

static const uint INVALID_SURFACE_ID = 0xFFFFFFFFu;
static const float INVALID_VIEW_Z = 1001.0;

float3 PreviousWorldPosition(float3 localPosition, InstanceGpu instanceData)
{
    float3x4 previousObjectToWorld = float3x4(
        instanceData.previousObjectToWorldRow0,
        instanceData.previousObjectToWorldRow1,
        instanceData.previousObjectToWorldRow2);
    return mul(previousObjectToWorld, float4(localPosition, 1.0));
}

bool FiniteNormal(float3 normal)
{
    return all(isfinite(normal)) && dot(normal, normal) > 1.0e-8;
}

[numthreads(8, 8, 1)]
void main(uint3 dispatchId : SV_DispatchThreadID)
{
    uint2 size;
    PrimarySurfaceId.GetDimensions(size.x, size.y);
    if (any(dispatchId.xy >= size))
        return;

    uint2 pixel = dispatchId.xy;
    PrimarySurfaceId[pixel] = INVALID_SURFACE_ID;
    PrimarySurfaceMeta[pixel] = float4(0.5, 0.5, INVALID_VIEW_Z, INVALID_VIEW_Z);
    PrimaryMotion[pixel] = 0.0;

    RayDesc ray;
    ray.Origin = CameraPosition;
    ray.Direction = Stage11PrimaryRayDirection(
        pixel,
        size,
        CameraPosition,
        CameraYaw,
        CameraPitch);
    ray.TMin = 0.001;
    ray.TMax = 1000.0;

    RayQuery<RAY_FLAG_CULL_BACK_FACING_TRIANGLES | RAY_FLAG_FORCE_OPAQUE> query;
    query.TraceRayInline(Scene, RAY_FLAG_NONE, 0xFF, ray);
    while (query.Proceed())
    {
    }
    if (query.CommittedStatus() != COMMITTED_TRIANGLE_HIT)
        return;

    uint instanceIndex = query.CommittedInstanceID();
    InstanceGpu instanceData = Instances[instanceIndex];
    uint primitive = query.CommittedPrimitiveIndex();
    uint3 triangleIndices = uint3(
        Indices[instanceData.indexOffset + primitive * 3u],
        Indices[instanceData.indexOffset + primitive * 3u + 1u],
        Indices[instanceData.indexOffset + primitive * 3u + 2u]);
    float2 committedBarycentrics = query.CommittedTriangleBarycentrics();
    float3 barycentrics = float3(
        1.0 - committedBarycentrics.x - committedBarycentrics.y,
        committedBarycentrics.x,
        committedBarycentrics.y);
    Vertex vertex0 = Vertices[instanceData.vertexOffset + triangleIndices.x];
    Vertex vertex1 = Vertices[instanceData.vertexOffset + triangleIndices.y];
    Vertex vertex2 = Vertices[instanceData.vertexOffset + triangleIndices.z];
    float3 localPosition = vertex0.position * barycentrics.x
        + vertex1.position * barycentrics.y
        + vertex2.position * barycentrics.z;
    float3 localNormal = vertex0.normal * barycentrics.x
        + vertex1.normal * barycentrics.y
        + vertex2.normal * barycentrics.z;
    float3 currentWorldPosition = mul(
        query.CommittedObjectToWorld3x4(),
        float4(localPosition, 1.0));
    float3 previousWorldPosition = PreviousWorldPosition(localPosition, instanceData);
    float3 worldNormal = normalize(mul(
        localNormal,
        (float3x3)query.CommittedWorldToObject3x4()));
    if (!FiniteNormal(worldNormal))
        return;
    // The query culls back-facing triangles. This explicit orientation keeps
    // the stored geometric normal consistent if a driver reports a reversed
    // winding at the committed hit and avoids a false same-surface rejection.
    if (dot(worldNormal, ray.Direction) > 0.0)
        worldNormal = -worldNormal;

    float currentViewZ = Stage11ViewZ(
        currentWorldPosition,
        CameraPosition,
        CameraYaw,
        CameraPitch);
    float previousViewZ = Stage11ViewZ(
        previousWorldPosition,
        PreviousCameraPosition,
        PreviousCameraYaw,
        PreviousCameraPitch);
    float2 currentUv = Stage11ProjectToUv(
        currentWorldPosition,
        CameraPosition,
        CameraYaw,
        CameraPitch,
        size);
    float2 previousUv = Stage11ProjectToUv(
        previousWorldPosition,
        PreviousCameraPosition,
        PreviousCameraYaw,
        PreviousCameraPitch,
        size);
    if (!all(isfinite(currentUv)) || !all(isfinite(previousUv))
        || !isfinite(currentViewZ) || !isfinite(previousViewZ)
        || currentViewZ <= 0.0 || previousViewZ <= 0.0)
        return;

    PrimarySurfaceId[pixel] = instanceData.stableSurfaceId;
    PrimarySurfaceMeta[pixel] = float4(
        Stage11OctEncode(normalize(worldNormal)),
        currentViewZ,
        previousViewZ);
    PrimaryMotion[pixel] = ResetHistory != 0u
        ? 0.0
        : (previousUv - currentUv) * float2(size);
}
