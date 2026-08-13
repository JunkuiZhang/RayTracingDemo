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

struct Material
{
    float4 baseColorFactor;
    float3 emissiveFactor;
    float metallicFactor;
    float roughnessFactor;
    float normalScale;
    float ior;
    uint flags;
    uint baseColorTextureAndSampler;
    uint metallicRoughnessTextureAndSampler;
    uint normalTextureAndSampler;
    uint emissiveTextureAndSampler;
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
StructuredBuffer<Material> Materials : register(t4);

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
// Scene stable IDs reserve the high bit for this pass. Marking a reflected
// hit keeps the physical floor distinct from that floor seen through a mirror
// while the remaining bits retain the reflected surface identity.
static const uint VIRTUAL_SURFACE_BIT = 0x80000000u;
static const uint MATERIAL_FLAG_LEGACY_METAL = 8u;
static const float INVALID_VIEW_Z = 1001.0;
static const float PURE_MIRROR_ROUGHNESS = 0.08;

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

float4 MakeMirrorPlane(float3 normal, float3 planePoint)
{
    float3 unitNormal = normalize(normal);
    return float4(unitNormal, dot(unitNormal, planePoint));
}

float3 ReflectPointAcrossPlane(float4 plane, float3 position)
{
    return position - 2.0
        * (dot(plane.xyz, position) - plane.w)
        * plane.xyz;
}

float3 ReflectVectorAcrossPlane(float4 plane, float3 direction)
{
    return direction - 2.0 * dot(plane.xyz, direction) * plane.xyz;
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
    float3x4 currentObjectToWorld = query.CommittedObjectToWorld3x4();
    float3 currentWorldPosition = mul(currentObjectToWorld, float4(localPosition, 1.0));
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

    Material primaryMaterial = Materials[instanceData.materialIndex];
    bool pureLegacyMirror =
        (primaryMaterial.flags & MATERIAL_FLAG_LEGACY_METAL) != 0u
        && primaryMaterial.roughnessFactor <= PURE_MIRROR_ROUGHNESS;
    // Primary Surface Replacement needs a previous mirror plane to reproject
    // correctly. InstanceGpu intentionally carries no previous inverse normal
    // transform, so limit this boundary guide to mirrors whose complete rigid
    // transform is static. Animated mirrors retain the conservative physical-
    // surface guide instead of receiving plausible but wrong virtual motion.
    bool staticMirror =
        all(abs(currentObjectToWorld[0]
            - instanceData.previousObjectToWorldRow0) <= 1.0e-5)
        && all(abs(currentObjectToWorld[1]
            - instanceData.previousObjectToWorldRow1) <= 1.0e-5)
        && all(abs(currentObjectToWorld[2]
            - instanceData.previousObjectToWorldRow2) <= 1.0e-5);
    if (!pureLegacyMirror || !staticMirror)
        return;

    float3 reflectedDirection = normalize(reflect(ray.Direction, worldNormal));
    if (!FiniteNormal(reflectedDirection)
        || dot(worldNormal, reflectedDirection) <= 0.0)
        return;

    RayDesc reflectedRay;
    // Match the radiance and RR specular-motion guide's self-intersection
    // convention so all three paths classify the same dominant reflection.
    reflectedRay.Origin = currentWorldPosition + worldNormal * 0.002;
    reflectedRay.Direction = reflectedDirection;
    reflectedRay.TMin = 0.001;
    reflectedRay.TMax = 1000.0;

    RayQuery<RAY_FLAG_CULL_BACK_FACING_TRIANGLES | RAY_FLAG_FORCE_OPAQUE>
        reflectedQuery;
    reflectedQuery.TraceRayInline(Scene, RAY_FLAG_NONE, 0xFF, reflectedRay);
    while (reflectedQuery.Proceed())
    {
    }
    if (reflectedQuery.CommittedStatus() != COMMITTED_TRIANGLE_HIT)
        return;

    uint reflectedInstanceIndex = reflectedQuery.CommittedInstanceID();
    InstanceGpu reflectedInstance = Instances[reflectedInstanceIndex];
    uint reflectedPrimitive = reflectedQuery.CommittedPrimitiveIndex();
    uint3 reflectedTriangleIndices = uint3(
        Indices[reflectedInstance.indexOffset + reflectedPrimitive * 3u],
        Indices[reflectedInstance.indexOffset + reflectedPrimitive * 3u + 1u],
        Indices[reflectedInstance.indexOffset + reflectedPrimitive * 3u + 2u]);
    float2 reflectedCommittedBarycentrics =
        reflectedQuery.CommittedTriangleBarycentrics();
    float3 reflectedBarycentrics = float3(
        1.0 - reflectedCommittedBarycentrics.x
            - reflectedCommittedBarycentrics.y,
        reflectedCommittedBarycentrics.x,
        reflectedCommittedBarycentrics.y);
    Vertex reflectedVertex0 =
        Vertices[reflectedInstance.vertexOffset + reflectedTriangleIndices.x];
    Vertex reflectedVertex1 =
        Vertices[reflectedInstance.vertexOffset + reflectedTriangleIndices.y];
    Vertex reflectedVertex2 =
        Vertices[reflectedInstance.vertexOffset + reflectedTriangleIndices.z];
    float3 reflectedLocalPosition =
        reflectedVertex0.position * reflectedBarycentrics.x
        + reflectedVertex1.position * reflectedBarycentrics.y
        + reflectedVertex2.position * reflectedBarycentrics.z;
    float3 reflectedLocalNormal =
        reflectedVertex0.normal * reflectedBarycentrics.x
        + reflectedVertex1.normal * reflectedBarycentrics.y
        + reflectedVertex2.normal * reflectedBarycentrics.z;
    float3 reflectedWorldPosition = mul(
        reflectedQuery.CommittedObjectToWorld3x4(),
        float4(reflectedLocalPosition, 1.0));
    float3 reflectedPreviousWorldPosition = PreviousWorldPosition(
        reflectedLocalPosition,
        reflectedInstance);
    float3 reflectedWorldNormal = normalize(mul(
        reflectedLocalNormal,
        (float3x3)reflectedQuery.CommittedWorldToObject3x4()));
    if (!FiniteNormal(reflectedWorldNormal))
        return;
    if (dot(reflectedWorldNormal, reflectedRay.Direction) > 0.0)
        reflectedWorldNormal = -reflectedWorldNormal;

    float4 currentMirrorPlane = MakeMirrorPlane(
        worldNormal,
        currentWorldPosition);
    float4 previousMirrorPlane = MakeMirrorPlane(
        worldNormal,
        previousWorldPosition);
    float3 virtualWorldPosition = ReflectPointAcrossPlane(
        currentMirrorPlane,
        reflectedWorldPosition);
    float3 virtualPreviousWorldPosition = ReflectPointAcrossPlane(
        previousMirrorPlane,
        reflectedPreviousWorldPosition);
    float3 virtualWorldNormal = normalize(ReflectVectorAcrossPlane(
        currentMirrorPlane,
        reflectedWorldNormal));
    if (!FiniteNormal(virtualWorldNormal))
        return;
    if (dot(virtualWorldNormal, CameraPosition - virtualWorldPosition) < 0.0)
        virtualWorldNormal = -virtualWorldNormal;

    float virtualCurrentViewZ = Stage11ViewZ(
        virtualWorldPosition,
        CameraPosition,
        CameraYaw,
        CameraPitch);
    float virtualPreviousViewZ = Stage11ViewZ(
        virtualPreviousWorldPosition,
        PreviousCameraPosition,
        PreviousCameraYaw,
        PreviousCameraPitch);
    float2 virtualCurrentUv = Stage11ProjectToUv(
        virtualWorldPosition,
        CameraPosition,
        CameraYaw,
        CameraPitch,
        size);
    float2 virtualPreviousUv = Stage11ProjectToUv(
        virtualPreviousWorldPosition,
        PreviousCameraPosition,
        PreviousCameraYaw,
        PreviousCameraPitch,
        size);
    if (!all(isfinite(virtualCurrentUv))
        || !all(isfinite(virtualPreviousUv))
        || !isfinite(virtualCurrentViewZ)
        || !isfinite(virtualPreviousViewZ)
        || virtualCurrentViewZ <= 0.0
        || virtualPreviousViewZ <= 0.0)
        return;

    // The post-RR stabilizer now sees reflected geometry boundaries inside a
    // pure mirror, but still keeps the mirror's physical outline separate via
    // VIRTUAL_SURFACE_BIT. This guide never changes the RR SDK inputs.
    PrimarySurfaceId[pixel] = VIRTUAL_SURFACE_BIT
        | reflectedInstance.stableSurfaceId;
    PrimarySurfaceMeta[pixel] = float4(
        Stage11OctEncode(virtualWorldNormal),
        virtualCurrentViewZ,
        virtualPreviousViewZ);
    PrimaryMotion[pixel] = ResetHistory != 0u
        ? 0.0
        : (virtualPreviousUv - virtualCurrentUv) * float2(size);
}
