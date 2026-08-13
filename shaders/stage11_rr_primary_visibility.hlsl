// Stable RR-only primary visibility. This pass deliberately does not trace
// radiance: its output-resolution, pixel-center ray owns surface identity and
// reprojection while the jittered path tracer remains responsible for RR
// shading quality. In particular, neither frame index nor projection jitter
// participates in this ray construction.
#include "stage11_camera.hlsli"
#include "stage11_material.hlsli"
#include "stage11_path_space.hlsli"

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
// Scene stable IDs reserve the high bit for this pass. Marking a virtual hit
// keeps a physical surface distinct from the same surface seen through a
// mirror or glass while the remaining bits retain the hit surface identity.
static const uint VIRTUAL_SURFACE_BIT = 0x80000000u;
static const uint MATERIAL_FLAG_LEGACY_DIELECTRIC = 4u;
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

bool MakeTransmissionVirtualSurface(
    uint instanceIndex,
    uint primitive,
    float2 committedBarycentrics,
    float3x4 objectToWorld,
    float3x4 worldToObject,
    float3 rayDirection,
    out uint virtualSurfaceId,
    out float4 virtualSurfaceMeta)
{
    InstanceGpu instanceData = Instances[instanceIndex];
    uint3 triangleIndices = uint3(
        Indices[instanceData.indexOffset + primitive * 3u],
        Indices[instanceData.indexOffset + primitive * 3u + 1u],
        Indices[instanceData.indexOffset + primitive * 3u + 2u]);
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
    float3 worldPosition = mul(objectToWorld, float4(localPosition, 1.0));
    float3 previousWorldPosition = PreviousWorldPosition(
        localPosition,
        instanceData);
    float3 worldNormal = normalize(mul(
        localNormal,
        (float3x3)worldToObject));
    if (!FiniteNormal(worldNormal))
        return false;
    if (dot(worldNormal, rayDirection) > 0.0)
        worldNormal = -worldNormal;

    float currentViewZ = Stage11ViewZ(
        worldPosition,
        CameraPosition,
        CameraYaw,
        CameraPitch);
    float previousViewZ = Stage11ViewZ(
        previousWorldPosition,
        PreviousCameraPosition,
        PreviousCameraYaw,
        PreviousCameraPitch);
    if (!isfinite(currentViewZ) || !isfinite(previousViewZ)
        || currentViewZ <= 0.0 || previousViewZ <= 0.0)
        return false;

    virtualSurfaceId = VIRTUAL_SURFACE_BIT | instanceData.stableSurfaceId;
    virtualSurfaceMeta = float4(
        Stage11OctEncode(worldNormal),
        currentViewZ,
        previousViewZ);
    return true;
}

bool TraceStaticGlassVirtualSurface(
    RayDesc primaryRay,
    float3 entryPosition,
    float3 entryNormal,
    float ior,
    out uint virtualSurfaceId,
    out float4 virtualSurfaceMeta)
{
    virtualSurfaceId = INVALID_SURFACE_ID;
    virtualSurfaceMeta = float4(0.5, 0.5, INVALID_VIEW_Z, INVALID_VIEW_Z);

    // Trace the principal transmitted path through one closed dielectric.
    // Fresnel reflection stays in RR's main signal; this guide identifies the
    // dominant image seen through the glass and never contributes radiance.
    float3 insideDirection = refract(
        primaryRay.Direction,
        entryNormal,
        1.0 / max(ior, 1.0001));
    if (!FiniteNormal(insideDirection))
        return false;
    insideDirection = normalize(insideDirection);

    RayDesc exitRay;
    exitRay.Origin = entryPosition + insideDirection * 0.002;
    exitRay.Direction = insideDirection;
    exitRay.TMin = 0.001;
    exitRay.TMax = 1000.0;
    // The first inside hit is either the dielectric exit or an opaque shared
    // contact interface (the Cornell floor intersects the hidden box bottom).
    // Accepting both avoids fabricating a second medium boundary behind the
    // actually visible opaque contact surface.
    RayQuery<RAY_FLAG_FORCE_OPAQUE> exitQuery;
    exitQuery.TraceRayInline(Scene, RAY_FLAG_NONE, 0xFF, exitRay);
    while (exitQuery.Proceed())
    {
    }
    if (exitQuery.CommittedStatus() != COMMITTED_TRIANGLE_HIT)
        return false;

    uint exitInstanceIndex = exitQuery.CommittedInstanceID();
    InstanceGpu exitInstance = Instances[exitInstanceIndex];
    Material exitMaterial = Materials[exitInstance.materialIndex];
    uint exitPrimitive = exitQuery.CommittedPrimitiveIndex();
    float2 exitCommittedBarycentrics = exitQuery.CommittedTriangleBarycentrics();
    if ((exitMaterial.flags & MATERIAL_FLAG_LEGACY_DIELECTRIC) == 0u)
    {
        return MakeTransmissionVirtualSurface(
            exitInstanceIndex,
            exitPrimitive,
            exitCommittedBarycentrics,
            exitQuery.CommittedObjectToWorld3x4(),
            exitQuery.CommittedWorldToObject3x4(),
            insideDirection,
            virtualSurfaceId,
            virtualSurfaceMeta);
    }
    uint3 exitIndices = uint3(
        Indices[exitInstance.indexOffset + exitPrimitive * 3u],
        Indices[exitInstance.indexOffset + exitPrimitive * 3u + 1u],
        Indices[exitInstance.indexOffset + exitPrimitive * 3u + 2u]);
    float3 exitBarycentrics = float3(
        1.0 - exitCommittedBarycentrics.x - exitCommittedBarycentrics.y,
        exitCommittedBarycentrics.x,
        exitCommittedBarycentrics.y);
    Vertex exitVertex0 = Vertices[exitInstance.vertexOffset + exitIndices.x];
    Vertex exitVertex1 = Vertices[exitInstance.vertexOffset + exitIndices.y];
    Vertex exitVertex2 = Vertices[exitInstance.vertexOffset + exitIndices.z];
    float3 exitLocalPosition = exitVertex0.position * exitBarycentrics.x
        + exitVertex1.position * exitBarycentrics.y
        + exitVertex2.position * exitBarycentrics.z;
    float3 exitLocalNormal = exitVertex0.normal * exitBarycentrics.x
        + exitVertex1.normal * exitBarycentrics.y
        + exitVertex2.normal * exitBarycentrics.z;
    float3 exitPosition = mul(
        exitQuery.CommittedObjectToWorld3x4(),
        float4(exitLocalPosition, 1.0));
    float3 exitNormal = normalize(mul(
        exitLocalNormal,
        (float3x3)exitQuery.CommittedWorldToObject3x4()));
    if (!FiniteNormal(exitNormal))
        return false;
    // HLSL refract expects N against the incident direction. At a volume exit
    // the outward geometric normal points with the ray and must be flipped.
    if (dot(exitNormal, insideDirection) > 0.0)
        exitNormal = -exitNormal;
    float3 outsideDirection = refract(
        insideDirection,
        exitNormal,
        max(exitMaterial.ior, 1.0001));
    if (!FiniteNormal(outsideDirection))
        return false;
    outsideDirection = normalize(outsideDirection);

    RayDesc transmittedRay;
    transmittedRay.Origin = exitPosition + outsideDirection * 0.002;
    transmittedRay.Direction = outsideDirection;
    transmittedRay.TMin = 0.001;
    transmittedRay.TMax = 1000.0;
    RayQuery<RAY_FLAG_CULL_BACK_FACING_TRIANGLES | RAY_FLAG_FORCE_OPAQUE>
        transmittedQuery;
    transmittedQuery.TraceRayInline(Scene, RAY_FLAG_NONE, 0xFF, transmittedRay);
    while (transmittedQuery.Proceed())
    {
    }
    if (transmittedQuery.CommittedStatus() != COMMITTED_TRIANGLE_HIT)
        return false;

    uint transmittedInstanceIndex = transmittedQuery.CommittedInstanceID();
    InstanceGpu transmittedInstance = Instances[transmittedInstanceIndex];
    Material transmittedMaterial = Materials[transmittedInstance.materialIndex];
    // One entry/exit pair is the supported real-time contract. Nested glass
    // needs another explicit layer and must conservatively keep the physical ID.
    if ((transmittedMaterial.flags & MATERIAL_FLAG_LEGACY_DIELECTRIC) != 0u)
        return false;
    return MakeTransmissionVirtualSurface(
        transmittedInstanceIndex,
        transmittedQuery.CommittedPrimitiveIndex(),
        transmittedQuery.CommittedTriangleBarycentrics(),
        transmittedQuery.CommittedObjectToWorld3x4(),
        transmittedQuery.CommittedWorldToObject3x4(),
        outsideDirection,
        virtualSurfaceId,
        virtualSurfaceMeta);
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
    bool legacyGlass =
        (primaryMaterial.flags & MATERIAL_FLAG_LEGACY_DIELECTRIC) != 0u;
    bool pureLegacyMirror =
        (primaryMaterial.flags & MATERIAL_FLAG_LEGACY_METAL) != 0u
        && primaryMaterial.roughnessFactor <= PURE_MIRROR_ROUGHNESS;
    // Virtual reflection/refraction guides need previous interface geometry.
    // InstanceGpu intentionally carries no previous inverse normal transform,
    // so limit them to interfaces whose complete rigid transform is static.
    bool staticPrimarySurface =
        all(abs(currentObjectToWorld[0]
            - instanceData.previousObjectToWorldRow0) <= 1.0e-5)
        && all(abs(currentObjectToWorld[1]
            - instanceData.previousObjectToWorldRow1) <= 1.0e-5)
        && all(abs(currentObjectToWorld[2]
            - instanceData.previousObjectToWorldRow2) <= 1.0e-5);
    bool staticCamera =
        all(abs(CameraPosition - PreviousCameraPosition) <= 1.0e-5)
        && abs(CameraYaw - PreviousCameraYaw) <= 1.0e-5
        && abs(CameraPitch - PreviousCameraPitch) <= 1.0e-5;

    if (legacyGlass && staticPrimarySurface && staticCamera)
    {
        uint transmittedSurfaceId;
        float4 transmittedSurfaceMeta;
        if (TraceStaticGlassVirtualSurface(
                ray,
                currentWorldPosition,
                worldNormal,
                primaryMaterial.ior,
                transmittedSurfaceId,
                transmittedSurfaceMeta))
        {
            // A static refractive image has zero output-pixel motion. The
            // transmitted hit's ID/normal/depth let the existing bounded RR
            // history reject real boundaries without blurring the whole frame.
            PrimarySurfaceId[pixel] = transmittedSurfaceId;
            PrimarySurfaceMeta[pixel] = transmittedSurfaceMeta;
            PrimaryMotion[pixel] = 0.0;
        }
        return;
    }

    if (!pureLegacyMirror || !staticPrimarySurface)
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
