#ifndef STAGE6_ATROUS_BINDINGS_HLSLI
#define STAGE6_ATROUS_BINDINGS_HLSLI

Texture2D<float4> DiffuseInput : register(t0);
Texture2D<float4> SpecularInput : register(t1);
Texture2D<float4> NormalRoughness : register(t2);
Texture2D<float> Depth : register(t3);
Texture2D<float4> Moments : register(t4);
Texture2D<uint> Id : register(t5);
Texture2D<uint2> HistoryLength : register(t6);
Texture2D<float> HitDistance : register(t7);
RWTexture2D<float4> DiffuseOutput : register(u0);
RWTexture2D<float4> SpecularOutput : register(u1);

cbuffer AtrousConstants : register(b0)
{
    uint StepWidth;
    uint Iteration;
};

float Luminance(float3 color)
{
    return dot(color, float3(0.2126, 0.7152, 0.0722));
}

float KernelWeight(int offset)
{
    offset = abs(offset);
    return offset == 0 ? 6.0 : (offset == 1 ? 4.0 : 1.0);
}

#endif
