use crate::realtime::AccelerationStructureMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccelerationStructurePolicy {
    pub compact_blas: bool,
    pub tlas_update_enabled: bool,
    pub blas_build_flags: u32,
    pub tlas_build_flags: u32,
}

const ALLOW_UPDATE: u32 = 1;
const ALLOW_COMPACTION: u32 = 2;
const PREFER_FAST_TRACE: u32 = 4;

pub fn acceleration_structure_policy(
    mode: AccelerationStructureMode,
    animate_model: bool,
    has_animation_groups: bool,
) -> AccelerationStructurePolicy {
    match mode {
        AccelerationStructureMode::Baseline => AccelerationStructurePolicy {
            compact_blas: false,
            tlas_update_enabled: true,
            blas_build_flags: PREFER_FAST_TRACE,
            tlas_build_flags: ALLOW_UPDATE | PREFER_FAST_TRACE,
        },
        AccelerationStructureMode::Optimized => AccelerationStructurePolicy {
            compact_blas: true,
            tlas_update_enabled: animate_model && has_animation_groups,
            blas_build_flags: PREFER_FAST_TRACE | ALLOW_COMPACTION,
            tlas_build_flags: if animate_model && has_animation_groups {
                ALLOW_UPDATE | PREFER_FAST_TRACE
            } else {
                PREFER_FAST_TRACE
            },
        },
    }
}

pub fn update_flags(base_flags: u32) -> u32 {
    base_flags | 32
}

pub fn required_scratch_size(
    blas_build_scratch: impl IntoIterator<Item = u64>,
    tlas_build_scratch: u64,
    tlas_update_scratch: Option<u64>,
) -> u64 {
    blas_build_scratch
        .into_iter()
        .chain([tlas_build_scratch])
        .chain(tlas_update_scratch)
        .max()
        .unwrap_or(0)
}

pub const RAYTRACING_AS_ALIGNMENT: u64 = 256;

pub fn align_acceleration_structure_size(value: u64) -> Option<u64> {
    let remainder = value % RAYTRACING_AS_ALIGNMENT;
    if remainder == 0 {
        Some(value)
    } else {
        value.checked_add(RAYTRACING_AS_ALIGNMENT - remainder)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionDecision {
    Compacted,
    InvalidSize,
    NoAllocationSaving,
    Disabled,
}

impl CompactionDecision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compacted => "compacted",
            Self::InvalidSize => "invalid_size",
            Self::NoAllocationSaving => "no_allocation_saving",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompactionEvaluation {
    pub decision: CompactionDecision,
    pub aligned_compacted_bytes: Option<u64>,
}

pub fn evaluate_compaction(
    enabled: bool,
    original_result_bytes: u64,
    reported_compacted_bytes: u64,
    original_allocation_bytes: u64,
    candidate_allocation_bytes: u64,
) -> CompactionEvaluation {
    if !enabled {
        return CompactionEvaluation {
            decision: CompactionDecision::Disabled,
            aligned_compacted_bytes: None,
        };
    }
    let Some(aligned_compacted_bytes) = align_acceleration_structure_size(reported_compacted_bytes)
    else {
        return CompactionEvaluation {
            decision: CompactionDecision::InvalidSize,
            aligned_compacted_bytes: None,
        };
    };
    if reported_compacted_bytes == 0
        || reported_compacted_bytes > original_result_bytes
        || original_allocation_bytes == 0
        || candidate_allocation_bytes == 0
    {
        return CompactionEvaluation {
            decision: CompactionDecision::InvalidSize,
            aligned_compacted_bytes: Some(aligned_compacted_bytes),
        };
    }
    let decision = if candidate_allocation_bytes < original_allocation_bytes {
        CompactionDecision::Compacted
    } else {
        CompactionDecision::NoAllocationSaving
    };
    CompactionEvaluation {
        decision,
        aligned_compacted_bytes: Some(aligned_compacted_bytes),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlasAllocationRecord {
    pub primitive_index: usize,
    pub original_result_bytes: u64,
    pub reported_compacted_bytes: u64,
    pub original_allocation_bytes: u64,
    pub candidate_allocation_bytes: u64,
    pub final_result_bytes: u64,
    pub final_allocation_bytes: u64,
    pub decision: CompactionDecision,
}

pub fn final_blas_primitive_order(records: &[BlasAllocationRecord]) -> Vec<usize> {
    records
        .iter()
        .map(|record| record.primitive_index)
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccelerationStructureStats {
    pub mode: AccelerationStructureMode,
    pub tlas_update_enabled: bool,
    pub blas_count: usize,
    pub compacted_blas_count: usize,
    pub invalid_compacted_size_count: usize,
    pub no_allocation_saving_count: usize,
    pub disabled_blas_count: usize,
    pub original_result_bytes: u64,
    pub final_result_bytes: u64,
    pub original_allocation_bytes: u64,
    pub final_allocation_bytes: u64,
    pub allocation_bytes_saved: u64,
    pub allocation_saving_ratio: Option<f64>,
    pub tlas_result_bytes: u64,
    pub tlas_allocation_bytes: u64,
    pub retained_update_scratch_required_bytes: u64,
    pub retained_update_scratch_allocation_bytes: u64,
    pub blas: Vec<BlasAllocationRecord>,
}

impl AccelerationStructureStats {
    pub fn from_records(
        mode: AccelerationStructureMode,
        tlas_update_enabled: bool,
        records: Vec<BlasAllocationRecord>,
        tlas_result_bytes: u64,
        tlas_allocation_bytes: u64,
        retained_update_scratch_required_bytes: u64,
        retained_update_scratch_allocation_bytes: u64,
    ) -> Self {
        let mut stats = Self {
            mode,
            tlas_update_enabled,
            blas_count: records.len(),
            compacted_blas_count: 0,
            invalid_compacted_size_count: 0,
            no_allocation_saving_count: 0,
            disabled_blas_count: 0,
            original_result_bytes: 0,
            final_result_bytes: 0,
            original_allocation_bytes: 0,
            final_allocation_bytes: 0,
            allocation_bytes_saved: 0,
            allocation_saving_ratio: None,
            tlas_result_bytes,
            tlas_allocation_bytes,
            retained_update_scratch_required_bytes,
            retained_update_scratch_allocation_bytes,
            blas: records,
        };
        for record in &stats.blas {
            stats.original_result_bytes = stats
                .original_result_bytes
                .saturating_add(record.original_result_bytes);
            stats.final_result_bytes = stats
                .final_result_bytes
                .saturating_add(record.final_result_bytes);
            stats.original_allocation_bytes = stats
                .original_allocation_bytes
                .saturating_add(record.original_allocation_bytes);
            stats.final_allocation_bytes = stats
                .final_allocation_bytes
                .saturating_add(record.final_allocation_bytes);
            match record.decision {
                CompactionDecision::Compacted => {
                    stats.compacted_blas_count = stats.compacted_blas_count.saturating_add(1)
                }
                CompactionDecision::InvalidSize => {
                    stats.invalid_compacted_size_count =
                        stats.invalid_compacted_size_count.saturating_add(1)
                }
                CompactionDecision::NoAllocationSaving => {
                    stats.no_allocation_saving_count =
                        stats.no_allocation_saving_count.saturating_add(1)
                }
                CompactionDecision::Disabled => {
                    stats.disabled_blas_count = stats.disabled_blas_count.saturating_add(1)
                }
            }
        }
        stats.allocation_bytes_saved = stats
            .original_allocation_bytes
            .saturating_sub(stats.final_allocation_bytes);
        if stats.original_allocation_bytes != 0 {
            let ratio =
                stats.allocation_bytes_saved as f64 / stats.original_allocation_bytes as f64;
            if ratio.is_finite() {
                stats.allocation_saving_ratio = Some(ratio);
            }
        }
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_matrix_preserves_baseline_and_limits_optimized_updates() {
        let baseline =
            acceleration_structure_policy(AccelerationStructureMode::Baseline, false, false);
        assert!(!baseline.compact_blas);
        assert!(baseline.tlas_update_enabled);
        assert_eq!(baseline.blas_build_flags, 4);
        assert_eq!(baseline.tlas_build_flags, 5);

        let optimized_static =
            acceleration_structure_policy(AccelerationStructureMode::Optimized, true, false);
        assert!(optimized_static.compact_blas);
        assert!(!optimized_static.tlas_update_enabled);
        assert_eq!(optimized_static.blas_build_flags, 6);
        assert_eq!(optimized_static.tlas_build_flags, 4);

        let optimized_dynamic =
            acceleration_structure_policy(AccelerationStructureMode::Optimized, true, true);
        assert!(optimized_dynamic.tlas_update_enabled);
        assert_eq!(optimized_dynamic.tlas_build_flags, 5);
    }

    #[test]
    fn update_flags_only_add_perform_update() {
        assert_eq!(update_flags(4), 36);
    }

    #[test]
    fn scratch_uses_update_only_for_dynamic_tlas() {
        assert_eq!(required_scratch_size([10, 20], 15, None), 20);
        assert_eq!(required_scratch_size([10, 20], 15, Some(30)), 30);
    }

    #[test]
    fn alignment_handles_boundaries_and_overflow() {
        assert_eq!(align_acceleration_structure_size(0), Some(0));
        assert_eq!(align_acceleration_structure_size(256), Some(256));
        assert_eq!(align_acceleration_structure_size(257), Some(512));
        assert_eq!(align_acceleration_structure_size(u64::MAX), None);
    }

    #[test]
    fn compaction_requires_real_allocation_saving() {
        assert_eq!(
            evaluate_compaction(true, 1024, 513, 1024, 1024).decision,
            CompactionDecision::NoAllocationSaving
        );
        assert_eq!(
            evaluate_compaction(true, 1024, 512, 1024, 512).decision,
            CompactionDecision::Compacted
        );
        assert_eq!(
            evaluate_compaction(true, 1024, 0, 1024, 512).decision,
            CompactionDecision::InvalidSize
        );
    }

    #[test]
    fn aggregate_preserves_order_and_reports_ratio() {
        let stats = AccelerationStructureStats::from_records(
            AccelerationStructureMode::Optimized,
            false,
            vec![
                BlasAllocationRecord {
                    primitive_index: 0,
                    original_result_bytes: 1024,
                    reported_compacted_bytes: 512,
                    original_allocation_bytes: 1024,
                    candidate_allocation_bytes: 512,
                    final_result_bytes: 512,
                    final_allocation_bytes: 512,
                    decision: CompactionDecision::Compacted,
                },
                BlasAllocationRecord {
                    primitive_index: 1,
                    original_result_bytes: 1024,
                    reported_compacted_bytes: 700,
                    original_allocation_bytes: 1024,
                    candidate_allocation_bytes: 1024,
                    final_result_bytes: 1024,
                    final_allocation_bytes: 1024,
                    decision: CompactionDecision::NoAllocationSaving,
                },
            ],
            256,
            256,
            0,
            0,
        );
        assert_eq!(stats.compacted_blas_count, 1);
        assert_eq!(stats.original_result_bytes, 2048);
        assert_eq!(stats.final_allocation_bytes, 1536);
        assert_eq!(stats.allocation_bytes_saved, 512);
        assert_eq!(stats.allocation_saving_ratio, Some(0.25));
        assert_eq!(stats.blas[0].primitive_index, 0);
        assert_eq!(stats.blas[1].primitive_index, 1);
    }

    #[test]
    fn mixed_compaction_keeps_final_blas_primitive_order() {
        let records = [
            BlasAllocationRecord {
                primitive_index: 0,
                original_result_bytes: 1024,
                reported_compacted_bytes: 512,
                original_allocation_bytes: 1024,
                candidate_allocation_bytes: 512,
                final_result_bytes: 512,
                final_allocation_bytes: 512,
                decision: CompactionDecision::Compacted,
            },
            BlasAllocationRecord {
                primitive_index: 1,
                original_result_bytes: 1024,
                reported_compacted_bytes: 0,
                original_allocation_bytes: 1024,
                candidate_allocation_bytes: 0,
                final_result_bytes: 1024,
                final_allocation_bytes: 1024,
                decision: CompactionDecision::InvalidSize,
            },
        ];
        assert_eq!(final_blas_primitive_order(&records), vec![0, 1]);
    }
}
