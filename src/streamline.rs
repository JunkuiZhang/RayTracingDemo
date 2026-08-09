//! Feature-gated Streamline metadata. The D3D12 bridge is added in Stage 10B.

pub const SDK_VERSION: &str = "2.12.0";

#[cfg(test)]
mod tests {
    use super::SDK_VERSION;

    #[test]
    fn locks_streamline_version() {
        assert_eq!(SDK_VERSION, "2.12.0");
    }
}
