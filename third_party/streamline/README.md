# NVIDIA Streamline v2.12.0 lock

Streamline is an optional local SDK input. The repository does not redistribute
the release ZIP, SDK tree, DLLs, or generated binaries. Run
`scripts/fetch_streamline.ps1` explicitly on a machine with network access; the
script verifies the official release archive, complete tag commit, required
headers/import library, production/development binaries, and licenses before
moving the SDK into ignored `external/streamline-v2.12.0/`.

The lock records the archive and file hashes observed from the official
`v2.12.0` release on 2026-08-10. A changed or incomplete local SDK is never
overwritten automatically. `build.rs` only reads this directory when the
`streamline` feature is enabled and never performs network access.

Release builds use the SDK's production `bin/x64` files. Debug builds use only
the matching `bin/x64/development` files. The Streamline, DLSS, Reflex, PCL,
and NVIDIA DLSS plugin licenses remain beside the executable for local review.
