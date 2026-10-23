//! TU3 drive pass 82AE2E78..82AE30D8, reconstructed from direct disassembly.
//! Finite compiled coefficients are required; exceptional SIMD arithmetic is
//! not claimed to match hardware. Linear/angular softness lanes are retained.

use super::joint::data::{
    Body, ConstraintGeometry, ReactionState, project_correction, read_column, write_column, xyz,
};

/// A drive record as the pass sees it: what it only reads, decoded once a solve (packed.rs), and
/// the linear (32) and angular (48) accumulators it writes.
pub(super) struct Drive {
    geometry: ConstraintGeometry,
    linear_target: [f32; 4],
    angular_target: [f32; 4],
    linear_limits: [f32; 3],
    angular_limits: [f32; 3],
    linear: [f32; 4],
    angular: [f32; 4],
}

impl Drive {
    pub fn decode(record: &[u32]) -> Self {
        let linear_target = read_column(record, 160);
        let angular_target = read_column(record, 176);
        Self {
            geometry: ConstraintGeometry::decode(record),
            linear_limits: [
                read_column(record, 96)[3],
                read_column(record, 112)[3],
                linear_target[3],
            ],
            angular_limits: [
                read_column(record, 128)[3],
                read_column(record, 144)[3],
                angular_target[3],
            ],
            linear_target,
            angular_target,
            linear: read_column(record, 32),
            angular: read_column(record, 48),
        }
    }

    pub fn publish(&self, record: &mut [u32]) {
        write_column(record, 32, self.linear);
        write_column(record, 48, self.angular);
    }

    /// The accumulated linear impulse, as column 32 holds it.
    pub fn linear(&self) -> [f32; 4] {
        self.linear
    }

    /// The accumulated angular impulse, as column 48 holds it.
    pub fn angular(&self) -> [f32; 4] {
        self.angular
    }

    pub fn solve(&mut self, a: &mut Body, b: &mut Body) {
        let geometry = &self.geometry;
        let mut body_a = ReactionState::load(a);
        let mut body_b = ReactionState::load(b);
        let (relative_point, relative_angular) = geometry.relative_corrections(&body_a, &body_b);
        let old_linear = self.linear;
        let old_angular = self.angular;
        let linear_response =
            project_correction(&geometry.linear_projection, relative_point, old_linear);
        let angular_response =
            project_correction(&geometry.angular_projection, relative_angular, old_angular);

        let mut linear_impulse = old_linear;
        let mut angular_impulse = old_angular;
        for axis in 0..3 {
            let linear_candidate =
                linear_response[axis].mul_add(old_linear[3], self.linear_target[axis]);
            let angular_candidate =
                angular_response[axis].mul_add(old_angular[3], self.angular_target[axis]);
            linear_impulse[axis] =
                symmetric_impulse_limit(linear_candidate, self.linear_limits[axis]);
            angular_impulse[axis] =
                symmetric_impulse_limit(angular_candidate, self.angular_limits[axis]);
        }
        let linear_change = xyz(core::array::from_fn(|axis| {
            linear_impulse[axis] - old_linear[axis]
        }));
        let angular_change = xyz(core::array::from_fn(|axis| {
            angular_impulse[axis] - old_angular[axis]
        }));
        // 82AE2FB8/2FC4 select XYZ from the new impulses while retaining each
        // accumulator's fourth (softness) lane from its old value.
        self.linear = linear_impulse;
        self.angular = angular_impulse;
        geometry.apply_impulse_changes(&mut body_a, &mut body_b, linear_change, angular_change);
        body_a.store(a);
        body_b.store(b);
    }
}

fn symmetric_impulse_limit(candidate: f32, limit: f32) -> f32 {
    let upper_limited = if candidate < limit { candidate } else { limit };
    let negative_limit = 0.0 - limit;
    if upper_limited > negative_limit {
        upper_limited
    } else {
        negative_limit
    }
}
