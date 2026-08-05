cbuffer FrameConstants : register(b0)
{
    float elapsed_seconds;
    uint output_width;
    uint output_height;
    uint frame_index;
};

RWTexture2D<float4> output_texture : register(u0);

[numthreads(8, 8, 1)]
void main(uint3 dispatch_id : SV_DispatchThreadID)
{
    if (dispatch_id.x >= output_width || dispatch_id.y >= output_height)
    {
        return;
    }

    float2 uv = (float2(dispatch_id.xy) + 0.5) / float2(output_width, output_height);
    float pulse = 0.5 + 0.5 * sin(elapsed_seconds * 1.5 + uv.x * 6.2831853);
    float3 color = float3(uv.x, uv.y, 0.15 + 0.7 * pulse);
    output_texture[dispatch_id.xy] = float4(color, 1.0);
}
