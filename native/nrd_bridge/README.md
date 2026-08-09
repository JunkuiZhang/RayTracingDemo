# NRD C ABI bridge

This directory contains the stable C ABI boundary for the optional NVIDIA NRD
backend. The implementation is intentionally not present until the locally
fetched NRD v4.17.3 source has been verified. Rust owns the opaque handle and
the D3D12 command-list lifecycle; the future C++ implementation must not close
or execute command lists, signal fences, wait for the GPU, or retain per-frame
resource pointers.

The header mirrors `src/reconstruction.rs`. `NrdBridgeCreateDesc` requires
three queued frames, and all public functions use fixed-width POD values or an
opaque handle. Exceptions, STL containers, allocators, and COM ownership must
remain inside the C++ implementation.
