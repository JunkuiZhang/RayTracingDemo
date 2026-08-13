#ifndef STAGE11_SCENE_HLSLI
#define STAGE11_SCENE_HLSLI

// Shared StructuredBuffer layouts. Rust locks these strides in scene tests.
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

#endif
