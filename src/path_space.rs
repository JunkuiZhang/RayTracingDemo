//! Shared CPU-side reference for the path-space identity and nested-medium ABI.
//!
//! The renderer will eventually execute this contract in HLSL.  Keeping a tiny
//! Rust model gives us deterministic tests for the bit packing before those
//! values become persistent temporal-history identities on the GPU.

use crate::reconstruction::DenoiserBackend;

pub(crate) const STABLE_PLANE_COUNT: usize = 3;
pub(crate) const STABLE_BRANCH_ROOT: u32 = 1;
pub(crate) const STABLE_BRANCH_JUST_STARTED: u32 = 0;
pub(crate) const STABLE_BRANCH_ENQUEUED: u32 = u32::MAX - 1;
pub(crate) const STABLE_BRANCH_INVALID: u32 = u32::MAX;
pub(crate) const MAX_STABLE_DELTA_VERTICES: u32 = 15;
pub(crate) const DELTA_LOBE_COUNT: u32 = 4;
pub(crate) const STABLE_PLANE_RECORD_STRIDE: usize = 64;
pub(crate) const STABLE_PLANE_COUNTER_COUNT: usize = 12;
pub(crate) const STABLE_PLANE_COUNTER_NAMES: [&str; STABLE_PLANE_COUNTER_COUNT] = [
    "pixels_traced",
    "active_plane_slots",
    "plane_count_0",
    "plane_count_1",
    "plane_count_2",
    "plane_count_3",
    "plane_overflow_pixels",
    "branch_queue_overflow_events",
    "interior_overflow_events",
    "false_intersection_rejections",
    "total_internal_reflection_events",
    "invalid_medium_exit_events",
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct StablePlaneCounterSnapshot {
    pub(crate) generation_id: u64,
    pub(crate) extent: [u32; 2],
    pub(crate) values: [u32; STABLE_PLANE_COUNTER_COUNT],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StablePlaneCounterTelemetry {
    pub(crate) completed_frames: u64,
    sums: [u64; STABLE_PLANE_COUNTER_COUNT],
    pub(crate) last: Option<StablePlaneCounterSnapshot>,
}

impl StablePlaneCounterTelemetry {
    /// Accept only a fence-complete, same-generation sample. The renderer
    /// supplies the epoch and extent because a counter from an old generation
    /// is numerically plausible but semantically unrelated to the current UI.
    pub(crate) fn accept(
        &mut self,
        snapshot: StablePlaneCounterSnapshot,
        expected_generation_id: u64,
        expected_extent: [u32; 2],
        sample_valid: bool,
    ) -> bool {
        if !sample_valid
            || snapshot.generation_id != expected_generation_id
            || snapshot.extent != expected_extent
        {
            return false;
        }
        self.completed_frames = self.completed_frames.saturating_add(1);
        for (sum, value) in self.sums.iter_mut().zip(snapshot.values) {
            *sum = sum.saturating_add(u64::from(value));
        }
        self.last = Some(snapshot);
        true
    }

    pub(crate) fn per_frame_mean(&self, index: usize) -> Option<f64> {
        (index < STABLE_PLANE_COUNTER_COUNT && self.completed_frames > 0)
            .then(|| self.sums[index] as f64 / self.completed_frames as f64)
    }

    pub(crate) fn sum(&self, index: usize) -> Option<u64> {
        (index < STABLE_PLANE_COUNTER_COUNT).then(|| self.sums[index])
    }

    /// Plane slots normalized by traced pixels, not by frame count. This is
    /// the only active-plane metric whose valid range is [0, 3].
    pub(crate) fn active_planes_mean(&self) -> Option<f64> {
        let pixels = self.sums[0];
        (pixels > 0).then(|| self.sums[1] as f64 / pixels as f64)
    }

    pub(crate) fn plane_count_histogram(&self) -> [u64; 4] {
        [self.sums[2], self.sums[3], self.sums[4], self.sums[5]]
    }
}

/// User-facing policy. `Auto` is resolved only at generation boundaries and
/// must never become a shader/runtime resource branch.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PathSpaceMode {
    #[default]
    Auto,
    Legacy,
    StablePlanes,
}

impl PathSpaceMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Legacy => "legacy",
            Self::StablePlanes => "stable-planes",
        }
    }
}

/// Concrete path-space implementation owned by an active render generation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ActivePathSpace {
    #[default]
    Legacy,
    StablePlanes,
}

impl ActivePathSpace {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::StablePlanes => "stable-planes",
        }
    }

    pub(crate) const fn uses_stable_planes(self) -> bool {
        matches!(self, Self::StablePlanes)
    }
}

/// Resolve policy once for the destination denoiser. Explicit user choices
/// always win; automatic mode enables stable planes only for denoisers that
/// consume the layered path-space contract.
pub(crate) const fn resolve_path_space(
    requested: PathSpaceMode,
    denoiser: DenoiserBackend,
) -> ActivePathSpace {
    match requested {
        PathSpaceMode::Legacy => ActivePathSpace::Legacy,
        PathSpaceMode::StablePlanes => ActivePathSpace::StablePlanes,
        PathSpaceMode::Auto => match denoiser {
            DenoiserBackend::Svgf => ActivePathSpace::Legacy,
            DenoiserBackend::NrdReblur | DenoiserBackend::DlssRayReconstruction => {
                ActivePathSpace::StablePlanes
            }
        },
    }
}

/// Appends a two-bit delta-lobe identity to a stable branch.
///
/// Invalid and build-pass sentinel IDs are deliberately rejected: allowing a
/// sentinel to wrap into a plausible ID would join unrelated temporal history.
pub(crate) fn advance_stable_branch(branch_id: u32, lobe_id: u32) -> Option<u32> {
    if branch_vertex_index(branch_id)? >= MAX_STABLE_DELTA_VERTICES || lobe_id >= DELTA_LOBE_COUNT {
        return None;
    }
    branch_id.checked_shl(2).map(|id| id | lobe_id)
}

pub(crate) fn stable_branch_parent_lobe(branch_id: u32) -> Option<u32> {
    branch_vertex_index(branch_id).map(|_| branch_id & 0b11)
}

/// Returns one for the camera root, two after the first delta event, and so on.
pub(crate) fn branch_vertex_index(branch_id: u32) -> Option<u32> {
    if matches!(
        branch_id,
        STABLE_BRANCH_JUST_STARTED | STABLE_BRANCH_ENQUEUED | STABLE_BRANCH_INVALID
    ) {
        return None;
    }
    Some((u32::BITS - 1 - branch_id.leading_zeros()) / 2 + 1)
}

/// Tests whether `ancestor_id` is the prefix of `branch_id` at its own vertex.
pub(crate) fn stable_branch_has_ancestor(branch_id: u32, ancestor_id: u32) -> bool {
    let Some(branch_vertex) = branch_vertex_index(branch_id) else {
        return false;
    };
    let Some(ancestor_vertex) = branch_vertex_index(ancestor_id) else {
        return false;
    };
    ancestor_vertex <= branch_vertex
        && (branch_id >> ((branch_vertex - ancestor_vertex) * 2)) == ancestor_id
}

pub(crate) const INTERIOR_SLOT_COUNT: usize = 2;
const INTERIOR_MATERIAL_BITS: u32 = 28;
const INTERIOR_MATERIAL_MASK: u32 = (1 << INTERIOR_MATERIAL_BITS) - 1;
const INTERIOR_PRIORITY_SHIFT: u32 = INTERIOR_MATERIAL_BITS;
pub(crate) const MAX_NESTED_PRIORITY: u8 = 15;
pub(crate) const NO_INTERIOR_MATERIAL: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InteriorListError {
    MaterialIdOutOfRange,
    PriorityOutOfRange,
    Full,
    MaterialNotPresent,
}

/// Priority-sorted bounded medium state carried by a path.
///
/// Asset priority zero means "highest" and maps to the internal value 15, so
/// an all-zero slot remains an unambiguous empty value.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct InteriorList {
    slots: [u32; INTERIOR_SLOT_COUNT],
}

impl InteriorList {
    pub(crate) fn top_material(self) -> u32 {
        decode_slot(self.slots[0])
            .map(|(material_id, _)| material_id)
            .unwrap_or(NO_INTERIOR_MATERIAL)
    }

    pub(crate) fn next_material(self) -> u32 {
        decode_slot(self.slots[1])
            .map(|(material_id, _)| material_id)
            .unwrap_or(NO_INTERIOR_MATERIAL)
    }

    pub(crate) fn top_priority(self) -> u8 {
        decode_slot(self.slots[0])
            .map(|(_, priority)| priority)
            .unwrap_or(0)
    }

    /// Lower-priority nested surfaces are skipped until the active higher-
    /// priority boundary has been left. Asset priority zero always wins.
    pub(crate) fn is_true_intersection(self, asset_priority: u8) -> bool {
        asset_priority == 0 || asset_priority >= self.top_priority()
    }

    pub(crate) fn enter(
        &mut self,
        material_id: u32,
        asset_priority: u8,
    ) -> Result<(), InteriorListError> {
        let slot = encode_slot(material_id, asset_priority)?;
        let Some(empty) = self.slots.iter_mut().find(|entry| **entry == 0) else {
            return Err(InteriorListError::Full);
        };
        *empty = slot;
        // Slot ordering is also a deterministic tie-break for equal priorities.
        self.slots.sort_unstable_by(|left, right| right.cmp(left));
        Ok(())
    }

    pub(crate) fn exit(&mut self, material_id: u32) -> Result<(), InteriorListError> {
        let Some(slot) = self
            .slots
            .iter_mut()
            .find(|entry| decode_slot(**entry).is_some_and(|(id, _)| id == material_id))
        else {
            return Err(InteriorListError::MaterialNotPresent);
        };
        *slot = 0;
        self.slots.sort_unstable_by(|left, right| right.cmp(left));
        Ok(())
    }

    pub(crate) fn encoded_slots(self) -> [u32; INTERIOR_SLOT_COUNT] {
        self.slots
    }
}

fn encode_slot(material_id: u32, asset_priority: u8) -> Result<u32, InteriorListError> {
    if material_id > INTERIOR_MATERIAL_MASK {
        return Err(InteriorListError::MaterialIdOutOfRange);
    }
    if asset_priority > MAX_NESTED_PRIORITY {
        return Err(InteriorListError::PriorityOutOfRange);
    }
    let internal_priority = if asset_priority == 0 {
        MAX_NESTED_PRIORITY
    } else {
        asset_priority
    };
    Ok((u32::from(internal_priority) << INTERIOR_PRIORITY_SHIFT) | material_id)
}

fn decode_slot(slot: u32) -> Option<(u32, u8)> {
    (slot != 0).then_some((
        slot & INTERIOR_MATERIAL_MASK,
        (slot >> INTERIOR_PRIORITY_SHIFT) as u8,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFERENCE_QUEUE_CAPACITY: usize = 6;
    const REFERENCE_LOW_ENERGY: f32 = 1.0e-5;
    const REFERENCE_MAX_BUILD_STEPS: usize = 16;

    #[test]
    fn automatic_path_space_resolves_once_for_each_denoiser() {
        assert_eq!(
            resolve_path_space(PathSpaceMode::Auto, DenoiserBackend::Svgf),
            ActivePathSpace::Legacy
        );
        assert_eq!(
            resolve_path_space(PathSpaceMode::Auto, DenoiserBackend::NrdReblur),
            ActivePathSpace::StablePlanes
        );
        assert_eq!(
            resolve_path_space(
                PathSpaceMode::Auto,
                DenoiserBackend::DlssRayReconstruction
            ),
            ActivePathSpace::StablePlanes
        );
    }

    #[test]
    fn explicit_path_space_overrides_every_denoiser() {
        let denoisers = [
            DenoiserBackend::Svgf,
            DenoiserBackend::NrdReblur,
            DenoiserBackend::DlssRayReconstruction,
        ];
        for denoiser in denoisers {
            assert_eq!(
                resolve_path_space(PathSpaceMode::Legacy, denoiser),
                ActivePathSpace::Legacy
            );
            assert_eq!(
                resolve_path_space(PathSpaceMode::StablePlanes, denoiser),
                ActivePathSpace::StablePlanes
            );
        }
    }

    #[derive(Clone, Copy)]
    enum ReferenceJunction {
        Dielectric {
            fresnel: f32,
            transmission_valid: bool,
        },
        Mirror,
        Tir,
        Base,
    }

    #[derive(Clone, Copy)]
    struct ReferenceBranch {
        branch_id: u32,
        throughput: f32,
        next_junction: usize,
        primary: bool,
    }

    #[derive(Default)]
    struct ReferenceSchedulerResult {
        base_ids: Vec<u32>,
        primary_base_ids: Vec<u32>,
        primary_continuation_ids: Vec<u32>,
        forks_enqueued: usize,
        queue_overflow: u32,
    }

    fn enqueue_reference_branch(
        queue: &mut Vec<ReferenceBranch>,
        branch: ReferenceBranch,
        forks_enqueued: &mut usize,
        queue_overflow: &mut u32,
    ) -> bool {
        // This ordering mirrors the shader: a discarded low-energy lobe is an
        // intentional topology decision, not evidence of queue pressure.
        if branch.throughput < REFERENCE_LOW_ENERGY {
            return false;
        }
        if queue.len() >= REFERENCE_QUEUE_CAPACITY {
            *queue_overflow += 1;
            return false;
        }
        queue.push(branch);
        *forks_enqueued += 1;
        true
    }

    fn apply_dielectric_reference(
        current: ReferenceBranch,
        fresnel: f32,
        transmission_valid: bool,
        queue: &mut Vec<ReferenceBranch>,
        forks_enqueued: &mut usize,
        queue_overflow: &mut u32,
    ) -> ReferenceBranch {
        let reflected_id = advance_stable_branch(current.branch_id, 1)
            .expect("reference dielectric must have a reflection identity");
        let transmitted_id = advance_stable_branch(current.branch_id, 2);
        let fresnel = fresnel.clamp(0.0, 1.0);
        let mut reflected = ReferenceBranch {
            branch_id: reflected_id,
            throughput: current.throughput * fresnel,
            next_junction: current.next_junction + 1,
            primary: false,
        };
        if !transmission_valid || transmitted_id.is_none() {
            // Invalid medium state has no legal transmission. Reflection is
            // the sole deterministic fallback and remains the current path.
            reflected.primary = current.primary;
            return reflected;
        }

        let _ = enqueue_reference_branch(queue, reflected, forks_enqueued, queue_overflow);
        // The transmission lobe is deliberately not enqueued. Keeping it in
        // the current slot protects a nested medium's primary chain when the
        // bounded fork queue is full.
        ReferenceBranch {
            branch_id: transmitted_id.unwrap(),
            throughput: current.throughput * (1.0 - fresnel),
            next_junction: current.next_junction + 1,
            primary: current.primary,
        }
    }

    fn run_reference_scheduler(junctions: &[ReferenceJunction]) -> ReferenceSchedulerResult {
        let mut result = ReferenceSchedulerResult::default();
        let mut queue = vec![ReferenceBranch {
            branch_id: STABLE_BRANCH_ROOT,
            throughput: 1.0,
            next_junction: 0,
            primary: true,
        }];
        let mut head = 0;

        while head < queue.len() {
            let mut state = queue[head];
            head += 1;
            for _ in 0..REFERENCE_MAX_BUILD_STEPS {
                let Some(junction) = junctions.get(state.next_junction).copied() else {
                    break;
                };
                match junction {
                    ReferenceJunction::Base => {
                        result.base_ids.push(state.branch_id);
                        if state.primary {
                            result.primary_base_ids.push(state.branch_id);
                        }
                        break;
                    }
                    ReferenceJunction::Mirror | ReferenceJunction::Tir => {
                        state.branch_id = advance_stable_branch(state.branch_id, 1)
                            .expect("reference single-lobe path must have an identity");
                        state.next_junction += 1;
                        if state.primary {
                            result.primary_continuation_ids.push(state.branch_id);
                        }
                    }
                    ReferenceJunction::Dielectric {
                        fresnel,
                        transmission_valid,
                    } => {
                        state = apply_dielectric_reference(
                            state,
                            fresnel,
                            transmission_valid,
                            &mut queue,
                            &mut result.forks_enqueued,
                            &mut result.queue_overflow,
                        );
                        if state.primary {
                            result.primary_continuation_ids.push(state.branch_id);
                        }
                    }
                }
            }
        }
        result
    }

    #[test]
    fn stable_branch_ids_preserve_lobes_and_ancestry() {
        let reflection = advance_stable_branch(STABLE_BRANCH_ROOT, 1).unwrap();
        let transmission = advance_stable_branch(STABLE_BRANCH_ROOT, 2).unwrap();
        let internal_reflection = advance_stable_branch(transmission, 1).unwrap();

        assert_eq!(reflection, 0b0101);
        assert_eq!(transmission, 0b0110);
        assert_eq!(stable_branch_parent_lobe(internal_reflection), Some(1));
        assert_eq!(branch_vertex_index(STABLE_BRANCH_ROOT), Some(1));
        assert_eq!(branch_vertex_index(internal_reflection), Some(3));
        assert!(stable_branch_has_ancestor(
            internal_reflection,
            transmission
        ));
        assert!(stable_branch_has_ancestor(
            internal_reflection,
            STABLE_BRANCH_ROOT
        ));
        assert!(!stable_branch_has_ancestor(internal_reflection, reflection));
    }

    #[test]
    fn stable_branch_sentinels_and_bounds_are_rejected() {
        assert_eq!(branch_vertex_index(STABLE_BRANCH_INVALID), None);
        assert_eq!(branch_vertex_index(STABLE_BRANCH_ENQUEUED), None);
        assert_eq!(advance_stable_branch(STABLE_BRANCH_JUST_STARTED, 0), None);
        assert_eq!(
            advance_stable_branch(STABLE_BRANCH_ROOT, DELTA_LOBE_COUNT),
            None
        );

        let mut branch = STABLE_BRANCH_ROOT;
        for _ in 1..MAX_STABLE_DELTA_VERTICES {
            branch = advance_stable_branch(branch, 1).unwrap();
        }
        assert_eq!(branch_vertex_index(branch), Some(MAX_STABLE_DELTA_VERTICES));
        assert_eq!(advance_stable_branch(branch, 1), None);
    }

    #[test]
    fn interior_list_sorts_priorities_and_restores_outer_medium() {
        let mut list = InteriorList::default();
        list.enter(7, 3).unwrap();
        list.enter(11, 8).unwrap();

        assert_eq!(list.top_material(), 11);
        assert_eq!(list.next_material(), 7);
        assert_eq!(list.top_priority(), 8);
        assert!(!list.is_true_intersection(2));
        assert!(list.is_true_intersection(8));

        list.exit(11).unwrap();
        assert_eq!(list.top_material(), 7);
        assert_eq!(list.next_material(), NO_INTERIOR_MATERIAL);
    }

    #[test]
    fn asset_priority_zero_is_highest_without_colliding_with_empty_slot() {
        let mut list = InteriorList::default();
        list.enter(0, 0).unwrap();

        assert_eq!(list.top_material(), 0);
        assert_eq!(list.top_priority(), MAX_NESTED_PRIORITY);
        assert_ne!(list.encoded_slots()[0], 0);
        assert!(list.is_true_intersection(0));
        assert!(!list.is_true_intersection(14));
    }

    #[test]
    fn interior_list_reports_overflow_and_unbalanced_exit() {
        let mut list = InteriorList::default();
        list.enter(1, 1).unwrap();
        list.enter(2, 2).unwrap();
        assert_eq!(list.enter(3, 3), Err(InteriorListError::Full));
        assert_eq!(list.exit(99), Err(InteriorListError::MaterialNotPresent));
        assert_eq!(
            list.enter(INTERIOR_MATERIAL_MASK + 1, 1),
            Err(InteriorListError::MaterialIdOutOfRange)
        );
    }

    #[test]
    fn hlsl_contract_uses_the_same_bounds_and_failure_semantics() {
        let shader = include_str!("../shaders/stage11_path_space.hlsli");
        assert!(shader.contains("static const uint STABLE_PLANE_COUNT = 3u;"));
        assert!(shader.contains("static const uint MAX_STABLE_DELTA_VERTICES = 15u;"));
        assert!(shader.contains("static const uint INTERIOR_SLOT_COUNT = 2u;"));
        assert!(shader.contains("bool AdvanceStableBranch("));
        assert!(shader.contains("advancedBranchId = STABLE_BRANCH_INVALID;"));
        assert!(shader.contains("bool IsTrueIntersection(uint assetPriority)"));
        assert!(shader.contains("else\n            return false;"));
        assert_eq!(std::mem::size_of::<InteriorList>(), 8);
        assert_eq!(STABLE_PLANE_COUNT, 3);
        assert_eq!(STABLE_PLANE_RECORD_STRIDE, 64);
        assert!(shader.contains("struct StablePlaneRecord"));
        assert!(shader.contains("float4 data3;"));
        assert!(shader.contains("uint PackStableHdr(float3 value)"));
        assert!(shader.contains("float3 UnpackStableHdr(uint packed)"));
    }

    #[test]
    fn stable_counter_schema_and_accumulator_reject_stale_epochs() {
        assert_eq!(STABLE_PLANE_COUNTER_NAMES.len(), STABLE_PLANE_COUNTER_COUNT);
        let mut telemetry = StablePlaneCounterTelemetry::default();
        let snapshot = StablePlaneCounterSnapshot {
            generation_id: 7,
            extent: [320, 180],
            values: [1; STABLE_PLANE_COUNTER_COUNT],
        };
        assert!(!telemetry.accept(snapshot, 8, [320, 180], true));
        assert!(!telemetry.accept(snapshot, 7, [640, 360], true));
        assert!(!telemetry.accept(snapshot, 7, [320, 180], false));
        assert_eq!(telemetry.completed_frames, 0);
        assert!(telemetry.accept(snapshot, 7, [320, 180], true));
        assert_eq!(telemetry.completed_frames, 1);
        assert_eq!(telemetry.per_frame_mean(0), Some(1.0));
        assert_eq!(telemetry.per_frame_mean(STABLE_PLANE_COUNTER_COUNT), None);
        assert_eq!(telemetry.sum(0), Some(1));
        assert_eq!(telemetry.active_planes_mean(), Some(1.0));
        assert_eq!(telemetry.plane_count_histogram(), [1; 4]);
    }

    #[test]
    fn stable_counter_active_mean_uses_pixels_and_histogram_uses_final_buckets() {
        let mut telemetry = StablePlaneCounterTelemetry::default();
        let snapshot = StablePlaneCounterSnapshot {
            generation_id: 3,
            extent: [4, 2],
            // Eight pixels own a total of twelve active slots. Exactly one
            // final histogram bucket is populated per pixel.
            values: [8, 12, 1, 3, 3, 1, 0, 0, 0, 2, 0, 0],
        };
        assert!(telemetry.accept(snapshot, 3, [4, 2], true));
        assert_eq!(telemetry.active_planes_mean(), Some(1.5));
        assert_eq!(telemetry.plane_count_histogram(), [1, 3, 3, 1]);
        assert_eq!(telemetry.plane_count_histogram().iter().sum::<u64>(), 8);
    }

    #[test]
    fn stable_counter_contract_has_twelve_u32_slots_and_group_sync() {
        let shader = include_str!("../shaders/stage11_stable_plane_build.hlsl");
        assert_eq!(STABLE_PLANE_COUNTER_COUNT * std::mem::size_of::<u32>(), 48);
        assert!(shader.contains("groupshared uint StablePlaneGroupCounters[12]"));
        assert!(shader.contains("GroupMemoryBarrierWithGroupSync();"));
        assert!(shader.contains("if (inBounds)"));
        assert!(shader.contains("InterlockedAdd(StablePlaneCounters[linearThread]"));
        assert!(shader.contains("STABLE_COUNTER_PLANE_COUNT_0 + planeCount"));
    }

    #[test]
    fn nested_four_segment_transmission_reaches_base_without_fork_queue() {
        let junctions = [
            ReferenceJunction::Dielectric {
                fresnel: 0.04,
                transmission_valid: true,
            },
            ReferenceJunction::Dielectric {
                fresnel: 0.02,
                transmission_valid: true,
            },
            ReferenceJunction::Dielectric {
                fresnel: 0.03,
                transmission_valid: true,
            },
            ReferenceJunction::Dielectric {
                fresnel: 0.05,
                transmission_valid: true,
            },
            ReferenceJunction::Base,
        ];
        let result = run_reference_scheduler(&junctions);
        let mut expected = STABLE_BRANCH_ROOT;
        for _ in 0..4 {
            expected = advance_stable_branch(expected, 2).unwrap();
        }

        assert!(result.primary_base_ids.contains(&expected));
        assert_eq!(result.primary_continuation_ids[..4], [
            advance_stable_branch(STABLE_BRANCH_ROOT, 2).unwrap(),
            advance_stable_branch(advance_stable_branch(STABLE_BRANCH_ROOT, 2).unwrap(), 2)
                .unwrap(),
            advance_stable_branch(
                advance_stable_branch(
                    advance_stable_branch(STABLE_BRANCH_ROOT, 2).unwrap(),
                    2,
                )
                .unwrap(),
                2,
            )
            .unwrap(),
            expected,
        ]);
    }

    #[test]
    fn dielectric_adds_only_reflection_to_the_fork_queue() {
        let result = run_reference_scheduler(&[
            ReferenceJunction::Dielectric {
                fresnel: 0.2,
                transmission_valid: true,
            },
            ReferenceJunction::Base,
        ]);

        assert_eq!(result.forks_enqueued, 1);
        assert_eq!(result.queue_overflow, 0);
        assert_eq!(
            result.primary_continuation_ids,
            vec![advance_stable_branch(STABLE_BRANCH_ROOT, 2).unwrap()]
        );
    }

    #[test]
    fn mirror_and_tir_continue_in_place_without_forks() {
        for junction in [ReferenceJunction::Mirror, ReferenceJunction::Tir] {
            let result = run_reference_scheduler(&[junction, ReferenceJunction::Base]);
            assert_eq!(result.forks_enqueued, 0);
            assert_eq!(result.queue_overflow, 0);
            assert_eq!(
                result.primary_continuation_ids,
                vec![advance_stable_branch(STABLE_BRANCH_ROOT, 1).unwrap()]
            );
        }
    }

    #[test]
    fn invalid_transmission_keeps_reflection_as_current_continuation() {
        let result = run_reference_scheduler(&[
            ReferenceJunction::Dielectric {
                fresnel: 0.2,
                transmission_valid: false,
            },
            ReferenceJunction::Base,
        ]);
        let reflected = advance_stable_branch(STABLE_BRANCH_ROOT, 1).unwrap();

        assert_eq!(result.forks_enqueued, 0);
        assert_eq!(result.queue_overflow, 0);
        assert_eq!(result.primary_continuation_ids, vec![reflected]);
        assert_eq!(result.primary_base_ids, vec![reflected]);
    }

    #[test]
    fn low_energy_reflection_is_pruned_before_capacity_and_transmission_survives() {
        let result = run_reference_scheduler(&[
            ReferenceJunction::Dielectric {
                fresnel: 0.5e-5,
                transmission_valid: true,
            },
            ReferenceJunction::Base,
        ]);

        assert_eq!(result.forks_enqueued, 0);
        assert_eq!(result.queue_overflow, 0);
        assert_eq!(result.primary_base_ids.len(), 1);
    }

    #[test]
    fn full_fork_queue_rejects_only_reflection_and_keeps_current_transmission() {
        let mut queue = Vec::new();
        let mut forks_enqueued = 0;
        let mut queue_overflow = 0;
        for index in 0..REFERENCE_QUEUE_CAPACITY {
            assert!(enqueue_reference_branch(
                &mut queue,
                ReferenceBranch {
                    branch_id: STABLE_BRANCH_ROOT + index as u32,
                    throughput: 1.0,
                    next_junction: 0,
                    primary: false,
                },
                &mut forks_enqueued,
                &mut queue_overflow,
            ));
        }
        let current = apply_dielectric_reference(
            ReferenceBranch {
                branch_id: STABLE_BRANCH_ROOT,
                throughput: 1.0,
                next_junction: 0,
                primary: true,
            },
            0.2,
            true,
            &mut queue,
            &mut forks_enqueued,
            &mut queue_overflow,
        );

        assert_eq!(queue.len(), REFERENCE_QUEUE_CAPACITY);
        assert_eq!(queue_overflow, 1);
        assert_eq!(
            current.branch_id,
            advance_stable_branch(STABLE_BRANCH_ROOT, 2).unwrap()
        );
    }

    #[test]
    fn fresnel_weight_changes_do_not_change_primary_branch_identity() {
        let topology = |fresnel| {
            run_reference_scheduler(&[
                ReferenceJunction::Dielectric {
                    fresnel,
                    transmission_valid: true,
                },
                ReferenceJunction::Dielectric {
                    fresnel: 1.0 - fresnel,
                    transmission_valid: true,
                },
                ReferenceJunction::Base,
            ])
            .primary_continuation_ids
        };

        assert_eq!(topology(0.01), topology(0.99));
    }

    #[test]
    fn stable_build_contract_keeps_threshold_before_capacity_and_no_transmission_enqueue() {
        let shader = include_str!("../shaders/stage11_stable_plane_build.hlsl");
        let threshold = shader
            .find("AverageThroughput(branch.throughput) < 1.0e-5")
            .unwrap();
        let capacity = shader.find("tail >= MAX_BUILD_QUEUE").unwrap();
        assert!(threshold < capacity);
        assert!(!shader.contains("EnqueueBranch(queue, tail, transmitted)"));
        assert!(shader.contains("state = transmitted;\n            continue;"));
        assert!(shader.contains("Throughput sorting would swap plane identities"));
    }
}
