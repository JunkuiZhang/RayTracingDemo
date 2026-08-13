//! Shared CPU-side reference for the path-space identity and nested-medium ABI.
//!
//! The renderer will eventually execute this contract in HLSL.  Keeping a tiny
//! Rust model gives us deterministic tests for the bit packing before those
//! values become persistent temporal-history identities on the GPU.

pub(crate) const STABLE_PLANE_COUNT: usize = 3;
pub(crate) const STABLE_BRANCH_ROOT: u32 = 1;
pub(crate) const STABLE_BRANCH_JUST_STARTED: u32 = 0;
pub(crate) const STABLE_BRANCH_ENQUEUED: u32 = u32::MAX - 1;
pub(crate) const STABLE_BRANCH_INVALID: u32 = u32::MAX;
pub(crate) const MAX_STABLE_DELTA_VERTICES: u32 = 15;
pub(crate) const DELTA_LOBE_COUNT: u32 = 4;
pub(crate) const STABLE_PLANE_RECORD_STRIDE: usize = 64;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PathSpaceMode {
    #[default]
    Legacy,
    StablePlanes,
}

impl PathSpaceMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::StablePlanes => "stable-planes",
        }
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
}
