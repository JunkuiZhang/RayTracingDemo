//! Stable debug-view names shared by CLI, F1 and the ToneMap root constant.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum DebugView {
    #[default]
    Final = 0,
    Raw = 1,
    Albedo = 2,
    NormalRoughness = 3,
    Depth = 4,
    Motion = 5,
    Variance = 6,
    HistoryRejection = 7,
    HistoryLength = 8,
    ObjectMaterialId = 9,
    SpecularHitDistance = 10,
    NrdValidation = 11,
}

impl DebugView {
    pub const ALL: [Self; 12] = [
        Self::Final,
        Self::Raw,
        Self::Albedo,
        Self::NormalRoughness,
        Self::Depth,
        Self::Motion,
        Self::Variance,
        Self::HistoryRejection,
        Self::HistoryLength,
        Self::ObjectMaterialId,
        Self::SpecularHitDistance,
        Self::NrdValidation,
    ];

    pub const fn from_index(index: u32) -> Option<Self> {
        match index {
            0 => Some(Self::Final),
            1 => Some(Self::Raw),
            2 => Some(Self::Albedo),
            3 => Some(Self::NormalRoughness),
            4 => Some(Self::Depth),
            5 => Some(Self::Motion),
            6 => Some(Self::Variance),
            7 => Some(Self::HistoryRejection),
            8 => Some(Self::HistoryLength),
            9 => Some(Self::ObjectMaterialId),
            10 => Some(Self::SpecularHitDistance),
            11 => Some(Self::NrdValidation),
            _ => None,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "final" => Some(Self::Final),
            "raw" => Some(Self::Raw),
            "albedo" => Some(Self::Albedo),
            "normal-roughness" => Some(Self::NormalRoughness),
            "depth" => Some(Self::Depth),
            "motion" => Some(Self::Motion),
            "variance" => Some(Self::Variance),
            "history-rejection" => Some(Self::HistoryRejection),
            "history-length" => Some(Self::HistoryLength),
            "object-material-id" => Some(Self::ObjectMaterialId),
            "specular-hit-distance" => Some(Self::SpecularHitDistance),
            "nrd-validation" => Some(Self::NrdValidation),
            _ => None,
        }
    }

    pub const fn index(self) -> u32 {
        self as u32
    }

    pub const fn hlsl_value(self) -> u32 {
        self.index()
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Final => "final",
            Self::Raw => "raw",
            Self::Albedo => "albedo",
            Self::NormalRoughness => "normal-roughness",
            Self::Depth => "depth",
            Self::Motion => "motion",
            Self::Variance => "variance",
            Self::HistoryRejection => "history-rejection",
            Self::HistoryLength => "history-length",
            Self::ObjectMaterialId => "object-material-id",
            Self::SpecularHitDistance => "specular-hit-distance",
            Self::NrdValidation => "nrd-validation",
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Final => "最终",
            Self::Raw => "原始 1 SPP",
            Self::Albedo => "反照率",
            Self::NormalRoughness => "法线/粗糙度",
            Self::Depth => "深度",
            Self::Motion => "运动矢量",
            Self::Variance => "方差",
            Self::HistoryRejection => "历史拒绝",
            Self::HistoryLength => "历史长度",
            Self::ObjectMaterialId => "物体/材质 ID",
            Self::SpecularHitDistance => "镜面命中距离",
            Self::NrdValidation => "NRD guide validation",
        }
    }

    pub const fn next(self) -> Self {
        Self::from_index((self.index() + 1) % Self::ALL.len() as u32)
            .expect("debug view index is always within the fixed list")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_names_indices_and_hlsl_values_round_trip() {
        for (index, view) in DebugView::ALL.iter().copied().enumerate() {
            let index = index as u32;
            assert_eq!(view.index(), index);
            assert_eq!(view.hlsl_value(), index);
            assert_eq!(DebugView::from_index(index), Some(view));
            assert_eq!(DebugView::from_name(view.name()), Some(view));
            assert!(!view.title().is_empty());
        }
        assert_eq!(DebugView::from_index(12), None);
        assert_eq!(DebugView::from_name("unknown"), None);
    }

    #[test]
    fn next_follows_f1_order_and_wraps_to_final() {
        let mut view = DebugView::Final;
        for expected in DebugView::ALL.iter().copied().skip(1) {
            view = view.next();
            assert_eq!(view, expected);
        }
        assert_eq!(view.next(), DebugView::Final);
    }
}
