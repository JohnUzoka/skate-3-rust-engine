//! Shared TU3 constraint iteration (`0x82AE27D0`).
//!
//! Constraint families must exchange reactions on every pass. Solving each
//! family to completion independently changes the physical result.
use super::{
    contact_solver::RetailContactJacobian, drive_solver::RetailDriveRows,
    joint_solver::RetailJointJacobian, rigid_body::RetailReactionCorrections,
};
mod contact;
pub mod contact_build;
mod drive;
mod joint;
pub mod packed;
mod packing;

pub(crate) fn compile_contact(record: &mut [u32; 64], time_step: f32) {
    contact_build::build(record, time_step);
}

pub(crate) fn solve_joint_record(record: &mut [u32], a: &mut [u32], b: &mut [u32]) {
    joint::solve(record, a, b);
}

/// Host indices replace the two guest reaction pointers stored in a Jacobian.
#[derive(Clone, Debug)]
pub struct JointConstraint {
    pub jacobian: RetailJointJacobian,
    pub reaction_a: usize,
    pub reaction_b: usize,
}

/// Runs the retail contact â†’ joint â†’ drive ordering. The caller clears the
/// reaction workspace and builds fresh rows once per simulation step.
pub fn solve_constraints(
    contacts: &mut [RetailContactJacobian],
    joints: &mut [JointConstraint],
    drives: &mut [RetailDriveRows],
    reactions: &mut [RetailReactionCorrections],
    iterations: u32,
) {
    // packed::solve's iterations, decoded straight from these records rather than from packed
    // copies of them: the same values, without copying every record twice a tick.
    use packed::{Solving, validate_pair};
    let count = reactions.len();
    for c in contacts.iter() {
        validate_pair(c.reaction_index_a, c.reaction_index_b, count);
    }
    for j in joints.iter() {
        validate_pair(j.reaction_a, j.reaction_b, count);
    }
    for d in drives.iter() {
        validate_pair(d.frame_a_body.reaction_index, d.frame_b_body.reaction_index, count);
    }
    if iterations == 0 {
        return;
    }
    let mut bodies: Vec<_> = reactions.iter().map(packing::reaction_body).collect();
    let mut c: Vec<_> = contacts
        .iter()
        .map(|c| Solving {
            record: contact::ContactRows::read(c.words()),
            a: c.reaction_index_a,
            b: c.reaction_index_b,
        })
        .collect();
    let mut j: Vec<_> = joints
        .iter()
        .map(|j| Solving {
            record: joint::Joint::decode(&j.jacobian.words),
            a: j.reaction_a,
            b: j.reaction_b,
        })
        .collect();
    let mut d: Vec<_> = drives
        .iter()
        .map(|d| {
            let record = packing::drive(d);
            Solving {
                record: drive::Drive::decode(&record.words),
                a: record.reaction_a,
                b: record.reaction_b,
            }
        })
        .collect();
    packed::iterate(&mut bodies, &mut c, &mut j, &mut d, iterations);
    for (contact, native) in contacts.iter_mut().zip(&c) {
        native.record.publish(&mut contact.words);
    }
    for (joint, native) in joints.iter_mut().zip(&j) {
        native.record.publish(&mut joint.jacobian.words);
    }
    for (drive, native) in drives.iter_mut().zip(&d) {
        let [lx, ly, lz, _] = native.record.linear();
        let [ax, ay, az, _] = native.record.angular();
        drive.accumulated_linear_impulse = [lx, ly, lz];
        drive.accumulated_angular_impulse = [ax, ay, az];
    }
    for (reaction, body) in reactions.iter_mut().zip(&bodies) {
        *reaction = packing::body_reaction(body);
    }
}
