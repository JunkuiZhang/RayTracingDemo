//! Fixed output/internal render extent calculations used by the realtime renderer.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent2D {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct RenderScale(f32);

impl PartialEq for RenderScale {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for RenderScale {}

impl Default for RenderScale {
    fn default() -> Self {
        Self::NATIVE
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderScaleError {
    NotFinite,
    BelowMinimum,
    AboveMaximum,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderExtentChange {
    DeferredWhileMinimized,
    QuantizedNoop,
    Recreate(Extent2D),
}

impl RenderScale {
    pub const NATIVE: Self = Self(1.0);
    pub const MIN: f32 = 0.5;
    pub const MAX: f32 = 1.0;

    pub fn new(value: f32) -> Result<Self, RenderScaleError> {
        if !value.is_finite() {
            return Err(RenderScaleError::NotFinite);
        }
        if value < Self::MIN {
            return Err(RenderScaleError::BelowMinimum);
        }
        if value > Self::MAX {
            return Err(RenderScaleError::AboveMaximum);
        }
        Ok(Self(value))
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

pub fn render_extent(output: Extent2D, scale: RenderScale) -> Extent2D {
    if scale == RenderScale::NATIVE {
        return output;
    }

    Extent2D {
        width: quantize_dimension(output.width, scale.get()),
        height: quantize_dimension(output.height, scale.get()),
    }
}

pub fn classify_render_extent_change(
    minimized: bool,
    output: Extent2D,
    current: Extent2D,
    requested: RenderScale,
) -> RenderExtentChange {
    if minimized {
        return RenderExtentChange::DeferredWhileMinimized;
    }
    let requested_extent = render_extent(output, requested);
    if requested_extent == current {
        RenderExtentChange::QuantizedNoop
    } else {
        RenderExtentChange::Recreate(requested_extent)
    }
}

fn quantize_dimension(output: u32, scale: f32) -> u32 {
    let scaled = (f64::from(output) * f64::from(scale)) as u64;
    let quantized = (scaled / 8) * 8;
    quantized.clamp(8, u64::from(output)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_finite_scales_in_the_fixed_range() {
        assert_eq!(RenderScale::new(0.5).unwrap().get(), 0.5);
        assert_eq!(RenderScale::new(1.0).unwrap().get(), 1.0);
        assert_eq!(RenderScale::new(f32::NAN), Err(RenderScaleError::NotFinite));
        assert_eq!(
            RenderScale::new(f32::INFINITY),
            Err(RenderScaleError::NotFinite)
        );
        assert_eq!(
            RenderScale::new(f32::NEG_INFINITY),
            Err(RenderScaleError::NotFinite)
        );
        assert_eq!(RenderScale::new(0.49), Err(RenderScaleError::BelowMinimum));
        assert_eq!(RenderScale::new(1.01), Err(RenderScaleError::AboveMaximum));
    }

    #[test]
    fn native_scale_preserves_every_output_dimension_exactly() {
        assert_eq!(
            render_extent(
                Extent2D {
                    width: 1920,
                    height: 1080,
                },
                RenderScale::NATIVE,
            ),
            Extent2D {
                width: 1920,
                height: 1080,
            }
        );
        assert_eq!(
            render_extent(
                Extent2D {
                    width: 321,
                    height: 181,
                },
                RenderScale::NATIVE,
            ),
            Extent2D {
                width: 321,
                height: 181,
            }
        );
    }

    #[test]
    fn reduced_scales_follow_the_eight_pixel_quantization_rule() {
        let output = Extent2D {
            width: 1920,
            height: 1080,
        };
        for (scale, expected) in [
            (0.83, (1592, 896)),
            (0.75, (1440, 808)),
            (0.67, (1280, 720)),
        ] {
            assert_eq!(
                render_extent(output, RenderScale::new(scale).unwrap()),
                Extent2D {
                    width: expected.0,
                    height: expected.1,
                }
            );
        }
        assert_eq!(
            render_extent(
                Extent2D {
                    width: 1280,
                    height: 720,
                },
                RenderScale::new(0.67).unwrap(),
            ),
            Extent2D {
                width: 856,
                height: 480,
            }
        );
    }

    #[test]
    fn reduced_extents_are_nonzero_monotonic_and_not_larger_than_output() {
        let output = Extent2D {
            width: 321,
            height: 181,
        };
        let mut previous = output;
        for value in [0.99, 0.83, 0.75, 0.67, 0.5] {
            let extent = render_extent(output, RenderScale::new(value).unwrap());
            assert!(extent.width >= 8 && extent.height >= 8);
            assert!(extent.width <= output.width && extent.height <= output.height);
            assert!(extent.width <= previous.width && extent.height <= previous.height);
            previous = extent;
        }
    }

    #[test]
    fn equal_quantized_extents_are_identical() {
        let output = Extent2D {
            width: 1920,
            height: 1080,
        };
        assert_eq!(
            render_extent(output, RenderScale::new(0.7501).unwrap()),
            render_extent(output, RenderScale::new(0.75).unwrap())
        );
    }

    #[test]
    fn extent_changes_are_deferred_while_minimized() {
        let output = Extent2D {
            width: 1920,
            height: 1080,
        };
        assert_eq!(
            classify_render_extent_change(true, output, output, RenderScale::new(0.67).unwrap(),),
            RenderExtentChange::DeferredWhileMinimized
        );
        assert_eq!(
            classify_render_extent_change(false, output, output, RenderScale::NATIVE),
            RenderExtentChange::QuantizedNoop
        );
        assert_eq!(
            classify_render_extent_change(false, output, output, RenderScale::new(0.67).unwrap(),),
            RenderExtentChange::Recreate(Extent2D {
                width: 1280,
                height: 720,
            })
        );
    }
}
