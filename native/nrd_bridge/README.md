# NRD C ABI bridge

This directory contains the stable C ABI boundary and the optional NVIDIA NRD
v4.17.3 bridge. CMake consumes only the locally fetched, locked NRD/NRI/
MathLib/ShaderMake/D3D12MemoryAllocator sources. Rust owns the opaque handle
and the D3D12 command-list lifecycle; the C++ implementation does not close or
execute command lists, signal fences, wait for the GPU, or retain per-frame
resource pointers.

The header mirrors `src/reconstruction.rs`. `NrdBridgeCreateDesc` requires
three queued frames, and all public functions use fixed-width POD values or an
opaque handle. Exceptions, STL containers, allocators, and COM ownership stay
inside the C++ implementation. `nrd_bridge_smoke` exercises version query and
create/destroy against a real D3D12 device without submitting GPU work.
