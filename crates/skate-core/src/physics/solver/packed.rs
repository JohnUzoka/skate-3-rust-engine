//! Native solver records with host indices replacing guest reaction pointers.
use super::contact::{self, ContactRows};
use super::joint::data::{Body, decode_body, encode_body};
use super::{drive, joint};

#[derive(Clone, Debug)]
pub struct Constraint<const WORDS: usize> {
    pub words: [u32; WORDS],
    pub reaction_a: usize,
    pub reaction_b: usize,
}

pub type Contact = Constraint<64>;
pub type Joint = Constraint<96>;
pub type Drive = Constraint<96>;
pub type Reaction = [u32; 16];

/// Complete contact → joint → drive loops of HorusPipeline (82AE27D0).
/// Contact position corrections remain distinct from velocity-producing
/// reactions; all families see preceding writes during the same iteration.
///
/// A pass reads the rest of each record and writes only its accumulated
/// impulses and its two reactions, so the records are decoded once, the
/// iterations run on floats, and what they wrote goes back into the records
/// once at the end: the same operations in the same order on the same bits.
pub fn solve(
    contacts: &mut [Contact],
    joints: &mut [Joint],
    drives: &mut [Drive],
    reactions: &mut [Reaction],
    iterations: u32,
) {
    validate(contacts, reactions.len());
    validate(joints, reactions.len());
    validate(drives, reactions.len());
    if iterations == 0 {
        return;
    }
    let mut bodies: Vec<Body> = reactions.iter().map(|words| decode_body(words)).collect();
    let mut contact_rows: Vec<_> = contacts
        .iter()
        .map(|c| Solving { record: ContactRows::read(&c.words), a: c.reaction_a, b: c.reaction_b })
        .collect();
    let mut joint_rows: Vec<_> = joints
        .iter()
        .map(|j| Solving { record: joint::Joint::decode(&j.words), a: j.reaction_a, b: j.reaction_b })
        .collect();
    let mut drive_rows: Vec<_> = drives
        .iter()
        .map(|d| Solving { record: drive::Drive::decode(&d.words), a: d.reaction_a, b: d.reaction_b })
        .collect();
    iterate(&mut bodies, &mut contact_rows, &mut joint_rows, &mut drive_rows, iterations);
    for (c, rows) in contacts.iter_mut().zip(&contact_rows) {
        rows.record.publish(&mut c.words);
    }
    for (j, joint) in joints.iter_mut().zip(&joint_rows) {
        joint.record.publish(&mut j.words);
    }
    for (d, drive) in drives.iter_mut().zip(&drive_rows) {
        drive.record.publish(&mut d.words);
    }
    for (words, body) in reactions.iter_mut().zip(&bodies) {
        encode_body(body, words);
    }
}

/// A decoded record and the two reactions it joins.
pub(super) struct Solving<T> {
    pub record: T,
    pub a: usize,
    pub b: usize,
}

/// The iterations of `solve` on decoded records, shared with `solve_constraints`.
pub(super) fn iterate(
    bodies: &mut [Body],
    contacts: &mut [Solving<ContactRows>],
    joints: &mut [Solving<joint::Joint>],
    drives: &mut [Solving<drive::Drive>],
    iterations: u32,
) {
    for _ in 0..iterations {
        for c in contacts.iter_mut() {
            let (a, b) = pair(bodies, c.a, c.b);
            contact::solve(&mut c.record, a, b);
        }
        for j in joints.iter_mut() {
            let (a, b) = pair(bodies, j.a, j.b);
            j.record.solve(a, b);
        }
        for d in drives.iter_mut() {
            let (a, b) = pair(bodies, d.a, d.b);
            d.record.solve(a, b);
        }
    }
}

fn validate<const N: usize>(constraints: &[Constraint<N>], count: usize) {
    for c in constraints {
        validate_pair(c.reaction_a, c.reaction_b, count);
    }
}

pub(super) fn validate_pair(a: usize, b: usize, count: usize) {
    assert_ne!(a, b);
    assert!(a < count && b < count);
}

fn pair(bodies: &mut [Body], a: usize, b: usize) -> (&mut Body, &mut Body) {
    if a < b {
        let (left, right) = bodies.split_at_mut(b);
        (&mut left[a], &mut right[0])
    } else {
        let (left, right) = bodies.split_at_mut(a);
        (&mut right[0], &mut left[b])
    }
}
