// Cornell Box DXR path tracer. This pass only traces one independent sample and
// writes first-hit attributes. Temporal reconstruction and spatial filtering are
// deliberately performed by separate compute dispatches.
RaytracingAccelerationStructure Scene : register(t0);

struct Vertex
{
    float3 position;
    float3 normal;
};

struct Material
{
    float4 albedo;
    // xyz: emission, w: 0 diffuse, 1 metal, 2 dielectric, 3 emissive.
    float4 emissionAndKind;
};

StructuredBuffer<Vertex> Vertices : register(t1);
StructuredBuffer<uint> Indices : register(t2);
StructuredBuffer<uint> MaterialIndices : register(t3);
StructuredBuffer<Material> Materials : register(t4);
StructuredBuffer<uint> ObjectIndices : register(t5);

RWTexture2D<float4> RawDiffuse : register(u0);
RWTexture2D<float4> RawSpecular : register(u1);
RWTexture2D<float4> GBufferAlbedo : register(u2);
RWTexture2D<float4> GBufferNormalRoughness : register(u3);
RWTexture2D<float> GBufferDepth : register(u4);
RWTexture2D<float2> GBufferMotion : register(u5);
RWTexture2D<uint> GBufferId : register(u6);
RWTexture2D<float4> GBufferWorldPosition : register(u7);
RWTexture2D<float> GBufferHitDistance : register(u8);

cbuffer FrameConstants : register(b0)
{
    uint FrameIndex;
    float3 CameraPosition;
    float CameraYaw;
    float CameraPitch;
    float2 CameraPadding;
    float3 PreviousCameraPosition;
    float PreviousCameraYaw;
    float PreviousCameraPitch;
    float2 PreviousCameraPadding;
    uint ResetHistory;
};

struct Payload
{
    float3 radiance;
    uint seed;
    uint depth;
    float lastPdf;
    uint firstKind;
    float hitDistance;
};

uint RandomUint(inout uint state)
{
    // PCG-inspired permutation. The frame/pixel seed remains reproducible while
    // avoiding the strong neighboring-pixel correlation of a bare xorshift.
    state = state * 747796405u + 2891336453u;
    uint word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

float RandomFloat(inout uint state)
{
    return (RandomUint(state) & 0x00FFFFFFu) / 16777216.0;
}

float3 SampleCosineHemisphere(float3 normal, inout uint seed)
{
    float radius = sqrt(RandomFloat(seed));
    float angle = 6.28318530718 * RandomFloat(seed);
    float2 disk = radius * float2(cos(angle), sin(angle));
    float z = sqrt(max(0.0, 1.0 - dot(disk, disk)));
    float3 tangent = normalize(abs(normal.z) < 0.999
        ? cross(float3(0, 0, 1), normal)
        : cross(float3(0, 1, 0), normal));
    float3 bitangent = cross(normal, tangent);
    return normalize(tangent * disk.x + bitangent * disk.y + normal * z);
}

float Schlick(float cosine, float etaRatio)
{
    float r0 = (1.0 - etaRatio) / (1.0 + etaRatio);
    r0 *= r0;
    return r0 + (1.0 - r0) * pow(1.0 - cosine, 5.0);
}

void CameraBasis(float yaw, float pitch, out float3 forward, out float3 right, out float3 up)
{
    forward = normalize(float3(sin(yaw) * cos(pitch), sin(pitch), cos(yaw) * cos(pitch)));
    right = normalize(cross(float3(0, 1, 0), forward));
    up = cross(forward, right);
}

float2 ProjectToPreviousUv(float3 worldPosition, uint2 size)
{
    const float focalLength = 2.747477419;
    float3 forward;
    float3 right;
    float3 up;
    CameraBasis(PreviousCameraYaw, PreviousCameraPitch, forward, right, up);
    float3 relative = worldPosition - PreviousCameraPosition;
    float forwardDistance = dot(relative, forward);
    if (forwardDistance <= 0.0001)
        return float2(-2.0, -2.0);

    float aspect = float(size.x) / float(size.y);
    float2 screen;
    screen.x = focalLength * dot(relative, right) / forwardDistance;
    screen.y = -focalLength * dot(relative, up) / forwardDistance;
    return float2(screen.x / aspect, screen.y) * 0.5 + 0.5;
}

[shader("raygeneration")]
void RayGen()
{
    uint2 pixel = DispatchRaysIndex().xy;
    uint2 size = DispatchRaysDimensions().xy;
    uint seed = pixel.x * 1973u + pixel.y * 9277u + FrameIndex * 26699u + 89173u;
    float2 jitter = float2(RandomFloat(seed), RandomFloat(seed));
    float2 uv = (float2(pixel) + jitter) / float2(size);
    float2 screen = uv * 2.0 - 1.0;
    screen.x *= float(size.x) / float(size.y);

    float3 forward;
    float3 right;
    float3 up;
    CameraBasis(CameraYaw, CameraPitch, forward, right, up);

    RayDesc ray;
    ray.Origin = CameraPosition;
    ray.Direction = normalize(forward * 2.747477419 + right * screen.x - up * screen.y);
    ray.TMin = 0.001;
    ray.TMax = 1000.0;

    Payload payload;
    payload.radiance = 0;
    payload.seed = seed;
    payload.depth = 0;
    payload.lastPdf = 0;
    payload.firstKind = 0;
    payload.hitDistance = 0;

    GBufferAlbedo[pixel] = 0;
    GBufferNormalRoughness[pixel] = 0;
    GBufferDepth[pixel] = 0;
    GBufferMotion[pixel] = 0;
    GBufferId[pixel] = 0xFFFFFFFFu;
    GBufferWorldPosition[pixel] = 0;
    GBufferHitDistance[pixel] = 0;
    TraceRay(Scene, RAY_FLAG_NONE, 0xFF, 0, 1, 0, ray, payload);

    float4 sample = float4(payload.radiance, 1.0);
    bool specularPrimary = payload.firstKind == 1u || payload.firstKind == 2u;
    RawDiffuse[pixel] = specularPrimary ? 0 : sample;
    RawSpecular[pixel] = specularPrimary ? sample : 0;
}

[shader("miss")]
void Miss(inout Payload payload)
{
    payload.radiance = 0;
    payload.hitDistance = 0;
}

[shader("miss")]
void ShadowMiss(inout Payload payload)
{
    payload.radiance = 1.0;
}

[shader("closesthit")]
void ClosestHit(inout Payload payload, in BuiltInTriangleIntersectionAttributes attributes)
{
    uint primitive = PrimitiveIndex();
    uint3 triIndices = uint3(
        Indices[primitive * 3],
        Indices[primitive * 3 + 1],
        Indices[primitive * 3 + 2]);
    float3 barycentric = float3(
        1.0 - attributes.barycentrics.x - attributes.barycentrics.y,
        attributes.barycentrics.x,
        attributes.barycentrics.y);
    float3 geometricNormal = normalize(
        Vertices[triIndices.x].normal * barycentric.x
        + Vertices[triIndices.y].normal * barycentric.y
        + Vertices[triIndices.z].normal * barycentric.z);
    bool frontFace = dot(WorldRayDirection(), geometricNormal) < 0.0;
    float3 normal = frontFace ? geometricNormal : -geometricNormal;

    uint materialIndex = MaterialIndices[primitive];
    Material material = Materials[materialIndex];
    uint kind = uint(material.emissionAndKind.w + 0.5);
    float3 hitPosition = WorldRayOrigin() + RayTCurrent() * WorldRayDirection();
    payload.hitDistance = RayTCurrent();

    if (payload.depth == 0)
    {
        uint2 pixel = DispatchRaysIndex().xy;
        uint2 size = DispatchRaysDimensions().xy;
        float roughness = kind == 0u ? 1.0 : (kind == 1u ? 0.05 : 0.0);
        payload.firstKind = kind;
        GBufferAlbedo[pixel] = float4(material.albedo.xyz, float(kind));
        GBufferNormalRoughness[pixel] = float4(normal * 0.5 + 0.5, roughness);
        GBufferDepth[pixel] = RayTCurrent();
        GBufferId[pixel] = (InstanceID() << 24u) | (ObjectIndices[primitive] << 8u) | materialIndex;
        GBufferWorldPosition[pixel] = float4(hitPosition, 1.0);
        float2 currentUv = (float2(pixel) + 0.5) / float2(size);
        float2 previousUv = ProjectToPreviousUv(hitPosition, size);
        GBufferMotion[pixel] = ResetHistory != 0u
            ? 0
            : (currentUv - previousUv) * float2(size);
    }

    if (kind == 3u)
    {
        float weight = 1.0;
        if (payload.depth > 0 && payload.lastPdf > 0.0)
        {
            const float lightArea = 0.25;
            float lightPdf = RayTCurrent() * RayTCurrent()
                / max(0.0001, abs(dot(normal, -WorldRayDirection())) * lightArea);
            float bsdfSquared = payload.lastPdf * payload.lastPdf;
            weight = bsdfSquared / (bsdfSquared + lightPdf * lightPdf);
        }
        payload.radiance = material.emissionAndKind.xyz * weight;
        return;
    }
    if (payload.depth >= 3)
    {
        payload.radiance = 0;
        return;
    }

    float3 directLighting = 0;
    float3 direction;
    if (kind == 1u)
    {
        direction = reflect(WorldRayDirection(), normal);
    }
    else if (kind == 2u)
    {
        float etaRatio = frontFace ? (1.0 / 1.5) : 1.5;
        float cosine = saturate(dot(-WorldRayDirection(), normal));
        float3 refracted = refract(WorldRayDirection(), normal, etaRatio);
        direction = length(refracted) < 0.001 || Schlick(cosine, etaRatio) > RandomFloat(payload.seed)
            ? reflect(WorldRayDirection(), normal)
            : refracted;
    }
    else
    {
        float2 lightRandom = float2(RandomFloat(payload.seed), RandomFloat(payload.seed));
        float3 lightPoint = float3(-0.25 + lightRandom.x * 0.5, 0.9966667, 0.6666667 + lightRandom.y * 0.5);
        float3 toLight = lightPoint - hitPosition;
        float lightDistance = length(toLight);
        float3 lightDirection = toLight / lightDistance;
        float surfaceCosine = max(0.0, dot(normal, lightDirection));
        float lightCosine = max(0.0, dot(float3(0, -1, 0), -lightDirection));
        if (surfaceCosine > 0.0 && lightCosine > 0.0)
        {
            Payload shadow;
            shadow.radiance = 0;
            shadow.seed = payload.seed;
            shadow.depth = payload.depth;
            shadow.lastPdf = 0;
            shadow.firstKind = payload.firstKind;
            shadow.hitDistance = 0;
            RayDesc shadowRay;
            shadowRay.Origin = hitPosition + normal * 0.002;
            shadowRay.Direction = lightDirection;
            shadowRay.TMin = 0.001;
            shadowRay.TMax = lightDistance - 0.004;
            TraceRay(
                Scene,
                RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER,
                0xFF,
                0,
                1,
                1,
                shadowRay,
                shadow);
            const float lightArea = 0.25;
            float lightPdf = lightDistance * lightDistance / (lightCosine * lightArea);
            float bsdfPdf = surfaceCosine / 3.14159265359;
            float lightSquared = lightPdf * lightPdf;
            float misWeight = lightSquared / (lightSquared + bsdfPdf * bsdfPdf);
            directLighting = shadow.radiance * material.albedo.xyz * Materials[3].emissionAndKind.xyz
                * (surfaceCosine / 3.14159265359) * misWeight / lightPdf;
        }
        direction = SampleCosineHemisphere(normal, payload.seed);
    }

    RayDesc bounce;
    bounce.Origin = hitPosition + direction * 0.002;
    bounce.Direction = normalize(direction);
    bounce.TMin = 0.001;
    bounce.TMax = 1000.0;
    Payload child;
    child.radiance = 0;
    child.seed = payload.seed;
    child.depth = payload.depth + 1;
    child.lastPdf = kind == 0u
        ? max(0.0, dot(normal, bounce.Direction)) / 3.14159265359
        : 0.0;
    child.firstKind = payload.firstKind;
    child.hitDistance = 0;
    TraceRay(Scene, RAY_FLAG_NONE, 0xFF, 0, 1, 0, bounce, child);
    payload.seed = child.seed;
    if (payload.depth == 0 && (kind == 1u || kind == 2u))
        GBufferHitDistance[DispatchRaysIndex().xy] = child.hitDistance;
    payload.radiance = directLighting + material.albedo.xyz * child.radiance;
}
