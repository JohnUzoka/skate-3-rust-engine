//! Render the same stock skeleton that animation and physics update.
//! The GLB supplies the mesh, skin weights and hierarchy; it does not play a
//! separate idle clip or own the skater's pose.
use crate::{app::FrameSet, assets::AssetStatus, physics::SkaterRuntime, world::PlayerRoot};
use bevy::{camera::visibility::NoFrustumCulling, mesh::skinning::SkinnedMesh, prelude::*};
use skate_core::animation::output::NativeMatrix;

#[derive(Resource, Default)]
pub(crate) struct AnimationStatus {
    pub ready: bool,
    bindings: Vec<BoneBinding>,
}
struct BoneBinding {
    entity: Entity,
    bone: usize,
    parent_bone: Option<usize>,
}
impl AnimationStatus {
    pub(crate) fn pose_transforms(&self, pose:&[Mat4])->Vec<(Entity,Transform)> {
        self.bindings.iter().filter_map(|b|{
            let global=*pose.get(b.bone)?*render_basis();
            let local=if let Some(parent)=b.parent_bone {(*pose.get(parent)?*render_basis()).inverse()*global} else {global};
            Some((b.entity,Transform::from_matrix(local)))
        }).collect()
    }

    pub(crate) fn online_bindings(&self) -> Vec<(Entity,usize,Option<usize>)> {
        self.bindings.iter().map(|b|(b.entity,b.bone,b.parent_bone)).collect()
    }

    /// How many joints of the skins `shown` aren't bound, and how many bound joints are gone.
    /// `present` poses only the bound joints, by entity, so either leaves those skins in their bind
    /// pose until they bind again.
    fn stale(
        &self,
        skins: &Query<(Entity, &SkinnedMesh)>,
        nodes: &Query<(&Name, &Transform)>,
        shown: impl Fn(Entity) -> bool,
    ) -> (usize, usize) {
        let bound: std::collections::HashSet<Entity> = self.bindings.iter().map(|b| b.entity).collect();
        let unbound = skins
            .iter()
            .filter(|(entity, _)| shown(*entity))
            .flat_map(|(_, skin)| skin.joints.iter())
            .filter(|joint| !bound.contains(*joint))
            .count();
        let gone = self.bindings.iter().filter(|b| nodes.get(b.entity).is_err()).count();
        (unbound, gone)
    }

    /// Prepare a hidden imported scene without disturbing the live bindings.
    pub(crate) fn for_scene(
        root: Entity,
        names: &[String],
        skins: &Query<(Entity, &SkinnedMesh)>,
        nodes: &Query<(&Name, &Transform)>,
        parents: &Query<&ChildOf>,
    ) -> Result<Self, String> {
        let mut bindings = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (entity, skin) in skins.iter() {
            if !parents.iter_ancestors(entity).any(|p| p == root) { continue; }
            for &joint in &skin.joints {
                if !seen.insert(joint) { continue; }
                if !parents.iter_ancestors(joint).any(|p| p == root) {
                    return Err("Imported skin refers to a joint outside its scene".into());
                }
                let (name, _) = nodes.get(joint).map_err(|_| "Imported joint has no name/transform")?;
                let bone = names.iter().position(|n| n.eq_ignore_ascii_case(name.as_str()))
                    .ok_or_else(|| format!("Unsupported imported bone: {name}"))?;
                let mut parent_bone = None;
                for p in parents.iter_ancestors(joint) {
                    if p == root { break; }
                    if let Ok((name, transform)) = nodes.get(p) {
                        if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(name.as_str())) {
                            parent_bone = Some(i);
                            break;
                        }
                        if !transform.to_matrix().abs_diff_eq(Mat4::IDENTITY, 0.00001) {
                            return Err("Imported armature has an unsupported ancestor transform".into());
                        }
                    }
                }
                bindings.push(BoneBinding { entity: joint, bone, parent_bone });
            }
        }
        if bindings.is_empty() { return Err("Imported scene has no skinned character".into()); }
        Ok(Self { ready: true, bindings })
    }
}
#[cfg(test)]
mod custom_model_binding_tests {
    use super::*;
    use bevy::ecs::system::SystemState;
    #[test]
    fn custom_models_bind_only_the_candidate_and_reject_foreign_joints() {
        let mut world = World::new();
        let root = world.spawn_empty().id();
        let hips = world.spawn((Name::new("HIPS"), Transform::default(), ChildOf(root))).id();
        let head = world.spawn((Name::new("HEAD"), Transform::default(), ChildOf(hips))).id();
        for _ in 0..2 {
            world.spawn((ChildOf(root), SkinnedMesh { inverse_bindposes: default(), joints: vec![hips, head] }));
        }
        let foreign = world.spawn((Name::new("unrelated"), Transform::default())).id();
        world.spawn(SkinnedMesh { inverse_bindposes: default(), joints: vec![foreign] });
        let names = vec!["HIPS".into(), "HEAD".into()];
        let mut queries: SystemState<(Query<(Entity, &SkinnedMesh)>, Query<(&Name, &Transform)>, Query<&ChildOf>)> = SystemState::new(&mut world);
        let (skins,nodes,parents) = queries.get(&world);
        let prepared = AnimationStatus::for_scene(root,&names,&skins,&nodes,&parents).unwrap();
        assert!(prepared.ready);
        assert_eq!(prepared.bindings.len(),2);
        assert_eq!(prepared.bindings.iter().find(|b| b.entity == head).unwrap().parent_bone,Some(0));
        world.spawn((ChildOf(root), SkinnedMesh { inverse_bindposes: default(), joints: vec![foreign] }));
        let (skins,nodes,parents) = queries.get(&world);
        assert!(AnimationStatus::for_scene(root,&names,&skins,&nodes,&parents).is_err());
        assert_eq!(prepared.bindings.len(),2);
    }
}
pub(crate) struct AnimationPlugin;
impl Plugin for AnimationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AnimationStatus>()
            .add_systems(Update, (disable_skater_culling, bind, present).chain().in_set(FrameSet::Animation));
    }
}
fn disable_skater_culling(
    mut commands: Commands,
    skins: Query<Entity, (With<SkinnedMesh>, Without<NoFrustumCulling>)>,
    parents: Query<&ChildOf>,
    roots: Query<Entity, With<PlayerRoot>>,
) {
    // GLB primitive bounds describe the bind pose, not the animated skin.
    // Apply to every skater mesh (not just the first skin used to bind bones).
    // World meshes retain their normal culling behavior.
    for entity in &skins {
        if parents.iter_ancestors(entity).any(|e| roots.contains(e)) {
            commands.entity(entity).insert(NoFrustumCulling);
        }
    }
}

fn bind(
    status: Res<AssetStatus>,
    skater: Res<SkaterRuntime>,
    mut animation: ResMut<AnimationStatus>,
    skins: Query<(Entity, &SkinnedMesh)>,
    nodes: Query<(&Name, &Transform)>,
    parents: Query<&ChildOf>,
    visibility: Query<&Visibility>,
    roots: Query<Entity, With<PlayerRoot>>,
    mut exit: MessageWriter<AppExit>,
    // Since when it has waited to bind, and whether it has said why.
    mut waiting: Local<Option<(std::time::Instant, bool)>>,
    // How many times the bindings went stale, for the first lines in the log.
    mut stale: Local<u32>,
) {
    // Not gated on `AssetStatus::Ready`: the skins below are the character's spawned scene, so
    // they are enough. On the Switch, starting with the map loading in the background, the
    // character's asset sometimes never counted as loaded with its dependencies, the skater
    // stayed in its bind pose (findings 59 §14), and nothing said why.
    if *status == AssetStatus::Failed {
        *waiting = None;
        return;
    }
    let shown = |entity| shown_under_player(entity, &parents, &visibility, &roots);
    if animation.ready {
        // The skater can show skins it isn't bound to, and so stand in its bind pose until a swap
        // binds it again (findings 59 §26): bind again as soon as it does.
        let (unbound, gone) = animation.stale(&skins, &nodes, shown);
        if unbound == 0 && gone == 0 {
            *waiting = None;
            return;
        }
        if *stale < 10 {
            warn!(
                "GAME_CHARACTER bindings went stale: {unbound} joints of the skins the skater shows aren't bound, and {gone} of the {} bound are gone; binding again",
                animation.bindings.len()
            );
        }
        *stale += 1;
        animation.ready = false;
        animation.bindings.clear();
    }
    let (since, warned) = waiting.get_or_insert_with(|| (std::time::Instant::now(), false));
    let names = &skater.animation.evaluator.frames.bone_names;
    let result = (|| -> Result<Option<Vec<BoneBinding>>, String> {
        let mut bindings = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (entity, skin) in &skins {
            if !shown(entity) {
                continue;
            }
            let mut binding = Vec::with_capacity(skin.joints.len());
            for &joint in &skin.joints {
                if !seen.insert(joint) { continue; }
                let (name, _) = nodes
                    .get(joint)
                    .map_err(|_| "Skater skin joint is missing its name or transform")?;
                let bone = names
                    .iter()
                    .position(|n| n.eq_ignore_ascii_case(name.as_str()))
                    .ok_or_else(|| {
                        format!("Skater skin bone {name} is absent from the stock rig")
                    })?;
                let mut parent_bone = None;
                for parent in parents.iter_ancestors(joint) {
                    if roots.contains(parent) {
                        break;
                    }
                    if let Ok((name, transform)) = nodes.get(parent) {
                        if let Some(index) = names
                            .iter()
                            .position(|n| n.eq_ignore_ascii_case(name.as_str()))
                        {
                            parent_bone = Some(index);
                            break;
                        }
                        // The supplied GLB's armature node is identity. Reject
                        // another import convention rather than double a pose.
                        if !transform.to_matrix().abs_diff_eq(Mat4::IDENTITY, 0.00001) {
                            return Err(format!(
                                "Skater armature ancestor {name} has an unsupported transform"
                            ));
                        }
                    }
                }
                binding.push(BoneBinding {
                    entity: joint,
                    bone,
                    parent_bone,
                });
            }
            // Bind each visible modular rig, retaining its authored inverse binds.
            bindings.extend(binding);
        }
        Ok(if bindings.is_empty() { None } else { Some(bindings) })
    })();
    match result {
        Ok(Some(binding)) => {
            animation.bindings = binding;
            animation.ready = true;
            info!(
                "GAME_CHARACTER_READY bones={} source=physical_stock_pose{}",
                animation.bindings.len(),
                if *status == AssetStatus::Ready { "" } else { ", before its asset counted as loaded" }
            );
        }
        Ok(None) if !*warned && since.elapsed() >= std::time::Duration::from_secs(10) => {
            let (mut under_player, mut hidden) = (0, 0);
            for (entity, _) in &skins {
                if parents.iter_ancestors(entity).any(|e| roots.contains(e)) {
                    under_player += 1;
                    if parents.iter_ancestors(entity).any(|e| visibility.get(e).is_ok_and(|v| *v == Visibility::Hidden)) {
                        hidden += 1;
                    }
                }
            }
            warn!(
                "GAME_CHARACTER not bound after 10 s: {} skins in all, {under_player} under the player, {hidden} of them hidden; the character's asset {}",
                skins.iter().count(),
                if *status == AssetStatus::Ready { "is loaded" } else { "hasn't counted as loaded with its dependencies" },
            );
            *warned = true;
        }
        Ok(None) => (),
        Err(message) => {
            error!("{message}");
            exit.write(AppExit::error());
        }
    }
}
/// A skin under the player with no hidden ancestor: one the skater shows, and binds.
fn shown_under_player(
    entity: Entity,
    parents: &Query<&ChildOf>,
    visibility: &Query<&Visibility>,
    roots: &Query<Entity, With<PlayerRoot>>,
) -> bool {
    parents.iter_ancestors(entity).any(|e| roots.contains(e))
        && !parents.iter_ancestors(entity).any(|e| visibility.get(e).is_ok_and(|v| *v == Visibility::Hidden))
}
fn present(
    history: Res<crate::presentation::Presentation>,
    skater: Res<SkaterRuntime>,
    replay: Res<crate::replay::Replay>,
    time: Res<Time<Fixed>>,
    animation: Res<AnimationStatus>,
    mut nodes: Query<&mut Transform>,
) {
    if !animation.ready {
        return;
    }
    // Existing GLB was exported through Blender: its bone-local axes are
    // rotated -90 degrees about X relative to the native frames. Both files'
    // world positions are Y-up. This is a skin basis change, not a physics turn.
    let basis = render_basis();
    let Some((previous, current, alpha)) = history.view(&replay, time.overstep_fraction()) else {
        let pose:Vec<_>=skater.render_pose.iter().copied().map(native_matrix).collect();
        for (entity,transform) in animation.pose_transforms(&pose) {if let Ok(mut node)=nodes.get_mut(entity){*node=transform;}}
        return;
    };
    for binding in &animation.bindings {
        // Blend bone-local rotations, not matrix entries or independent world
        // positions: joints retain their hierarchy while limbs turn.
        let local = |snapshot: &crate::presentation::Snapshot| {
            let global = snapshot.bones[binding.bone] * basis;
            Transform::from_matrix(if let Some(parent) = binding.parent_bone {
                (snapshot.bones[parent] * basis).inverse() * global
            } else { global })
        };
        if let Ok(mut transform) = nodes.get_mut(binding.entity) {
            *transform = crate::presentation::blend(local(previous), local(current), alpha);
        }
    }
}
fn render_basis() -> Mat4 {
    Mat4::from_cols(Vec4::X, -Vec4::Z, Vec4::Y, Vec4::W)
}
pub(crate) fn native_matrix(matrix: NativeMatrix) -> Mat4 {
    Mat4::from_cols(
        Vec3::from_array(matrix[0][..3].try_into().unwrap()).extend(0.0),
        Vec3::from_array(matrix[1][..3].try_into().unwrap()).extend(0.0),
        Vec3::from_array(matrix[2][..3].try_into().unwrap()).extend(0.0),
        Vec3::from_array(matrix[3][..3].try_into().unwrap()).extend(1.0),
    )
}

#[cfg(test)]
mod online_swap_tests {
    use super::*;
    use bevy::{ecs::system::SystemState,mesh::skinning::SkinnedMesh};
    #[test]
    fn online_appearance_swap_seeds_new_rig_and_keeps_animating() {
        let mut world=World::new();
        let mut roots=vec![];
        for _ in 0..2 {
            let root=world.spawn_empty().id();
            let hip=world.spawn((Name::new("HIPS"),Transform::default(),ChildOf(root))).id();
            let head=world.spawn((Name::new("HEAD"),Transform::default(),ChildOf(hip))).id();
            world.spawn((ChildOf(root),SkinnedMesh{inverse_bindposes:default(),joints:vec![hip,head]}));
            roots.push((root,hip,head));
        }
        let names=vec!["HIPS".to_owned(),"HEAD".to_owned()];
        let bind=|world:&mut World,root| {
            let mut query:SystemState<(Query<(Entity,&SkinnedMesh)>,Query<(&Name,&Transform)>,Query<&ChildOf>)>=SystemState::new(world);
            let (skins,nodes,parents)=query.get(world);
            AnimationStatus::for_scene(root,&names,&skins,&nodes,&parents).unwrap()
        };
        let old=bind(&mut world,roots[0].0);
        let pose=[Mat4::from_translation(Vec3::new(1.,2.,3.)),Mat4::from_rotation_z(0.7)];
        for (entity,t) in old.pose_transforms(&pose){*world.get_mut::<Transform>(entity).unwrap()=t;}
        let replacement=bind(&mut world,roots[1].0);
        assert_eq!(*world.get::<Transform>(roots[1].1).unwrap(),Transform::default());
        for (entity,t) in replacement.pose_transforms(&pose){*world.get_mut::<Transform>(entity).unwrap()=t;}
        assert_eq!(world.get::<Transform>(roots[0].1),world.get::<Transform>(roots[1].1));
        assert_eq!(world.get::<Transform>(roots[0].2),world.get::<Transform>(roots[1].2));
        let next=[Mat4::from_translation(Vec3::new(2.,3.,4.)),Mat4::from_rotation_z(1.2)];
        for (entity,t) in replacement.pose_transforms(&next){*world.get_mut::<Transform>(entity).unwrap()=t;}
        assert_ne!(world.get::<Transform>(roots[0].1),world.get::<Transform>(roots[1].1));
        assert_ne!(world.get::<Transform>(roots[0].2),world.get::<Transform>(roots[1].2));
    }
}

#[cfg(test)]
mod stale_binding_tests {
    use super::*;
    use bevy::ecs::system::SystemState;
    type Queries = (
        Query<'static, 'static, (Entity, &'static SkinnedMesh)>,
        Query<'static, 'static, (&'static Name, &'static Transform)>,
        Query<'static, 'static, &'static ChildOf>,
        Query<'static, 'static, &'static Visibility>,
        Query<'static, 'static, Entity, With<PlayerRoot>>,
    );
    fn stale(world: &World, queries: &mut SystemState<Queries>, animation: &AnimationStatus) -> (usize, usize) {
        let (skins, nodes, parents, visibility, roots) = queries.get(world);
        animation.stale(&skins, &nodes, |e| shown_under_player(e, &parents, &visibility, &roots))
    }
    #[test]
    fn bindings_go_stale_when_the_skater_shows_unbound_joints_or_loses_bound_ones() {
        let mut world = World::new();
        let player = world.spawn(PlayerRoot).id();
        let rig = |world: &mut World, visibility: Visibility| {
            let model = world.spawn((ChildOf(player), visibility)).id();
            let hips = world.spawn((Name::new("HIPS"), Transform::default(), ChildOf(model))).id();
            world.spawn((ChildOf(model), SkinnedMesh { inverse_bindposes: default(), joints: vec![hips] }));
            (model, hips)
        };
        let (stock, stock_hips) = rig(&mut world, Visibility::Inherited);
        let (outfit, _) = rig(&mut world, Visibility::Hidden);
        let animation = AnimationStatus { ready: true, bindings: vec![BoneBinding { entity: stock_hips, bone: 0, parent_bone: None }] };
        let mut queries = SystemState::<Queries>::new(&mut world);
        assert_eq!(stale(&world, &mut queries, &animation), (0, 0));
        // The outfit shown and the stock skater hidden, with no rebind: the outfit's joint isn't bound.
        *world.get_mut::<Visibility>(stock).unwrap() = Visibility::Hidden;
        *world.get_mut::<Visibility>(outfit).unwrap() = Visibility::Inherited;
        assert_eq!(stale(&world, &mut queries, &animation), (1, 0));
        // The bound joint despawned, as when a scene is spawned again.
        world.despawn(stock_hips);
        assert_eq!(stale(&world, &mut queries, &animation), (1, 1));
    }
}
