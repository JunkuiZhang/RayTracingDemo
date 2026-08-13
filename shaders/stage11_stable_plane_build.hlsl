#include "stage11_camera.hlsli"
#include "stage11_material.hlsli"
#include "stage11_path_space.hlsli"
#include "stage11_scene.hlsli"

RaytracingAccelerationStructure Scene : register(t0);
StructuredBuffer<Vertex> Vertices : register(t1);
StructuredBuffer<uint> Indices : register(t2);
StructuredBuffer<Material> Materials : register(t3);
StructuredBuffer<InstanceGpu> Instances : register(t4);

RWStructuredBuffer<StablePlaneRecord> StablePlaneRecords : register(u0);
RWTexture2DArray<uint> StablePlaneHeaders : register(u1);
RWTexture2DArray<float4> StablePlaneNoisyDiffuse : register(u2);
RWTexture2DArray<float4> StablePlaneNoisySpecular : register(u3);
RWTexture2D<float4> StableRadiance : register(u4);

cbuffer FrameConstants : register(b0)
{
    uint FrameIndex;
    float3 CameraPosition;
    float CameraYaw;
    float CameraPitch;
    float2 CameraJitterPx;
    float3 PreviousCameraPosition;
    float PreviousCameraYaw;
    float PreviousCameraPitch;
    uint DlssGuideMode;
    uint NrdEnabled;
    uint ResetHistory;
};

static const uint MAX_BUILD_QUEUE = 6u;
static const uint MAX_BUILD_STEPS = 16u;
static const uint MAX_FALSE_INTERSECTIONS = 8u;

struct BuildBranchState
{
    float3 origin;
    float3 direction;
    float3 throughput;
    float sceneLength;
    uint branchId;
    uint depth;
    uint2 interiorSlots;
};

float3 StablePrimaryRayDirection(uint2 pixel, uint2 extent)
{
    float2 sampleOffset = StablePrimarySampleOffset(pixel, FrameIndex, DlssGuideMode);
    float2 uv = (float2(pixel) + sampleOffset + CameraJitterPx) / float2(extent);
    float2 screen = uv * 2.0 - 1.0;
    screen.x *= float(extent.x) / float(extent.y);
    float3 forward;
    float3 right;
    float3 up;
    Stage11CameraBasis(CameraYaw, CameraPitch, forward, right, up);
    return normalize(forward * STAGE11_CAMERA_FOCAL_LENGTH + right * screen.x - up * screen.y);
}

float StableSchlick(float cosine, float incidentIor, float transmittedIor)
{
    float r0 = (incidentIor - transmittedIor) / max(incidentIor + transmittedIor, 1.0e-6);
    r0 *= r0;
    return r0 + (1.0 - r0) * pow(1.0 - cosine, 5.0);
}

float AverageThroughput(float3 throughput)
{
    return (throughput.x + throughput.y + throughput.z) / 3.0;
}

bool EnqueueBranch(
    inout BuildBranchState queue[MAX_BUILD_QUEUE],
    inout uint tail,
    BuildBranchState branch)
{
    if (tail >= MAX_BUILD_QUEUE || AverageThroughput(branch.throughput) < 1.0e-5)
        return false;
    queue[tail++] = branch;
    return true;
}

[numthreads(8, 8, 1)]
void main(uint3 dispatchId : SV_DispatchThreadID)
{
    uint width;
    uint height;
    StableRadiance.GetDimensions(width, height);
    uint2 extent = uint2(width, height);
    uint2 pixel = dispatchId.xy;
    if (any(pixel >= extent))
        return;

    [unroll]
    for (uint plane = 0u; plane < STABLE_PLANE_COUNT; ++plane)
    {
        StablePlaneHeaders[uint3(pixel, plane)] = STABLE_BRANCH_INVALID;
        StablePlaneNoisyDiffuse[uint3(pixel, plane)] = 0.0;
        StablePlaneNoisySpecular[uint3(pixel, plane)] = 0.0;
    }
    StableRadiance[pixel] = 0.0;

    BuildBranchState queue[MAX_BUILD_QUEUE];
    BuildBranchState root;
    root.origin = CameraPosition;
    root.direction = StablePrimaryRayDirection(pixel, extent);
    root.throughput = 1.0.xxx;
    root.sceneLength = 0.0;
    root.branchId = STABLE_BRANCH_ROOT;
    root.depth = 0u;
    root.interiorSlots = 0u;
    queue[0] = root;

    uint head = 0u;
    uint tail = 1u;
    uint planeCount = 0u;
    uint dominantPlane = 0u;
    float dominantWeight = -1.0;

    while (head < tail && planeCount < STABLE_PLANE_COUNT)
    {
        BuildBranchState state = queue[head++];
        uint rejectedIntersections = 0u;

        for (uint step = 0u; step < MAX_BUILD_STEPS; ++step)
        {
            RayDesc ray;
            ray.Origin = state.origin;
            ray.Direction = state.direction;
            ray.TMin = 0.001;
            ray.TMax = 1000.0;
            RayQuery<RAY_FLAG_NONE> query;
            query.TraceRayInline(Scene, RAY_FLAG_NONE, 0xff, ray);
            while (query.Proceed())
            {
            }
            if (query.CommittedStatus() != COMMITTED_TRIANGLE_HIT)
                break;

            uint instanceId = query.CommittedInstanceID();
            InstanceGpu instanceData = Instances[instanceId];
            uint primitive = query.CommittedPrimitiveIndex();
            float2 bary = query.CommittedTriangleBarycentrics();
            float3 barycentric = float3(1.0 - bary.x - bary.y, bary.x, bary.y);
            uint3 triangleIndices = uint3(
                Indices[instanceData.indexOffset + primitive * 3u],
                Indices[instanceData.indexOffset + primitive * 3u + 1u],
                Indices[instanceData.indexOffset + primitive * 3u + 2u]);
            uint vertex0 = instanceData.vertexOffset + triangleIndices.x;
            uint vertex1 = instanceData.vertexOffset + triangleIndices.y;
            uint vertex2 = instanceData.vertexOffset + triangleIndices.z;
            float3 localNormal = normalize(
                Vertices[vertex0].normal * barycentric.x
                + Vertices[vertex1].normal * barycentric.y
                + Vertices[vertex2].normal * barycentric.z);
            float3 geometricNormal = normalize(mul(
                localNormal,
                (float3x3)query.CommittedWorldToObject3x4()));
            bool frontFace = query.CommittedTriangleFrontFace();
            float3 normal = frontFace ? geometricNormal : -geometricNormal;
            uint materialIndex = instanceData.materialIndex;
            Material material = Materials[materialIndex];
            bool dielectric = (material.flags & 4u) != 0u;
            bool mirror = (material.flags & 8u) != 0u;
            float hitT = query.CommittedRayT();
            float3 hitPosition = state.origin + state.direction * hitT;

            PathInteriorList interior;
            interior.slots = state.interiorSlots;
            uint currentMaterial = interior.TopMaterial();
            if (currentMaterial != NO_INTERIOR_MATERIAL)
            {
                // Closed media tint by traveled distance, not by repeatedly
                // multiplying the interface base color. This is the same
                // Beer-Lambert convention used by RTXPT-style nested media.
                state.throughput *= exp(
                    -max(Materials[currentMaterial].absorptionCoefficient, 0.0.xxx) * hitT);
            }
            if (dielectric && !MaterialIsThinSurface(material)
                && !interior.IsTrueIntersection(MaterialNestedPriority(material)))
            {
                if (++rejectedIntersections > MAX_FALSE_INTERSECTIONS)
                    break;
                state.origin = hitPosition + state.direction * 0.002;
                state.sceneLength += hitT;
                continue;
            }

            if (!dielectric && !mirror)
            {
                uint address = StablePlaneAddress(pixel, planeCount, extent);
                StablePlaneRecord record;
                record.data0 = float4(state.origin, hitT);
                record.data1 = float4(state.direction, state.sceneLength);
                record.data2 = float4(state.throughput, asfloat(state.depth));
                record.data3 = float4(
                    asfloat(state.interiorSlots.x),
                    asfloat(state.interiorSlots.y),
                    EncodeStableDirection(StablePrimaryRayDirection(pixel, extent)));
                StablePlaneRecords[address] = record;
                StablePlaneHeaders[uint3(pixel, planeCount)] = state.branchId;
                float weight = AverageThroughput(state.throughput);
                if (weight > dominantWeight)
                {
                    dominantWeight = weight;
                    dominantPlane = planeCount;
                }
                ++planeCount;
                break;
            }

            uint reflectedBranchId;
            if (!AdvanceStableBranch(state.branchId, 1u, reflectedBranchId))
                break;
            BuildBranchState reflected = state;
            reflected.origin = hitPosition + normal * 0.002;
            reflected.direction = normalize(reflect(state.direction, normal));
            reflected.sceneLength += hitT;
            reflected.branchId = reflectedBranchId;
            reflected.depth = state.depth + 1u;

            if (mirror)
            {
                reflected.throughput *= max(material.baseColorFactor.xyz, 0.0.xxx);
                EnqueueBranch(queue, tail, reflected);
                break;
            }

            float incidentIor = currentMaterial == NO_INTERIOR_MATERIAL
                ? 1.0
                : max(Materials[currentMaterial].ior, 1.0e-4);
            PathInteriorList transmittedInterior = interior;
            float transmittedIor = 1.0;
            bool mediumUpdateValid = true;
            if (MaterialIsThinSurface(material))
            {
                transmittedIor = max(material.ior, 1.0e-4);
            }
            else if (frontFace)
            {
                transmittedIor = max(material.ior, 1.0e-4);
                mediumUpdateValid = transmittedInterior.Enter(
                    materialIndex,
                    MaterialNestedPriority(material));
            }
            else
            {
                mediumUpdateValid = transmittedInterior.Exit(materialIndex);
                uint outerMaterial = transmittedInterior.TopMaterial();
                transmittedIor = outerMaterial == NO_INTERIOR_MATERIAL
                    ? 1.0
                    : max(Materials[outerMaterial].ior, 1.0e-4);
            }

            float cosine = saturate(dot(-state.direction, normal));
            float fresnel = StableSchlick(cosine, incidentIor, transmittedIor);
            float eta = incidentIor / transmittedIor;
            float3 refractionDirection = MaterialIsThinSurface(material)
                ? state.direction
                : refract(state.direction, normal, eta);
            bool totalInternalReflection = length(refractionDirection) < 1.0e-4;
            reflected.throughput *= totalInternalReflection ? 1.0 : fresnel;
            EnqueueBranch(queue, tail, reflected);

            if (!totalInternalReflection && mediumUpdateValid)
            {
                uint transmittedBranchId;
                if (AdvanceStableBranch(state.branchId, 2u, transmittedBranchId))
                {
                    BuildBranchState transmitted = state;
                    transmitted.origin = hitPosition + normalize(refractionDirection) * 0.002;
                    transmitted.direction = normalize(refractionDirection);
                    transmitted.throughput *= 1.0 - fresnel;
                    transmitted.sceneLength += hitT;
                    transmitted.branchId = transmittedBranchId;
                    transmitted.depth = state.depth + 1u;
                    transmitted.interiorSlots = transmittedInterior.slots;
                    EnqueueBranch(queue, tail, transmitted);
                }
            }
            break;
        }
    }

    // Alpha carries only an exact small integer and is not radiance. P3/P4 use
    // it to select the primary guide after all planes have been filled.
    StableRadiance[pixel] = float4(0.0, 0.0, 0.0, float(dominantPlane));
}
