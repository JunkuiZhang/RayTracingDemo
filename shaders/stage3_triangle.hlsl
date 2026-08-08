// Cornell Box DXR path tracer. This pass only traces one independent sample and
// writes first-hit attributes. Temporal reconstruction and spatial filtering are
// deliberately performed by separate compute dispatches.
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
StructuredBuffer<Material> Materials : register(t3);
StructuredBuffer<InstanceGpu> Instances : register(t4);
Texture2D<float4> MaterialTextures[] : register(t5);
static const uint MAX_MATERIAL_SAMPLERS = 64u;
SamplerState MaterialSamplers[MAX_MATERIAL_SAMPLERS] : register(s0);

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
    float3 rawDiffuse;
    float3 rawSpecular;
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

float3 PreviousWorldPosition(float3 localPosition, InstanceGpu instanceData)
{
    float3x4 previousObjectToWorld = float3x4(
        instanceData.previousObjectToWorldRow0,
        instanceData.previousObjectToWorldRow1,
        instanceData.previousObjectToWorldRow2);
    return mul(previousObjectToWorld, float4(localPosition, 1.0));
}

float4 SampleMaterialTexture(uint textureAndSampler, float2 uv)
{
    const uint textureViewMask = (1u << 7u) - 1u;
    const uint samplerIndexMask = (1u << 6u) - 1u;
    uint textureView = textureAndSampler & textureViewMask;
    uint samplerIndex = (textureAndSampler >> 7u) & samplerIndexMask;
    return MaterialTextures[NonUniformResourceIndex(textureView)]
        .SampleLevel(MaterialSamplers[NonUniformResourceIndex(samplerIndex)], uv, 0.0);
}

static const float PI = 3.14159265359;

float3 FresnelSchlick(float cosine, float3 f0)
{
    return f0 + (1.0 - f0) * pow(1.0 - saturate(cosine), 5.0);
}

float D_GGX(float NoH, float roughness)
{
    float alpha = roughness * roughness;
    float alphaSquared = alpha * alpha;
    float denominator = NoH * NoH * (alphaSquared - 1.0) + 1.0;
    return alphaSquared / max(PI * denominator * denominator, 1.0e-7);
}

float G_SchlickGGX(float NoX, float roughness)
{
    float k = (roughness + 1.0) * (roughness + 1.0) / 8.0;
    return NoX / max(NoX * (1.0 - k) + k, 1.0e-6);
}

float G_Smith(float NoV, float NoL, float roughness)
{
    return G_SchlickGGX(NoV, roughness) * G_SchlickGGX(NoL, roughness);
}

struct BrdfEvaluation
{
    float3 diffuse;
    float3 specular;
    float diffusePdf;
    float specularPdf;
};

BrdfEvaluation EvaluateBrdf(
    float3 normal,
    float3 viewDirection,
    float3 lightDirection,
    float3 baseColor,
    float metallic,
    float roughness)
{
    BrdfEvaluation value;
    value.diffuse = 0;
    value.specular = 0;
    value.diffusePdf = 0;
    value.specularPdf = 0;
    float NoV = saturate(dot(normal, viewDirection));
    float NoL = saturate(dot(normal, lightDirection));
    if (NoV <= 0.0 || NoL <= 0.0)
        return value;

    float3 halfVector = normalize(viewDirection + lightDirection);
    float NoH = saturate(dot(normal, halfVector));
    float VoH = saturate(dot(viewDirection, halfVector));
    float3 f0 = lerp(0.04.xxx, baseColor, metallic);
    float3 fresnel = FresnelSchlick(VoH, f0);
    float distribution = D_GGX(NoH, roughness);
    float geometry = G_Smith(NoV, NoL, roughness);
    value.specular = distribution * geometry * fresnel
        / max(4.0 * NoV * NoL, 1.0e-6);
    value.diffuse = (1.0 - metallic) * (1.0 - fresnel) * baseColor / PI;
    value.diffusePdf = NoL / PI;
    value.specularPdf = distribution * NoH / max(4.0 * VoH, 1.0e-6);
    return value;
}

float3 SampleGgxDirection(
    float3 normal,
    float3 viewDirection,
    float roughness,
    inout uint seed,
    out float pdf)
{
    float alpha = roughness * roughness;
    float phi = 2.0 * PI * RandomFloat(seed);
    float random = RandomFloat(seed);
    float cosTheta = sqrt((1.0 - random) / (1.0 + (alpha * alpha - 1.0) * random));
    float sinTheta = sqrt(max(0.0, 1.0 - cosTheta * cosTheta));
    float3 halfTangent = float3(sinTheta * cos(phi), sinTheta * sin(phi), cosTheta);
    float3 tangent = normalize(abs(normal.z) < 0.999
        ? cross(float3(0, 0, 1), normal)
        : cross(float3(0, 1, 0), normal));
    float3 bitangent = cross(normal, tangent);
    float3 halfVector = normalize(
        tangent * halfTangent.x + bitangent * halfTangent.y + normal * halfTangent.z);
    float3 lightDirection = normalize(reflect(-viewDirection, halfVector));
    float NoH = saturate(dot(normal, halfVector));
    float VoH = saturate(dot(viewDirection, halfVector));
    pdf = D_GGX(NoH, roughness) * NoH / max(4.0 * VoH, 1.0e-6);
    return lightDirection;
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
    payload.rawDiffuse = 0;
    payload.rawSpecular = 0;

    GBufferAlbedo[pixel] = 0;
    GBufferNormalRoughness[pixel] = 0;
    GBufferDepth[pixel] = 0;
    GBufferMotion[pixel] = 0;
    GBufferId[pixel] = 0xFFFFFFFFu;
    GBufferWorldPosition[pixel] = 0;
    GBufferHitDistance[pixel] = 0;
    TraceRay(Scene, RAY_FLAG_NONE, 0xFF, 0, 1, 0, ray, payload);

    RawDiffuse[pixel] = float4(payload.rawDiffuse, 1.0);
    RawSpecular[pixel] = float4(payload.rawSpecular, 1.0);
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
    InstanceGpu instanceData = Instances[InstanceID()];
    uint3 triIndices = uint3(
        Indices[instanceData.indexOffset + primitive * 3],
        Indices[instanceData.indexOffset + primitive * 3 + 1],
        Indices[instanceData.indexOffset + primitive * 3 + 2]);
    float3 barycentric = float3(
        1.0 - attributes.barycentrics.x - attributes.barycentrics.y,
        attributes.barycentrics.x,
        attributes.barycentrics.y);
    uint vertex0 = instanceData.vertexOffset + triIndices.x;
    uint vertex1 = instanceData.vertexOffset + triIndices.y;
    uint vertex2 = instanceData.vertexOffset + triIndices.z;
    float3 localPosition = Vertices[vertex0].position * barycentric.x
        + Vertices[vertex1].position * barycentric.y
        + Vertices[vertex2].position * barycentric.z;
    float3 localNormal = normalize(
        Vertices[vertex0].normal * barycentric.x
        + Vertices[vertex1].normal * barycentric.y
        + Vertices[vertex2].normal * barycentric.z);
    float4 localTangent = Vertices[vertex0].tangent * barycentric.x
        + Vertices[vertex1].tangent * barycentric.y
        + Vertices[vertex2].tangent * barycentric.z;
    float2 texcoord0 = Vertices[vertex0].texcoord0 * barycentric.x
        + Vertices[vertex1].texcoord0 * barycentric.y
        + Vertices[vertex2].texcoord0 * barycentric.z;

    uint materialIndex = instanceData.materialIndex;
    Material material = Materials[materialIndex];
    float3 geometricNormal = normalize(mul(localNormal, (float3x3)WorldToObject3x4()));
    bool frontFace = HitKind() == HIT_KIND_TRIANGLE_FRONT_FACE;
    bool doubleSided = (material.flags & 1u) != 0u
        || (material.flags & 4u) != 0u;
    float3 normal = frontFace ? geometricNormal : -geometricNormal;
    if (!frontFace && !doubleSided)
        return;
    float4 baseColor = material.baseColorFactor
        * SampleMaterialTexture(material.baseColorTextureAndSampler, texcoord0);
    float4 metallicRoughness = SampleMaterialTexture(
        material.metallicRoughnessTextureAndSampler,
        texcoord0);
    float3 emissive = material.emissiveFactor
        * SampleMaterialTexture(material.emissiveTextureAndSampler, texcoord0).xyz;
    float metallic = saturate(material.metallicFactor * metallicRoughness.b);
    float roughness = clamp(material.roughnessFactor * metallicRoughness.g, 0.045, 1.0);

    if ((material.flags & 2u) != 0u)
    {
        float3 tangent = normalize(mul(localTangent.xyz, (float3x3)ObjectToWorld3x4()));
        tangent = normalize(tangent - normal * dot(normal, tangent));
        float3 bitangent = normalize(cross(normal, tangent)) * localTangent.w;
        float3 tangentNormal = SampleMaterialTexture(
            material.normalTextureAndSampler,
            texcoord0).xyz * 2.0 - 1.0;
        tangentNormal.xy *= material.normalScale;
        normal = normalize(
            tangent * tangentNormal.x + bitangent * tangentNormal.y + normal * tangentNormal.z);
        if (dot(normal, -WorldRayDirection()) < 0.0)
            normal = -normal;
    }

    bool legacyDielectric = (material.flags & 4u) != 0u;
    bool legacyMetal = (material.flags & 8u) != 0u;
    bool legacyEmissive = (material.flags & 16u) != 0u;
    uint kind = legacyDielectric ? 2u : (legacyMetal ? 1u : (legacyEmissive ? 3u : 0u));
    float3 hitPosition = mul(ObjectToWorld3x4(), float4(localPosition, 1.0));
    float3 previousHitPosition = PreviousWorldPosition(localPosition, instanceData);
    payload.hitDistance = RayTCurrent();

    if (payload.depth == 0)
    {
        uint2 pixel = DispatchRaysIndex().xy;
        uint2 size = DispatchRaysDimensions().xy;
        payload.firstKind = kind;
        GBufferAlbedo[pixel] = float4(baseColor.xyz, float(kind));
        GBufferNormalRoughness[pixel] = float4(normal * 0.5 + 0.5, roughness);
        GBufferDepth[pixel] = RayTCurrent();
        GBufferId[pixel] = instanceData.stableSurfaceId;
        GBufferWorldPosition[pixel] = float4(hitPosition, 1.0);
        float2 currentUv = (float2(pixel) + 0.5) / float2(size);
        float2 previousUv = ProjectToPreviousUv(previousHitPosition, size);
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
            weight = bsdfSquared / max(bsdfSquared + lightPdf * lightPdf, 1.0e-7);
        }
        payload.radiance = emissive * weight;
        if (payload.depth == 0)
            payload.rawDiffuse = payload.radiance;
        return;
    }
    if (payload.depth >= 3)
    {
        payload.radiance = kind == 0u ? emissive : 0;
        return;
    }

    float3 viewDirection = normalize(-WorldRayDirection());
    float3 directDiffuse = 0;
    float3 directSpecular = 0;
    if (kind == 0u)
    {
        float2 lightRandom = float2(RandomFloat(payload.seed), RandomFloat(payload.seed));
        float3 lightPoint = float3(-0.25 + lightRandom.x * 0.5, 0.9966667, 0.6666667 + lightRandom.y * 0.5);
        float3 toLight = lightPoint - hitPosition;
        float lightDistance = length(toLight);
        float3 lightDirection = toLight / max(lightDistance, 1.0e-6);
        float NoL = max(0.0, dot(normal, lightDirection));
        float lightCosine = max(0.0, dot(float3(0, -1, 0), -lightDirection));
        if (NoL > 0.0 && lightCosine > 0.0)
        {
            Payload shadow;
            shadow.radiance = 0;
            shadow.seed = payload.seed;
            shadow.depth = payload.depth;
            shadow.lastPdf = 0;
            shadow.firstKind = 0;
            shadow.hitDistance = 0;
            shadow.rawDiffuse = 0;
            shadow.rawSpecular = 0;
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
            BrdfEvaluation brdf = EvaluateBrdf(
                normal,
                viewDirection,
                lightDirection,
                baseColor.xyz,
                metallic,
                roughness);
            float specularProbability = clamp(
                max(max(lerp(0.04, baseColor.x, metallic), lerp(0.04, baseColor.y, metallic)),
                    lerp(0.04, baseColor.z, metallic)),
                0.05,
                0.95);
            float bsdfPdf = (1.0 - specularProbability) * brdf.diffusePdf
                + specularProbability * brdf.specularPdf;
            float lightPdf = lightDistance * lightDistance
                / max(lightCosine * 0.25, 1.0e-6);
            float lightSquared = lightPdf * lightPdf;
            float bsdfSquared = bsdfPdf * bsdfPdf;
            float misWeight = lightSquared / max(lightSquared + bsdfSquared, 1.0e-7);
            float3 lightRadiance = Materials[3].emissiveFactor;
            float visibility = shadow.radiance;
            directDiffuse = visibility * lightRadiance * brdf.diffuse * NoL
                * misWeight / max(lightPdf, 1.0e-6);
            directSpecular = visibility * lightRadiance * brdf.specular * NoL
                * misWeight / max(lightPdf, 1.0e-6);
        }
    }

    float3 direction = 0;
    float3 bounceWeight = 0;
    float samplePdf = 1.0;
    bool sampledSpecular = kind == 1u || kind == 2u;
    if (kind == 1u)
    {
        direction = reflect(WorldRayDirection(), normal);
        bounceWeight = baseColor.xyz;
    }
    else if (kind == 2u)
    {
        float etaRatio = frontFace ? (1.0 / max(material.ior, 1.0001)) : max(material.ior, 1.0001);
        float cosine = saturate(dot(-WorldRayDirection(), normal));
        float3 refracted = refract(WorldRayDirection(), normal, etaRatio);
        bool reflectRay = length(refracted) < 0.001
            || Schlick(cosine, etaRatio) > RandomFloat(payload.seed);
        direction = reflectRay ? reflect(WorldRayDirection(), normal) : refracted;
        bounceWeight = baseColor.xyz;
    }
    else
    {
        float3 f0 = lerp(0.04.xxx, baseColor.xyz, metallic);
        float specularProbability = clamp(max(max(f0.x, f0.y), f0.z), 0.05, 0.95);
        sampledSpecular = RandomFloat(payload.seed) < specularProbability;
        if (sampledSpecular)
        {
            direction = SampleGgxDirection(normal, viewDirection, roughness, payload.seed, samplePdf);
            float NoL = max(0.0, dot(normal, direction));
            BrdfEvaluation brdf = EvaluateBrdf(
                normal,
                viewDirection,
                direction,
                baseColor.xyz,
                metallic,
                roughness);
            samplePdf = specularProbability * brdf.specularPdf
                + (1.0 - specularProbability) * brdf.diffusePdf;
            bounceWeight = (brdf.diffuse + brdf.specular) * NoL / max(samplePdf, 1.0e-6);
        }
        else
        {
            direction = SampleCosineHemisphere(normal, payload.seed);
            float NoL = max(0.0, dot(normal, direction));
            BrdfEvaluation brdf = EvaluateBrdf(
                normal,
                viewDirection,
                direction,
                baseColor.xyz,
                metallic,
                roughness);
            samplePdf = (1.0 - specularProbability) * brdf.diffusePdf
                + specularProbability * brdf.specularPdf;
            bounceWeight = (brdf.diffuse + brdf.specular) * NoL / max(samplePdf, 1.0e-6);
        }
    }

    if (dot(normal, direction) <= 0.0)
        bounceWeight = 0;
    RayDesc bounce;
    bounce.Origin = hitPosition + (kind == 2u ? direction : normal) * 0.002;
    bounce.Direction = normalize(direction);
    bounce.TMin = 0.001;
    bounce.TMax = 1000.0;
    Payload child;
    child.radiance = 0;
    child.seed = payload.seed;
    child.depth = payload.depth + 1;
    child.lastPdf = kind == 0u ? max(samplePdf, 1.0e-6) : 0.0;
    child.firstKind = 0;
    child.hitDistance = 0;
    child.rawDiffuse = 0;
    child.rawSpecular = 0;
    TraceRay(Scene, RAY_FLAG_NONE, 0xFF, 0, 1, 0, bounce, child);
    payload.seed = child.seed;
    float3 bouncedRadiance = bounceWeight * child.radiance;
    if (payload.depth == 0)
    {
        payload.rawDiffuse = emissive + directDiffuse;
        payload.rawSpecular = directSpecular;
        if (sampledSpecular)
            payload.rawSpecular += bouncedRadiance;
        else
            payload.rawDiffuse += bouncedRadiance;
    }
    if (payload.depth == 0 && sampledSpecular)
        GBufferHitDistance[DispatchRaysIndex().xy] = child.hitDistance;
    payload.radiance = emissive + directDiffuse + directSpecular + bouncedRadiance;
}

// Kept as a reference while validating the GGX replacement; it is not an
// exported shader and therefore cannot be selected by the DXR hit group.
void LegacyClosestHit(inout Payload payload, in BuiltInTriangleIntersectionAttributes attributes)
{
    uint primitive = PrimitiveIndex();
    InstanceGpu instanceData = Instances[InstanceID()];
    uint3 triIndices = uint3(
        Indices[instanceData.indexOffset + primitive * 3],
        Indices[instanceData.indexOffset + primitive * 3 + 1],
        Indices[instanceData.indexOffset + primitive * 3 + 2]);
    float3 barycentric = float3(
        1.0 - attributes.barycentrics.x - attributes.barycentrics.y,
        attributes.barycentrics.x,
        attributes.barycentrics.y);
    float3 localPosition = Vertices[instanceData.vertexOffset + triIndices.x].position * barycentric.x
        + Vertices[instanceData.vertexOffset + triIndices.y].position * barycentric.y
        + Vertices[instanceData.vertexOffset + triIndices.z].position * barycentric.z;
    float3 localNormal = normalize(
        Vertices[instanceData.vertexOffset + triIndices.x].normal * barycentric.x
        + Vertices[instanceData.vertexOffset + triIndices.y].normal * barycentric.y
        + Vertices[instanceData.vertexOffset + triIndices.z].normal * barycentric.z);
    float2 texcoord0 = Vertices[instanceData.vertexOffset + triIndices.x].texcoord0 * barycentric.x
        + Vertices[instanceData.vertexOffset + triIndices.y].texcoord0 * barycentric.y
        + Vertices[instanceData.vertexOffset + triIndices.z].texcoord0 * barycentric.z;
    uint materialIndex = instanceData.materialIndex;
    Material material = Materials[materialIndex];
    float3 geometricNormal = normalize(mul(localNormal, (float3x3)WorldToObject3x4()));
    bool frontFace = HitKind() == HIT_KIND_TRIANGLE_FRONT_FACE;
    float3 normal = frontFace ? geometricNormal : -geometricNormal;
    float4 baseColor = material.baseColorFactor
        * SampleMaterialTexture(material.baseColorTextureAndSampler, texcoord0);
    float4 metallicRoughness = SampleMaterialTexture(
        material.metallicRoughnessTextureAndSampler,
        texcoord0);
    float3 emissive = material.emissiveFactor
        * SampleMaterialTexture(material.emissiveTextureAndSampler, texcoord0).xyz;
    float metallic = saturate(material.metallicFactor * metallicRoughness.b);
    float roughness = saturate(material.roughnessFactor * metallicRoughness.g);
    uint kind = (material.flags & 4u) != 0u
        ? 2u
        : (any(emissive > 0.0) ? 3u : (metallic > 0.5 ? 1u : 0u));
    float3 hitPosition = mul(ObjectToWorld3x4(), float4(localPosition, 1.0));
    float3 previousHitPosition = PreviousWorldPosition(localPosition, instanceData);
    payload.hitDistance = RayTCurrent();

    if (payload.depth == 0)
    {
        uint2 pixel = DispatchRaysIndex().xy;
        uint2 size = DispatchRaysDimensions().xy;
        payload.firstKind = kind;
        GBufferAlbedo[pixel] = float4(baseColor.xyz, float(kind));
        GBufferNormalRoughness[pixel] = float4(normal * 0.5 + 0.5, roughness);
        GBufferDepth[pixel] = RayTCurrent();
        GBufferId[pixel] = instanceData.stableSurfaceId;
        GBufferWorldPosition[pixel] = float4(hitPosition, 1.0);
        float2 currentUv = (float2(pixel) + 0.5) / float2(size);
        float2 previousUv = ProjectToPreviousUv(previousHitPosition, size);
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
        payload.radiance = emissive * weight;
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
            directLighting = shadow.radiance * baseColor.xyz * Materials[3].emissiveFactor
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
    payload.radiance = directLighting + baseColor.xyz * child.radiance;
}
