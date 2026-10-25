//! Main-thread SDK adapter. Lua never receives World, entity IDs, or asset handles.
mod audio;
mod canvas;
mod graphics_dynamic;
mod vehicle_camera;
pub(crate) mod bridge;
pub(crate) mod replication;
mod graphics;
mod graphics_deformation;
pub(crate) mod attachment;
mod glb;
mod menu;
mod observation;
mod engine_access;
mod participation;
mod camera_stream;
pub(crate) use participation::{player_suspended, peer_suspended};
mod session;
mod volumes;
mod world_audio;
mod triggers;
mod capture;
pub(crate) mod player_physics;

pub(crate) use menu::ModMenu;

use bevy::{
    asset::io::{AssetSourceBuilder, file::FileAssetReader},
    prelude::*,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use skate_dynamics::{
    BodyDesc, ContactEvent, DynamicsWorld, GROUND_BODY_ID, RevoluteJointDesc, Shape,
};
use skate_mods::{
    with_host, Command, DynamicsHost, Manager, RaycastFilter, RaycastOptions,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Resource)]
pub(crate) struct Mods {
    pub manager: Manager,
    native_snapshot: Option<([u64; 4], skate_mods::SnapshotFields)>,
    world: DynamicsWorld,
    bodies: BTreeMap<(String, String), u64>,
    joints: BTreeMap<(String, String), u64>,
    graphics: BTreeMap<(String, String), GraphicsOwned>,
    overlays: BTreeMap<(String, String), Entity>,
    canvases: BTreeMap<(String, String), canvas::Canvas>,
    attach: Option<AttachState>,
    detach_error: Option<String>,
    detach_pending: Option<(Transform,std::time::Instant)>,
    camera: CameraOverride,
    suspended_by: BTreeSet<String>,
    hidden_players: BTreeMap<Entity, Visibility>,
    remote_cameras: BTreeMap<u64, camera_stream::View>,
    generation: u64,
    graphics_serial: u64,
    debug_owners: BTreeSet<String>,
    ground_ready: bool,
    /// Passive finite-mass Rapier shadows of remote native actor bodies.
    skater_proxies: BTreeMap<usize, u64>,
    /// APPLICATION keys currently published for local dynamics bodies.
    dyn_published: BTreeSet<String>,
    replication: replication::State,
    /// Local Lua `sdk.net.publish` values: (mod_id, key) → JSON.
    net_states: BTreeMap<(String, String), Value>,
    /// Wire keys currently published for local net states.
    net_published: BTreeSet<String>,
    /// Remote Lua net values: (peer, mod_id, key) → JSON.
    net_remote: BTreeMap<(u64, String, String), Value>,
    /// Maps (peer, wire_key) → (mod_id, key) for empty-value cleanup.
    net_remote_wire: BTreeMap<(u64, String), (String, String)>,
    net_status: String,
    multiplayer_debug: BTreeMap<(String, String), String>,
    /// Contacts from the last DynamicsWorld::step (before drain).
    last_contacts: Vec<ContactEvent>,
    command_results: BTreeMap<(String,String), Value>,
    graph_gates: BTreeMap<(String,String,usize),(String,bool)>,
    input_overrides: BTreeMap<usize,(String,f32)>,
    session: session::Runtime,
    skater_remote: BTreeMap<u64, observation::WireObs>,
    pending_remote_teleport: Option<skate_mods::TeleportOptions>,
    volumes: BTreeMap<(String, String), volumes::Volume>,
    pub(crate) custom_menus: BTreeMap<(String,String), skate_mods::extensions::MenuOptions>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetWire {
    owner: String,
    fingerprint: u64,
    key: String,
    value: Value,
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ModCameraSet;


type GraphicsOwned = graphics::Owned;

#[derive(Clone)]
struct AttachState {
    owner: String,
    body: String,
    offset: Vec3,
    hidden: Vec<(Entity, Visibility)>,
    fallback: Transform,
}

#[derive(Default)]
struct CameraOverride {
    owner: Option<String>,
    rig: Option<vehicle_camera::Rig>,
    saved_near: Option<f32>,
    follow: Option<(String, String, Vec3)>,
    fixed: Option<(Vec3, Option<Vec3>)>,
    watch: Option<u64>,
}

impl CameraOverride {
    fn clear(&mut self) {
        // Restore the camera lens on the next presentation frame, even on unload.
        let saved_near = self.saved_near.take();
        *self = Self { saved_near, ..Default::default() };
    }
    fn claim(&mut self, owner: &str) -> Result<(), String> {
        if self.owner.as_deref().is_some_and(|o| o != owner) {
            return Err("camera is owned by another mod".into());
        }
        self.owner = Some(owner.to_owned());
        Ok(())
    }
    fn follows(&self, owner: &str, body: &str) -> bool {
        self.owner.as_deref() == Some(owner) &&
            (self.rig.as_ref().is_some_and(|r| r.body == body) ||
             self.follow.as_ref().is_some_and(|(_, b, _)| b == body))
    }
}

pub(crate) struct ModdingPlugin;

impl Plugin for ModdingPlugin {
    fn build(&self, app: &mut App) {
        let root = package_root();
        if let Err(e) = std::fs::create_dir_all(&root) {
            warn!("Cannot create mods folder {}: {e}", root.display());
        }
        info!("Mod packages folder: {}", root.display());
        let settings = std::env::var_os("SKATE3_MOD_SETTINGS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                app.world()
                    .resource::<crate::config::Config>()
                    .asset_root
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .join("settings/mods")
            });
        app.insert_resource(Mods::new(Manager::new(root, settings)))
        .init_resource::<ModMenu>();
        menu::install(app);
        audio::install(app);
        world_audio::install(app);
        graphics_dynamic::install(app);
        capture::install(app);
        triggers::install(app);
        app.add_systems(
            PreUpdate,
            maintenance.after(crate::map_transition::MapTransitionSet),
        )
        .add_systems(
            FixedUpdate,
            (replication::sample_fixed, bridge::dynamics_to_board).chain()
                .after(crate::app::SimulationSet::Controls)
                .after(crate::multiplayer::prepare)
                .before(crate::app::SimulationSet::Physics)
                .run_if(crate::graphics_menu::gameplay_active),
        )
        .add_systems(
            FixedUpdate,
            fixed
                .after(crate::app::SimulationSet::Physics)
                .after(crate::trigger_volumes::TriggerSet)
                .run_if(crate::graphics_menu::gameplay_active),
        )
        .add_systems(
            Update,
            (
                update.after(crate::app::FrameSet::Animation),
                bridge::sync_network.after(crate::multiplayer::send_pose).after(update),
                sync_net.after(bridge::sync_network).after(ModCameraSet),
                participation::present.after(update).after(crate::app::FrameSet::Animation),
                graphics::debug.after(bridge::sync_network).after(update),
                present_camera
                    .in_set(ModCameraSet)
                    .after(crate::camera::present)
                    .after(update)
                    .after(bridge::sync_network)
                    .after(crate::multiplayer::RemoteRenderSet)
                    .before(crate::app::FrameSet::Verification),
            ),
        );
    }
}

pub(crate) fn register_source(app: &mut App) {
    let root = package_root();
    app.register_asset_source(
        "mods",
        AssetSourceBuilder::new(move || Box::new(FileAssetReader::new(root.clone()))),
    );
}

pub(crate) fn package_root() -> std::path::PathBuf {
    std::env::var_os("SKATE3_MODS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.join("mods")))
                .unwrap_or_else(|| "mods".into())
        })
}

pub(crate) fn player_attached(mods: &Mods) -> bool {
    mods.attach.is_some()
}

/// Whether a mod can read the player's physics this tick: `snapshot_ro`, the only reader of
/// `SkaterRuntime::mod_contact_frame`, runs only while mod work is pending.
pub(crate) fn reads_player_physics(mods: &Mods) -> bool {
    mods.mod_work_pending()
}

impl Mods {
    /// Whether anything can use the dynamics world's map ground: a running mod,
    /// or bodies or joints a mod left behind.
    fn needs_ground(&self) -> bool {
        self.manager.packages.values().any(|p| p.running()) || !self.bodies.is_empty() || !self.joints.is_empty()
    }

    /// Whether any mod has work the snapshot serves. The game's own mirrors of other players
    /// (`skater_proxies`, from `bridge::push_skater_into_rapier`) don't count. A busy check that
    /// counted them built the mod snapshot on every physics tick with one remote player present,
    /// about 40 % of the Switch's main thread in a profiled run, though no mod reads it.
    fn mod_work_pending(&self) -> bool {
        self.manager.packages.values().any(|p| p.running())
            || !self.manager.commands.is_empty()
            || !self.manager.retired.is_empty()
            || !self.bodies.is_empty()
            || !self.graphics.is_empty()
            || !self.overlays.is_empty()
            || !self.joints.is_empty()
            || self.attach.is_some()
            || !self.canvases.is_empty()
            || !self.volumes.is_empty()
    }
}

fn body_snapshot_for(mods: &Mods, owner: &str, player_shapes: &[(skate_dynamics::rapier3d::prelude::SharedShape, skate_dynamics::rapier3d::prelude::Pose)]) -> serde_json::Value {
    let mut bodies = serde_json::Map::new();
    let mut reverse = BTreeMap::<u64, String>::new();
    for ((o, key), id) in &mods.bodies {
        if o != owner {
            continue;
        }
        reverse.insert(*id, key.clone());
        if let Some(snap) = mods.world.read(*id) {
            bodies.insert(
                key.clone(),
                json!({
                    "player_overlapping": mods.world.overlaps_shapes(*id, player_shapes),
                    "position": snap.position,
                    "rotation": snap.rotation,
                    "linvel": snap.linvel,
                    "angvel": snap.angvel,
                    "force": snap.force,
                    "torque": snap.torque,
                    "mass": snap.mass,
                    "speed": (snap.linvel[0]*snap.linvel[0]
                        + snap.linvel[1]*snap.linvel[1]
                        + snap.linvel[2]*snap.linvel[2]).sqrt(),
                }),
            );
        }
    }
    let skip: BTreeSet<u64> = mods.skater_proxies.values().copied().collect();
    let label = |id: u64| -> Option<String> {
        if id == GROUND_BODY_ID {
            Some("ground".into())
        } else {
            reverse.get(&id).cloned()
        }
    };
    let owned = |id: u64| reverse.contains_key(&id);
    let contacts: Vec<_> = mods
        .last_contacts
        .iter()
        .filter(|c| !skip.contains(&c.body_a) && !skip.contains(&c.body_b))
        .filter(|c| owned(c.body_a) || owned(c.body_b))
        .map(|c| {
            json!({
                "a": label(c.body_a),
                "b": label(c.body_b),
                "started": c.started,
            })
        })
        .collect();
    let touching: Vec<_> = mods
        .world
        .active_contact_pairs()
        .into_iter()
        .filter(|(a, b)| !skip.contains(a) && !skip.contains(b))
        .filter(|(a, b)| owned(*a) || owned(*b))
        .map(|(a, b)| json!({"a": label(a), "b": label(b)}))
        .collect();
    json!({"bodies": bodies, "contacts": contacts, "touching": touching})
}

fn network_snapshot(world: &World, mods: &Mods) -> Value {
    let net = world.get_resource::<crate::multiplayer::Multiplayer>();
    let (active, id, host) = net.map_or((false, 0, true), |n| n.mod_identity());
    let host_id = net.map_or(id, |n| n.host_actor());
    let mut players: Vec<String> = net
        .map(|n| n.player_ids().into_iter().map(|p| p.to_string()).collect())
        .unwrap_or_default();
    if players.is_empty() {
        players.push(id.to_string());
    }
    let mut states = serde_json::Map::new();
    let insert = |states: &mut serde_json::Map<String, Value>,
                  mod_id: &str,
                  peer: &str,
                  key: &str,
                  value: &Value| {
        if value.is_null() {
            return;
        }
        let mod_entry = states
            .entry(mod_id.to_owned())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        let Some(mod_map) = mod_entry.as_object_mut() else {
            return;
        };
        let peer_entry = mod_map
            .entry(peer.to_owned())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        let Some(peer_map) = peer_entry.as_object_mut() else {
            return;
        };
        peer_map.insert(key.to_owned(), value.clone());
    };
    let local = id.to_string();
    for ((mod_id, key), value) in &mods.net_states {
        insert(&mut states, mod_id, &local, key, value);
    }
    for ((peer, mod_id, key), value) in &mods.net_remote {
        insert(&mut states, mod_id, &peer.to_string(), key, value);
    }
    json!({
        "active": active,
        "local_id": local,
        "is_host": host,
        "host_id": host_id.to_string(),
        "players": players,
        "states": states,
        "status": mods.net_status,
    })
}

fn net_wire_key(owner: &str, key: &str) -> String {
    let full = format!("net:{owner}:{key}");
    if full.len() <= 128 {
        full
    } else {
        format!(
            "net:{:016x}",
            skate_net::hash(format!("{owner}/{key}").as_bytes())
        )
    }
}

fn ensure_ground(world: &World, mods: &mut Mods) -> Result<(), String> {
    // Rapier's copy of the map collision serves only mods. Downtown's keeps 220 MiB and
    // peaks at 362 MiB while it builds, which ran the Switch out of memory right after a
    // map change (findings 58 §3.9), so it waits for something that can use it.
    if mods.ground_ready || !mods.needs_ground() {
        return Ok(());
    }
    mods.world.set_ground(
        world
            .resource::<crate::physics::GamePhysics>()
            .world_triangles()
            .iter()
            .map(|t| t.triangle.vertices.map(|p| [p.x, p.y, p.z])),
    )?;
    mods.ground_ready = true;
    Ok(())
}

/// Drops rapier's copy of the live map's collision when a map change starts, as
/// `start` does the map's own. Nothing steps it while the change loads, and the
/// commit's `clear_runtime` rebuilds it from the new map once something needs it.
pub(crate) fn release_ground(world: &mut World) {
    if let Some(mut mods) = world.get_resource_mut::<Mods>() {
        mods.world.clear_ground();
        mods.ground_ready = false;
    }
}

fn maintenance(world: &mut World, mut scanned_once: Local<bool>) {
    // Listing the mods folder on the SD card waits behind any other write to it, for 100-260 ms
    // (findings 63 §6.3). On the Switch, list it once, on a map change (below) and while the
    // mods menu is open. A PC keeps polling every 500 ms for hot reload.
    let menu_open = world.get_resource::<ModMenu>().is_some_and(|menu| menu.open);
    let poll = cfg!(not(target_os = "horizon")) || menu_open || !*scanned_once;
    *scanned_once = true;
    world.resource_scope(|world, mut mods: Mut<Mods>| {
        let generation = world
            .resource::<crate::map_transition::CurrentMap>()
            .generation;
        if mods.generation != generation {
            clear_runtime(world, &mut mods);
            mods.generation = generation;
            mods.manager.commands.clear();
            if mods.mod_work_pending() {
                let camera = camera_position(world);
                let snap = snapshot_ro(world, &mut mods, camera);
                mods.manager.snapshot = std::sync::Arc::new(snap);
                let map = mods.manager.snapshot["map"].clone();
                mods.manager
                    .dispatch("on_event", json!({"name":"world_changed","map":map}));
            }
            mods.manager.scan(false);
            audio_reloads(world, &mut mods);
            sync_audio_content(world, &mods);
            return;
        }
        if poll {
            mods.manager.scan(false);
        }
        audio_reloads(world, &mut mods);
        sync_audio_content(world, &mods);
        if !mods.mod_work_pending() {
            return;
        }
        // The last update already published observations. Maintenance only scans
        // packages and applies pending lifecycle/menu commands.
        apply(world, &mut mods);
    });
}

/// Doc 16 L7: a running mod whose only changed files are its `audio.json` and files it names keeps
/// its script; its audio content is reloaded (the overlay is read again by `sync_audio_content`
/// and hot-swapped, `game_audio::swap`). The native clips of the changed files are dropped (read
/// again at the next play or rule compile); a changed file the script holds as a Bevy clip
/// reloads the mod as before.
fn audio_reloads(world: &mut World, mods: &mut Mods) {
    let ids: Vec<String> = mods.manager.packages.iter().filter(|(_, p)| p.running()).map(|(id, _)| id.clone()).collect();
    for id in ids {
        let Some(changes) = mods.manager.packages.get_mut(&id).and_then(|p| p.take_audio_changes()) else { continue };
        if audio::files_changed(world, &id, &changes) {
            info!("Mods: {id}: audio content changed ({}): reloaded while the script keeps running", changes.join(", "));
        } else {
            info!("Mods: {id}: a changed file is a clip the script plays ({}): the mod reloads", changes.join(", "));
            mods.manager.reload(&id);
        }
    }
}

/// Audio content overlays follow the running mods (`game_audio::AudioContent`): a mod that
/// starts, changes, stops or fails changes the set, and the audio restarts once at the next pass.
fn sync_audio_content(world: &mut World, mods: &Mods) {
    let Some(mut content) = world.get_resource_mut::<crate::game_audio::AudioContent>() else { return };
    let content = content.bypass_change_detection();
    content.scanned = true;
    content.sync(mods.manager.packages.iter().filter(|(_, p)| p.running()).map(|(id, p)| (id.as_str(), p.root.as_path(), p.content_fingerprint())));
}

fn camera_position(world: &mut World) -> Option<[f32; 3]> {
    world
        .query_filtered::<&Transform, With<crate::camera::GameplayCamera>>()
        .iter(world)
        .next()
        .map(|t| t.translation.to_array())
}

fn snapshot_ro(world: &World, mods: &mut Mods, camera: Option<[f32; 3]>) -> serde_json::Value {
    let _span = bevy::log::tracing::info_span!("mods.snapshot").entered();
    // The native state changes at simulation ticks. Render callbacks can reuse
    // its serialization until either native resource changes, while inputs,
    // command receipts, camera and network observations remain frame-current.
    let key = [
        world.get_resource_ref::<crate::physics::GamePhysics>().unwrap().last_changed().get() as u64,
        world.get_resource_ref::<crate::physics::SkaterRuntime>().unwrap().last_changed().get() as u64,
        mods.bodies.len() as u64,
        mods.generation,
    ];
    if mods.native_snapshot.as_ref().is_none_or(|(previous, _)| *previous != key) {
        let fields = BTreeMap::from([
            ("player_physics".into(), std::sync::Arc::new(player_physics::snapshot(world, mods))),
            ("engine".into(), std::sync::Arc::new(engine_access::snapshot(world))),
        ]);
        mods.native_snapshot = Some((key, std::sync::Arc::new(fields)));
    }
    mods.manager.snapshot_fields = mods.native_snapshot.as_ref().unwrap().1.clone();
    let map = world.resource::<crate::map_transition::CurrentMap>();
    let physics = world.resource::<crate::physics::GamePhysics>();
    let keys: BTreeMap<_, _> = world
        .resource::<ButtonInput<KeyCode>>()
        .get_pressed()
        .map(|k| (format!("{k:?}"), true))
        .collect();
    let actions = *world
        .resource::<crate::input::PublishedTickInput>()
        .0
        .actions()
        .values();
    let pad = world.resource::<crate::input::ControllerInput>().raw_input();
    let controllers = controllers_snapshot(world.resource::<crate::input::ControllerInput>());
    let net = network_snapshot(world, mods);
    let local_id = net
        .get("local_id")
        .and_then(Value::as_str)
        .unwrap_or("0")
        .to_owned();
    let (mut player, skaters) = observation::snapshot(world, mods, &local_id);
    if let Some(root) = attachment::local_root(mods) {
        if let Some(obj) = player.as_object_mut() {
            let heading = {
                let f = root.rotation * Vec3::Z;
                f.x.atan2(f.z)
            };
            obj.insert("position".into(), json!(root.translation.to_array()));
            obj.insert("heading".into(), json!(heading));
            obj.insert("rotation".into(), json!(root.rotation.to_array()));
        }
    }
    let host = net.get("is_host").and_then(Value::as_bool).unwrap_or(true);
    let host_id = net
        .get("host_id")
        .and_then(Value::as_str)
        .unwrap_or(&local_id)
        .to_owned();
    let players: Vec<String> = net
        .get("players")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_else(|| vec![local_id.clone()]);
    json!({
        "player": player,
        "skaters": skaters,
        "command_results": mods.command_results.iter().fold(serde_json::Map::<String,Value>::new(), |mut out,((owner,key),value)| {
            out.entry(owner.clone()).or_insert_with(||json!({}))[key]=value.clone();out
        }),
        "volumes": mods.manager.packages.keys().map(|owner| {
            (owner.clone(), volumes::snapshot(mods, owner, &local_id, &observation::local(world), &mods.skater_remote))
        }).collect::<serde_json::Map<String, Value>>(),
        "world_audio": mods.manager.packages.keys().map(|owner| (owner.clone(), world_audio::snapshot(world, owner))).collect::<serde_json::Map<String, Value>>(),
        "world_audio_info": world_audio::info(world),
        "audio": mods.manager.packages.keys().map(|owner| (owner.clone(), crate::game_audio::mod_snapshot(world, owner))).filter(|(_, v)| !v.is_null()).collect::<serde_json::Map<String, Value>>(),
        "audio_info": crate::game_audio::mod_info(world),
        "session": session::lua(mods, net.get("active").and_then(Value::as_bool).unwrap_or(false), &local_id, host, &host_id, &players),
        "attach": mods.attach.as_ref().map(|a| json!({"body": a.body, "owner": a.owner})),
        "detach_error": mods.detach_error,
        "detach_pending": mods.detach_pending.is_some(),
        "map": {"name": map.name, "generation": map.generation},
        "triggers": triggers::snapshot(world),
        "menus": crate::retail_menus::snapshot(world),
        "tick": physics.ticks,
        "keys": keys,
        "actions": actions,
        "pad": {
            "buttons": pad.buttons,
            "triggers": pad.triggers,
            "left": pad.left,
            "right": pad.right,
        },
        "controllers": controllers,
        "paused": world.resource::<crate::graphics_menu::Menu>().open,
        "replay": world.resource::<crate::replay::Replay>().active,
        // Read-only frame-time statistics (`frame_timing`); null before the plugin runs.
        "frame": world.get_resource::<crate::frame_timing::FrameTiming>().map(|t| t.snapshot()),
        "camera": camera.map(|position| json!({"position": position})),
        "camera_angle": camera_angle_snapshot(world),
        "physics": {"bodies": {}, "contacts": []},
        "network": net,
    })
}

fn fixed(world: &mut World) {
    let _span = bevy::log::tracing::info_span!("mods.fixed").entered();
    if world.resource::<crate::replay::Replay>().active {
        return;
    }
    let dt = world.resource::<Time<Fixed>>().delta_secs_f64();
    world.resource_scope(|world, mut mods: Mut<Mods>| {
        bridge::take_reactions(&mut mods, &mut world.resource_mut::<crate::physics::GamePhysics>());
        if mods.mod_work_pending()
            || mods.manager.packages.values().any(|package| package.running())
        {
            let camera = camera_position(world);
            mods.manager.snapshot = std::sync::Arc::new(snapshot_ro(world, &mut mods, camera));
        }
        if let Err(e) = ensure_ground(world, &mut mods) {
            warn!("dynamics ground: {e}");
        }
        // Enter/exit events of this tick's trigger update, before on_fixed_update.
        triggers::dispatch(world, &mut mods);
        let ids: Vec<_> = mods
            .manager
            .packages
            .iter()
            .filter(|(_, p)| p.running())
            .map(|(id, _)| id.clone())
            .collect();
        // Only the running mods' callbacks below use the player's shapes.
        let player_shapes = if !ids.is_empty() && mods.suspended_by.is_empty() && mods.attach.is_none() {
            bridge::player_shapes(world)
        } else { Vec::new() };
        for id in ids {
            let _span = bevy::log::tracing::info_span!("mods.fixed_callback", mod_id = %id).entered();
            let physics = body_snapshot_for(&mods, &id, &player_shapes);
            // Sync dynamics queries for this mod while on_fixed_update runs.
            struct Host<'a> {
                world: &'a mut DynamicsWorld,
                bodies: &'a BTreeMap<(String, String), u64>,
                owner: &'a str,
            }
            impl DynamicsHost for Host<'_> {
                fn raycast(
                    &mut self,
                    origin: [f32; 3],
                    direction: [f32; 3],
                    options: &RaycastOptions,
                ) -> Option<serde_json::Value> {
                    let hit = match options.filter {
                        RaycastFilter::Ground => self.world.raycast_ground(
                            origin,
                            direction,
                            options.max_distance,
                        )?,
                        RaycastFilter::All => {
                            let excluded: Vec<_> = options
                                .exclude
                                .iter()
                                .filter_map(|key| {
                                    self.bodies
                                        .get(&(self.owner.to_owned(), key.clone()))
                                        .copied()
                                })
                                .collect();
                            self.world.raycast_excluding_bodies(
                                origin,
                                direction,
                                options.max_distance,
                                &excluded,
                            )?
                        }
                    };
                    let body_key = self
                        .bodies
                        .iter()
                        .find(|((o, _), id)| o.as_str() == self.owner && **id == hit.body)
                        .map(|((_, k), _)| k.clone())
                        .or_else(|| (hit.body == GROUND_BODY_ID).then(|| "ground".into()));
                    Some(serde_json::json!({
                        "body": body_key,
                        "point": hit.point,
                        "normal": hit.normal,
                        "toi": hit.toi,
                    }))
                }
                fn velocity_at(&self, key: &str, point: [f32; 3]) -> Option<[f32; 3]> {
                    let id = *self.bodies.get(&(self.owner.to_owned(), key.to_owned()))?;
                    self.world.velocity_at(id, point)
                }
                fn effective_inv_mass(
                    &self,
                    key: &str,
                    point: [f32; 3],
                    direction: [f32; 3],
                ) -> Option<f32> {
                    let id = *self.bodies.get(&(self.owner.to_owned(), key.to_owned()))?;
                    self.world.effective_inv_mass(id, point, direction)
                }
                fn spring_ray(
                    &mut self,
                    key: &str,
                    desc: skate_dynamics::SpringRayDesc,
                ) -> Option<skate_dynamics::SpringRayHit> {
                    let id = *self.bodies.get(&(self.owner.to_owned(), key.to_owned()))?;
                    self.world.spring_ray(id, desc)
                }
                fn local_ang_accel_impulse(
                    &self,
                    key: &str,
                    local_accel: [f32; 3],
                    dt: f32,
                ) -> Option<[f32; 3]> {
                    let id = *self.bodies.get(&(self.owner.to_owned(), key.to_owned()))?;
                    self.world.local_ang_accel_impulse(id, local_accel, dt)
                }
            }
            // Split-borrow world/bodies for the query host while Manager runs Lua.
            let Mods {
                manager,
                world: dyn_world,
                bodies,
                ..
            } = &mut *mods;
            let mut host = Host {
                world: dyn_world,
                bodies,
                owner: &id,
            };
            with_host(&mut host, || {
                manager.call_with_physics(&id, "on_fixed_update", json!({"dt": dt}), physics);
            });
        }
        // New command frame: drop last tick's user forces so they cannot stack in Rapier.
        mods.world.begin_force_frame();
        apply(world, &mut mods);
        if let Err(e) = ensure_ground(world, &mut mods) {
            warn!("dynamics ground: {e}");
        }
        {
            let physics = world.resource::<crate::physics::GamePhysics>();
            let skater = world.resource::<crate::physics::SkaterRuntime>();
            bridge::push_skater_into_rapier(&mut mods, physics, skater);
        }
        if !mods.bodies.is_empty() || mods.ground_ready || !mods.skater_proxies.is_empty() {
            let _span = bevy::log::tracing::info_span!("mods.dynamics_step").entered();
            mods.world.step(dt as f32);
            mods.last_contacts = mods.world.drain_contacts();
        }
        triggers::sync_tracked(world, &mods);
        sync_graphics(world, &mut mods);
        sync_attach(world, &mut mods);
        // Record native post-step poses for render interpolation, including hood view.
        record_camera(&mut mods, dt as f32);
    });
}

fn update(world: &mut World) {
    let _span = bevy::log::tracing::info_span!("mods.update").entered();
    world.resource_scope(|world, mut mods: Mut<Mods>| {
        if !mods.mod_work_pending() {
            return;
        }
        let camera = camera_position(world);
        let snap = snapshot_ro(world, &mut mods, camera);
        let paused =
            snap["paused"].as_bool().unwrap_or(true) || snap["replay"].as_bool().unwrap_or(false);
        let dt = world.resource::<Time<Real>>().delta_secs_f64().min(0.25);
        mods.manager.snapshot = std::sync::Arc::new(snap);
        // UI state must refresh during pause (including network role changes).
        // This callback does not advance Lua simulation time or timers.
        dispatch_profiled(&mut mods.manager, "on_ui_update", json!({"dt": dt, "paused": paused}));
        if !paused {
            dispatch_profiled(&mut mods.manager, "on_update", json!({"dt": dt}));
        }
        apply(world, &mut mods);
        sync_graphics(world, &mut mods);
        sync_attach(world, &mut mods);
        let hidden = paused || world.get_resource::<crate::customiser::Customiser>()
            .is_some_and(|c| c.open);
        canvas::present(world, &mods.canvases, hidden);
    });
}

/// Camera Angle holds and shot tunings of one mod (`Some`) or of every mod (`None`).
fn clear_camera_angle(world: &mut World, owner: Option<&str>) {
    if let Some(mut settings) = world.get_resource_mut::<crate::camera::CameraAngleSettings>() {
        settings.clear_owner(owner);
    }
}

fn camera_angle_snapshot(world: &World) -> Value {
    let shot = world.get_resource::<crate::camera::CameraRuntime>().map_or("", |c| c.selected_shot());
    world.get_resource::<crate::camera::CameraAngleSettings>().map_or(Value::Null, |s| s.snapshot(shot))
}

fn clear_runtime(world: &mut World, mods: &mut Mods) {
    replication::reset(world,mods);
    audio::clear(world);
    world_audio::clear(world);
    crate::game_audio::clear_mods_runtime(world);
    graphics_dynamic::clear(world);
    canvas::clear_owner(world, &mut mods.canvases, None);
    detach_player(world, mods, true);
    let keys: Vec<_> = mods.graphics.keys().cloned().collect();
    for key in keys {
        retire_graphics(world, mods, &key);
    }
    let overlays: Vec<_> = mods.overlays.keys().cloned().collect();
    for key in overlays {
        if let Some(e) = mods.overlays.remove(&key) {
            world.despawn(e);
        }
    }
    mods.multiplayer_debug.clear();
    mods.bodies.clear();
    mods.joints.clear();
    mods.skater_proxies.clear();
    mods.dyn_published.clear();
    mods.net_states.clear();
    mods.net_published.clear();
    mods.net_remote.clear();
    mods.net_remote_wire.clear();
    mods.net_status.clear();
    mods.last_contacts.clear();
    engine_access::restore_gates(world,mods,None);
    mods.command_results.clear();mods.input_overrides.clear();
    mods.debug_owners.clear();
    player_physics::clear(world,None);
    mods.custom_menus.clear();
    crate::retail_menus::clear_mods(world);
    participation::clear(world, mods);
    mods.remote_cameras.clear();
    mods.session.reset();
    mods.skater_remote.clear();
    mods.pending_remote_teleport = None;
    volumes::clear(world, mods);
    triggers::clear(world);
    capture::clear(world);
    clear_camera_angle(world, None);
    world.resource_mut::<crate::physics::GamePhysics>().set_external_queries(None);
    mods.world = DynamicsWorld::default();
    mods.ground_ready = false;
    mods.camera.clear();
    mods.detach_pending=None;
    mods.detach_error=None;
}

fn retire_graphics(world: &mut World, mods: &mut Mods, key: &(String, String)) {
    if let Some(owned) = mods.graphics.remove(key) {
        owned.deformation.clear(world);
        world.despawn(owned.entity);
        if let Some(id) = owned.mesh {
            world.resource_mut::<Assets<Mesh>>().remove(id);
        }
        if let Some(id) = owned.material {
            world.resource_mut::<Assets<StandardMaterial>>().remove(id);
        }
    }
}

fn apply(world: &mut World, mods: &mut Mods) {
    let _span = bevy::log::tracing::info_span!("mods.apply").entered();
    let retired = std::mem::take(&mut mods.manager.retired);
    for id in &retired {
        mods.suspended_by.remove(id);
        engine_access::restore_gates(world,mods,Some(id));
        mods.command_results.retain(|(owner,_),_|owner!=id);
        mods.input_overrides.retain(|_,(owner,_)|owner!=id);
        audio::stop_owner(world, id, true);
        world_audio::clear_owner(world, id);
        crate::game_audio::clear_mod(world, id);
        graphics_dynamic::clear_owner(world, id);
        volumes::clear_owner(world, mods, id);
        triggers::clear_owner(world, id);
        capture::clear_owner(world, id);
        clear_camera_angle(world, Some(id));
        player_physics::clear(world,Some(id));
        mods.custom_menus.retain(|(owner,_),_|owner!=id);
        crate::retail_menus::clear_owner(world, id);
        canvas::clear_owner(world, &mut mods.canvases, Some(id));
        detach_if_owner(world, mods, id);
        if mods.camera.owner.as_ref() == Some(id) {
            mods.camera.clear();
        }
        let gkeys: Vec<_> = mods
            .graphics
            .keys()
            .filter(|(o, _)| o == id)
            .cloned()
            .collect();
        for key in gkeys {
            retire_graphics(world, mods, &key);
        }
        mods.multiplayer_debug.retain(|(owner, _), _| owner != id);
        let okeys: Vec<_> = mods
            .overlays
            .keys()
            .filter(|(o, _)| o == id)
            .cloned()
            .collect();
        for key in okeys {
            if let Some(e) = mods.overlays.remove(&key) {
                world.despawn(e);
            }
        }
        let bkeys: Vec<_> = mods
            .bodies
            .keys()
            .filter(|(o, _)| o == id)
            .cloned()
            .collect();
        for key in bkeys {
            if let Some(body) = mods.bodies.remove(&key) {
                mods.world.remove(body);
            }
        }
        let jkeys: Vec<_> = mods
            .joints
            .keys()
            .filter(|(o, _)| o == id)
            .cloned()
            .collect();
        for key in jkeys {
            if let Some(j) = mods.joints.remove(&key) {
                mods.world.remove_joint(j);
            }
        }
        mods.net_states.retain(|(o, _), _| o != id);
    }
    mods.manager
        .commands
        .retain(|(owner, _)| !retired.iter().any(|id| id == owner));
    let mut batches = BTreeMap::<String, Vec<Command>>::new();
    for (id, command) in std::mem::take(&mut mods.manager.commands) {
        batches.entry(id).or_default().push(command);
    }
    for (id, mut commands) in batches {
        if !mods.manager.packages.get(&id).is_some_and(|p| p.running()) {
            continue;
        }
        if let Err(e) = ensure_ground(world, mods) {
            warn!("Lua mod {id}: {e}");
            engine_access::restore_gates(world,mods,Some(&id));
            mods.manager.fail(&id, e);
            mods.input_overrides.retain(|_,(owner,_)|owner!=&id);
            audio::stop_owner(world, &id, true);
            world_audio::clear_owner(world, &id);
            crate::game_audio::clear_mod(world, &id);
            graphics_dynamic::clear_owner(world, &id);
            volumes::clear_owner(world, mods, &id);
            triggers::clear_owner(world, &id);
            capture::clear_owner(world, &id);
            clear_camera_angle(world, Some(&id));
            player_physics::clear(world,Some(&id));
            mods.custom_menus.retain(|(owner,_),_|owner!=&id);
            crate::retail_menus::clear_owner(world, &id);
            continue;
        }
        commands.sort_by_key(|command| match command {
            Command::PlayerDetach { .. } => 0,
            Command::PhysicsRemove { .. } | Command::PhysicsRemoveJoint { .. } => 1,
            Command::PhysicsSpawn { .. } | Command::CameraCapture { .. } => 2,
            _ => 3,
        });
        let result = (|| {
            for command in commands {
                apply_one(world, mods, &id, command)?;
            }
            Ok::<(), String>(())
        })();
        if let Err(e) = result {
            warn!("Lua mod {id}: {e}");
            engine_access::restore_gates(world,mods,Some(&id));
            mods.manager.fail(&id, e);
            mods.input_overrides.retain(|_,(owner,_)|owner!=&id);
            audio::stop_owner(world, &id, true);
            world_audio::clear_owner(world, &id);
            crate::game_audio::clear_mod(world, &id);
            graphics_dynamic::clear_owner(world, &id);
            volumes::clear_owner(world, mods, &id);
            triggers::clear_owner(world, &id);
            capture::clear_owner(world, &id);
            clear_camera_angle(world, Some(&id));
            player_physics::clear(world,Some(&id));
            mods.custom_menus.retain(|(owner,_),_|owner!=&id);
            crate::retail_menus::clear_owner(world, &id);
        }
    }
    sync_audio_content(world, mods);
    let menu_events = world.get_resource_mut::<crate::retail_menus::RetailMenus>().map(|mut m| m.drain_events()).unwrap_or_default();
    for (owner, payload) in menu_events {
        mods.manager.call(&owner, "on_event", payload);
    }
    let mut row = 0;
    for entity in mods.overlays.values() {
        if let Some(mut node) = world.get_mut::<Node>(*entity) {
            let top=px(16. + row as f32 * 28.);
            if node.top!=top {node.top=top;}
            row += 1;
        }
    }
}

fn apply_one(
    world: &mut World,
    mods: &mut Mods,
    id: &str,
    command: Command,
) -> Result<(), String> {
    match command {
        Command::RigPart {index,options} => player_physics::set_part(world,id,index,options)?,
        Command::GraphGate {graph,target,index,enabled} => engine_access::gate(world,mods,id,graph,target,index,enabled)?,
        Command::EngineInspect {..} => return Err("use commands.request for engine inspection results".into()),
        Command::Request {key,command,token} => {
            let slot=(id.to_owned(),key);
            if !mods.command_results.contains_key(&slot) && mods.command_results.keys().filter(|(o,_)|o==id).count()>=64 {
                return Err("64 command result slots per mod maximum".into());
            }
            let (result,value)=if let Command::EngineInspect {system}=*command {
                (Ok(()),engine_access::inspect(world,&system))
            } else {(apply_one(world,mods,id,*command),Value::Null)};
            let tick=world.resource::<crate::physics::GamePhysics>().ticks;
            mods.command_results.insert(slot,json!({"token":token,"ok":result.is_ok(),"error":result.err(),"value":value,"tick":tick}));
        }
        Command::InputOverride {action,value} => {
            if mods.input_overrides.get(&action).is_some_and(|(o,_)|o!=id) { return Err("input action owned by another mod".into()); }
            if let Some(value)=value {mods.input_overrides.insert(action,(id.to_owned(),value));} else {mods.input_overrides.remove(&action);}
        }
        Command::NativeImpulse {body,impulse,point,angular} => {
            if mods.attach.is_some() || !mods.suspended_by.is_empty() {return Err("native player is attached or suspended".into());}
            player_physics::impulse(world,&body,impulse,point,angular)?;
        }
        Command::UiCanvas { key, options } => canvas::set(world, &mut mods.canvases, id, key, options)?,
        Command::UiRemove { key } => {
            let slot = (id.to_owned(), key);
            canvas::remove(world, &mut mods.canvases, &slot);
            if let Some(e) = mods.overlays.remove(&slot) { world.despawn(e); }
        }
        Command::CameraRig { body, options } => {
            resolve_body(mods, id, &body)?;
            mods.camera.claim(id)?;
            mods.camera.follow = None;
            mods.camera.fixed = None;
            mods.camera.watch = None;
            if let Some(rig) = mods.camera.rig.as_mut().filter(|r| r.body == body) {
                rig.configure(options);
            } else {
                mods.camera.rig = Some(vehicle_camera::Rig::new(body, options));
            }
        }
        Command::AudioPreload { path } => audio::preload(world, mods, id, &path)?,
        Command::AudioPlay { key, options } => audio::play(world, mods, id, key, options)?,
        Command::AudioUpdate { key, options } => audio::update_voice(world, id, &key, options),
        Command::AudioStop { key, fade_out } => audio::stop(world, id, &key, fade_out),
        Command::AudioStopAll {} => audio::stop_owner(world, id, false),
        Command::AudioFrontend { name } => audio::frontend(world, &name),
        Command::AudioTeleportEffect { amount } => audio::teleport_effect(world, amount),
        Command::AudioPost { key, class, words } => audio_api(world, |api, native, _| api.post(native, id, &key, &class, &words))?,
        Command::AudioRedeliver { key, words } => audio_api(world, |api, _, _| api.redeliver(id, &key, &words))?,
        Command::AudioRelease { key } => audio_api(world, |api, _, _| {
            api.release(id, &key);
            Ok(())
        })?,
        Command::AudioSetGlobal { name, value } => audio_api(world, |api, native, _| api.set_global(native, id, &name, value))?,
        Command::AudioWatch { globals, mixmap } => {
            let mixmap: Vec<_> = mixmap.into_iter().map(|k| (k.slot, k.object, k.instance, k.output)).collect();
            audio_api(world, |api, native, _| api.watch(native, id, &globals, &mixmap))?
        }
        Command::AudioSubscribe { tags } => audio_api(world, |api, native, _| api.subscribe(id, tags, native))?,
        Command::AudioSetTuning { domain, patch } => crate::game_audio::set_tuning(world, id, &domain, patch)?,
        Command::AudioSetMixmapInput { slot, object, instance, input, value, float } => {
            use crate::game_audio::mixmap_inputs::InputValue;
            let value = value.map(|v| if float { InputValue::Float(v as f32) } else { InputValue::Word(v as i32) });
            crate::game_audio::set_mixmap_input(world, id, &slot, object, instance, input, value)?
        }
        Command::AudioSeed { seed } => crate::game_audio::set_seed(world, id, seed)?,
        Command::AudioRule { key, rule } => crate::game_audio::set_rule(world, id, &key, rule, |world, path| audio::load_native_clip(world, mods, id, path))?,
        Command::WorldAudioSpawn { key, object, options } => world_audio::spawn(world, mods, id, key, object, options)?,
        Command::WorldAudioUpdate { key, options } => world_audio::update(world, mods, id, &key, options)?,
        Command::WorldAudioEvent { key, event, options } => world_audio::event(world, id, &key, &event, options)?,
        Command::WorldAudioRemove { key } => world_audio::remove(world, id, &key),
        Command::WorldAudioAnnouncer { character } => world_audio::announcer(world, id, character)?,
        Command::WorldAudioAnnounce { event, options } => world_audio::announce(world, &event, options)?,
        Command::WorldAudioAlarmRule { options } => world_audio::alarm_rule(world, id, options)?,
        Command::GraphicsMeshBuffer { key, options } => {
            graphics_dynamic::mesh_buffer(world, mods, id, key, options)?;
        }
        Command::GraphicsMeshBufferWrite { key, data } => {
            graphics_dynamic::mesh_buffer_write(world, mods, id, &key, data)?;
        }
        Command::GraphicsMeshBufferAppend { key, data } => {
            graphics_dynamic::mesh_buffer_append(world, mods, id, &key, data)?;
        }
        Command::GraphicsLight { key, options } => graphics_dynamic::light(world, mods, id, key, options)?,
        Command::Log { text } => info!("Lua [{id}]: {text}"),
        Command::MultiplayerDebug { key, text } => {
            let slot = (id.to_owned(), key);
            if text.is_empty() { mods.multiplayer_debug.remove(&slot); }
            else {
                if !mods.multiplayer_debug.contains_key(&slot)
                    && mods.multiplayer_debug.keys().filter(|(owner,_)| owner == id).count() >= 8 {
                    return Err("8 multiplayer debug entries per mod maximum".into());
                }
                mods.multiplayer_debug.insert(slot, text);
            }
        }
        Command::Overlay { key, text } => {
            let k = (id.to_owned(), key);
            // Empty overlay text removes its row rather than leaving a blank slot.
            if text.is_empty() {
                if let Some(e) = mods.overlays.remove(&k) { world.despawn(e); }
                return Ok(());
            }
            if let Some(entity) = mods.overlays.get(&k) {
                if let Some(mut t) = world.get_mut::<Text>(*entity) {
                    if t.0!=text {**t = text;}
                    return Ok(());
                }
            }
            if let Some(e) = mods.overlays.remove(&k) {
                world.despawn(e);
            }
            let entity = world
                .spawn((
                    Text::new(text),
                    TextFont {
                        font_size: 19.,
                        ..default()
                    },
                    TextColor(Color::WHITE),
                    GlobalZIndex(3),
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(16.),
                        ..default()
                    },
                ))
                .id();
            mods.overlays.insert(k, entity);
        }
        Command::PhysicsSpawn { key, body } => {
            mods.native_snapshot = None;
            let k = (id.to_owned(), key);
            if let Some(old) = mods.bodies.remove(&k) {
                mods.world.remove(old);
            }
            if mods.bodies.keys().filter(|(o, _)| o == id).count() >= 64 {
                return Err("64 physics bodies per mod maximum".into());
            }
            let body = resolve_body_desc(mods, id, body)?;
            let body_id = mods.world.spawn(body)?;
            mods.bodies.insert(k, body_id);
        }
        Command::PhysicsRemove { key } => {
            mods.native_snapshot = None;
            if mods.attach.as_ref().is_some_and(|a|a.owner==id && a.body==key) { detach_player(world,mods,true); }
            let bound:Vec<_>=mods.graphics.iter().filter(|((o,_),g)|o==id && g.body.as_deref()==Some(key.as_str())).map(|(k,_)|k.clone()).collect();
            for slot in bound { retire_graphics(world,mods,&slot); }
            if mods.camera.follows(id, &key) { mods.camera.clear(); }
            audio::stop_body(world, id, &key);
            graphics_dynamic::remove(world, id, &key);
            let k = (id.to_owned(), key);
            if let Some(body) = mods.bodies.remove(&k) {
                mods.world.remove(body);
            }
        }
        Command::PhysicsAddCollider {
            key,
            shape,
            position,
            friction,
        } => {
            let body = resolve_body(mods, id, &key)?;
            let points = resolve_convex_points(mods, id, shape)?;
            mods.world
                .add_convex_collider(body, &points, position, friction)?;
        }
        Command::PhysicsForce { key, force, point } => {
            let body = resolve_body(mods, id, &key)?;
            if !mods.world.apply_force(body, force, point) {
                return Err(format!("force failed for {key}"));
            }
        }
        Command::PhysicsImpulse {
            key,
            impulse,
            point,
        } => {
            let body = resolve_body(mods, id, &key)?;
            if !mods.world.apply_impulse(body, impulse, point) {
                return Err(format!("impulse failed for {key}"));
            }
        }
        Command::PhysicsTorque { key, torque } => {
            let body = resolve_body(mods, id, &key)?;
            if !mods.world.apply_torque(body, torque) {
                return Err(format!("torque failed for {key}"));
            }
        }
        Command::PhysicsTorqueImpulse { key, torque } => {
            let body = resolve_body(mods, id, &key)?;
            if !mods.world.apply_torque_impulse(body, torque) {
                return Err(format!("torque_impulse failed for {key}"));
            }
        }
        Command::PhysicsSetLinvel { key, linvel } => {
            let body = resolve_body(mods, id, &key)?;
            if !mods.world.set_linvel(body, linvel) {
                return Err(format!("set_linvel failed for {key}"));
            }
        }
        Command::PhysicsSetAngvel { key, angvel } => {
            let body = resolve_body(mods, id, &key)?;
            if !mods.world.set_angvel(body, angvel) {
                return Err(format!("set_angvel failed for {key}"));
            }
        }
        Command::PhysicsSetPose {
            key,
            position,
            rotation,
        } => {
            let body = resolve_body(mods, id, &key)?;
            if !mods.world.set_pose(body, position, rotation) {
                return Err(format!("set_pose failed for {key}"));
            }
        }
        Command::PhysicsRevolute {
            key,
            body_a,
            body_b,
            anchor_a,
            anchor_b,
            axis,
            limits,
            contacts_enabled,
        } => {
            let a = resolve_body(mods, id, &body_a)?;
            let b = resolve_body(mods, id, &body_b)?;
            let k = (id.to_owned(), key);
            if let Some(old) = mods.joints.remove(&k) {
                mods.world.remove_joint(old);
            }
            let jid = mods.world.add_revolute_joint(RevoluteJointDesc {
                body_a: a,
                body_b: b,
                anchor_a,
                anchor_b,
                axis,
                limits,
                contacts_enabled,
            })?;
            mods.joints.insert(k, jid);
        }
        Command::PhysicsJointMotor { key, motor } => {
            let j = *mods
                .joints
                .get(&(id.to_owned(), key.clone()))
                .ok_or_else(|| format!("unknown joint {key}"))?;
            if !mods.world.set_joint_motor(j, motor) {
                return Err(format!("joint motor failed for {key}"));
            }
        }
        Command::PhysicsPrismatic {
            key,
            body_a,
            body_b,
            anchor_a,
            anchor_b,
            axis,
            limits,
            contacts_enabled,
        } => {
            let a = resolve_body(mods, id, &body_a)?;
            let b = resolve_body(mods, id, &body_b)?;
            let k = (id.to_owned(), key);
            if let Some(old) = mods.joints.remove(&k) {
                mods.world.remove_joint(old);
            }
            let jid = mods.world.add_prismatic_joint(skate_dynamics::PrismaticJointDesc {
                body_a: a,
                body_b: b,
                anchor_a,
                anchor_b,
                axis,
                limits,
                contacts_enabled,
            })?;
            mods.joints.insert(k, jid);
        }
        Command::PhysicsJointSpring { key, spring } => {
            let j = *mods
                .joints
                .get(&(id.to_owned(), key.clone()))
                .ok_or_else(|| format!("unknown joint {key}"))?;
            if !mods.world.set_joint_spring(j, spring) {
                return Err(format!("joint spring failed for {key}"));
            }
        }
        Command::PhysicsRemoveJoint { key } => {
            let k = (id.to_owned(), key);
            if let Some(j) = mods.joints.remove(&k) {
                mods.world.remove_joint(j);
            }
        }
        Command::GraphicsMesh { key,path,body,position,rotation,scale,color,visible,opacity,deform_nodes } => {
            graphics::spawn(world,mods,id,id,key,
                skate_mods::scene::GraphicsDefinition { path,body,color,opacity,deform_nodes },
                skate_mods::scene::TransformState { position:position.unwrap_or([0.;3]),rotation:rotation.unwrap_or([0.,0.,0.,1.]),scale },visible,None)?;
        }
        Command::GraphicsTransform { key, options } => {
            let owned=mods.graphics.get_mut(&(id.to_owned(),key)).ok_or("unknown graphics key")?;
            owned.transform.apply(&options);
        }
        Command::GraphicsNode { key,node,options } => graphics::set_node(mods,id,&key,node,options)?,
        Command::GraphicsResetNode { key,node } => graphics::reset_node(world,mods,id,&key,&node),
        Command::PhysicsDebug { enabled } => {
            if enabled { mods.debug_owners.insert(id.to_owned()); }
            else { mods.debug_owners.remove(id); }
        }
        Command::GraphicsRemove { key } => {
            graphics_dynamic::remove(world, id, &key);
            retire_graphics(world, mods, &(id.to_owned(), key));
        }
        Command::GraphicsVisibility { key,visible } => {
            if let Some(owned)=mods.graphics.get_mut(&(id.to_owned(),key)) { owned.visible=visible; }
        }
        Command::PlayerAttach { body,offset } => attachment::attach(world,mods,id,body,offset)?,
        Command::PlayerDetach { options } => {
            if mods.attach.as_ref().is_some_and(|a|a.owner==id) { attachment::detach(world,mods,false,&options); }
        }
        Command::CameraFollow { body, offset } => {
            if let Some(body) = body {
                resolve_body(mods, id, &body)?;
                mods.camera.claim(id)?;
                mods.camera.fixed = None;
                mods.camera.rig = None;
                mods.camera.watch = None;
                mods.camera.follow = Some((id.to_owned(), body, Vec3::from_array(offset)));
            } else if mods.camera.owner.as_deref() == Some(id) {
                mods.camera.clear();
            }
        }
        Command::CameraSet { position, look_at } => {
            mods.camera.claim(id)?;
            mods.camera.rig = None;
            mods.camera.follow = None;
            mods.camera.watch = None;
            mods.camera.fixed = Some((
                Vec3::from_array(position),
                look_at.map(Vec3::from_array),
            ));
        }
        Command::PlayerSuspend { suspended } => {
            if suspended { mods.suspended_by.insert(id.to_owned()); }
            else { mods.suspended_by.remove(id); }
        }
        Command::CameraAngle { angle } => {
            world.resource_mut::<crate::camera::CameraAngleSettings>().force(id, angle.map(Into::into))?;
        }
        Command::CameraShotTune { shot, patch } => {
            if !world.resource::<crate::camera::CameraRuntime>().has_shot(&shot) {
                return Err(format!("unknown camera shot {shot}"));
            }
            world.resource_mut::<crate::camera::CameraAngleSettings>().tune(id, &shot, patch)?;
        }
        Command::CameraWatch { peer } => {
            let empty = peer.as_deref().is_none_or(|p| p.is_empty());
            if empty {
                if mods.camera.owner.as_deref() == Some(id) {
                    mods.camera.watch = None;
                    if mods.camera.follow.is_none()
                        && mods.camera.fixed.is_none()
                        && mods.camera.rig.is_none()
                    {
                        mods.camera.clear();
                    }
                }
            } else {
                let peer = peer.unwrap();
                let pid = peer.parse::<u64>().map_err(|_| "invalid peer")?;
                mods.camera.claim(id)?;
                mods.camera.follow = None;
                mods.camera.fixed = None;
                mods.camera.rig = None;
                mods.camera.watch = Some(pid);
            }
        }
        Command::NetworkState { key, value } => {
            let slot = (id.to_owned(), key);
            if value.is_null() {
                mods.net_states.remove(&slot);
            } else {
                if mods.net_states.len() >= 128 && !mods.net_states.contains_key(&slot) {
                    return Err("128 shared net states maximum".into());
                }
                mods.net_states.insert(slot, value);
            }
        }
        Command::UiMenu {key,options} => {
            let slot=(id.to_owned(),key);
            if !mods.custom_menus.contains_key(&slot) && mods.custom_menus.keys().filter(|(o,_)|o==id).count()>=8 {return Err("8 menus per mod maximum".into());}
            if options.section.is_some() {
                let others: Vec<_> = mods.custom_menus.iter().filter(|(k,v)| **k != slot && v.section.is_some()).collect();
                let mut sections: BTreeSet<_> = others.iter().filter_map(|(_,v)| v.section.as_deref()).collect();
                sections.insert(options.section.as_deref().unwrap());
                if others.len() >= 64 || sections.len() > 8 { return Err("64 section menus and 8 custom sections maximum".into()); }
            }
            mods.custom_menus.insert(slot,options);
        }
        Command::UiRemoveMenu {key} => {mods.custom_menus.remove(&(id.to_owned(),key));}
        Command::PlayerJoint {joint,options} => player_physics::set(world,id,joint,Some(options))?,
        Command::PlayerResetJoint {joint} => player_physics::set(world,id,joint,None)?,
        Command::PlayerResetJoints {} => player_physics::clear(world,Some(id)),
        Command::PlayerTeleport { options } => session::apply_local(world, &options)?,
        Command::SessionClaim {} => session::claim(world, mods)?,
        Command::SessionTransfer { peer } => session::transfer(world, mods, &peer)?,
        Command::SessionTeleport { peer, options } => session::teleport(world, mods, &peer, options)?,
        Command::VolumeBox { key, options } => volumes::set(world, mods, id, key, options)?,
        Command::VolumeRemove { key } => volumes::remove(world, mods, id, &key),
        Command::TriggerBox { key, options } => triggers::set_box(world, id, key, options)?,
        Command::TriggerRemove { key } => triggers::remove_box(world, id, &key),
        Command::TriggerEnable { id: volume, enabled } => triggers::enable(world, id, &volume, enabled)?,
        Command::TriggerTrack { key, options } => triggers::track(world, mods, id, key, options)?,
        Command::TriggerUntrack { key } => triggers::untrack(world, id, &key),
        Command::TriggerConfigure { options } => triggers::configure(world, id, options)?,
        Command::CameraCapture { key, options } => { capture::set(world, id, key, options)?; }
        c @ (Command::MenuCategory { .. } | Command::MenuItem { .. } | Command::MenuSetting { .. } | Command::MenuHide { .. }
            | Command::MenuHandle { .. } | Command::MenuUnhandle { .. } | Command::MenuSetValue { .. }) => {
            let mut menus = world.get_resource_mut::<crate::retail_menus::RetailMenus>().ok_or("menus are not ready")?;
            crate::retail_menus::apply_command(&mut menus, id, c)?;
        }
        Command::MenuMovie { name, path } => {
            let package = mods.manager.packages.get(id).ok_or("missing package")?;
            let root = package.root.canonicalize().map_err(|e| e.to_string())?;
            let full = root.join(&path).canonicalize().map_err(|e| e.to_string())?;
            if !full.starts_with(&root) { return Err("menu movie escapes its package".into()); }
            if std::fs::metadata(&full).map_err(|e| e.to_string())?.len() > crate::retail_menu_movies::MAX_MOD_MOVIE_BYTES {
                return Err("menu movie file is too large".into());
            }
            let json = serde_json::from_slice(&std::fs::read(&full).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            let mut movies = world.get_resource_mut::<crate::retail_menu_movies::RetailMenuMovies>().ok_or("menu movies are not ready")?;
            movies.set_mod_movie(id, &name, json)?;
        }
        Command::MenuBitmap { movie, bitmap, path } => {
            let package = mods.manager.packages.get(id).ok_or("missing package")?;
            let input = skate_mods::read_bounded(&package.root, &path, crate::retail_menu_movies::MAX_MOD_BITMAP_BYTES)?;
            let image = crate::retail_menu_movies::decode_mod_bitmap(&input)?;
            let mut movies = world.get_resource_mut::<crate::retail_menu_movies::RetailMenuMovies>().ok_or("menu movies are not ready")?;
            movies.set_mod_bitmap(id, &movie, bitmap, image)?;
        }
        Command::CameraClearCapture { key } => capture::remove(world, id, &key),
    }
    Ok(())
}

/// The runtime audio API (`game_audio::mod_audio`) for a mod command.
fn audio_api(world: &mut World, f: impl FnOnce(&mut crate::game_audio::mod_audio::AudioApi, Option<&crate::game_audio::Native>, u64) -> Result<(), String>) -> Result<(), String> {
    crate::game_audio::with_api(world, f).unwrap_or_else(|| Err("game audio is unavailable".into()))
}

fn resolve_body(mods: &Mods, owner: &str, key: &str) -> Result<u64, String> {
    mods.bodies
        .get(&(owner.to_owned(), key.to_owned()))
        .copied()
        .ok_or_else(|| format!("unknown body {key}"))
}

fn resolve_body_desc(mods: &Mods, owner: &str, mut body: BodyDesc) -> Result<BodyDesc, String> {
    match &body.shape {
        Shape::Mesh { .. } => {
            let points = resolve_convex_points(mods, owner, body.shape.clone())?;
            body.shape = Shape::Convex { points };
        }
        Shape::Model { path, object, options } => {
            let package = mods.manager.packages.get(owner).ok_or("missing package")?;
            if path.is_empty() || !skate_mods::scene::valid_asset(path) {
                return Err("invalid model GLB path".into());
            }
            let root = package.root.canonicalize().map_err(|e| e.to_string())?;
            let full = root.join(path).canonicalize().map_err(|e| e.to_string())?;
            if !full.starts_with(&root) { return Err("model GLB escapes its package".into()); }
            body.shape = skate_mods::model_shape_file(&full, object, options)?;
        }
        _ => {}
    }
    Ok(body)
}

fn resolve_convex_points(mods: &Mods, owner: &str, shape: Shape) -> Result<Vec<[f32; 3]>, String> {
    match shape {
        Shape::Convex { points } => {
            if points.len() < 4 || points.len() > 512 {
                return Err("convex needs 4..=512 points".into());
            }
            Ok(points)
        }
        Shape::Mesh { path, object } => {
            let package = mods
                .manager
                .packages
                .get(owner)
                .ok_or("missing package")?;
            if !skate_mods::scene::valid_asset(&path) || path.is_empty() { return Err("invalid collider GLB path".into()); }
            let root=package.root.canonicalize().map_err(|e|e.to_string())?;
            let full=root.join(&path).canonicalize().map_err(|e|e.to_string())?;
            if !full.starts_with(&root) { return Err("collider GLB escapes its package".into()); }
            glb::convex_points(&full, &object)
        }
        _ => Err("add_collider shape must be mesh or convex".into()),
    }
}

fn sync_graphics(world: &mut World, mods: &mut Mods) { graphics::sync(world,mods); }

fn sync_attach(world:&mut World,mods:&mut Mods) { attachment::sync(world,mods); }
fn detach_if_owner(world:&mut World,mods:&mut Mods,owner:&str) {
    if mods.attach.as_ref().is_some_and(|a|a.owner==owner) { detach_player(world,mods,true); }
}
fn detach_player(world:&mut World,mods:&mut Mods,forced:bool) {
    attachment::detach(world,mods,forced,&skate_mods::scene::DetachOptions::default());
}

fn sync_net(world: &mut World) {
    world.resource_scope(|world, mut mods: Mut<Mods>| {
        let packages: BTreeMap<_, _> = mods
            .manager
            .packages
            .iter()
            .filter(|(_, p)| p.running())
            .map(|(id, p)| (id.clone(), p.content_fingerprint()))
            .collect();
        mods.net_states
            .retain(|(owner, _), _| packages.contains_key(owner));
        mods.session.connect(world.get_resource::<crate::multiplayer::Multiplayer>().and_then(|n| n.session_identity()));
        let obs_bytes = observation::encode_local(world, player_suspended(&mods));
        let view_bytes = camera_stream::capture(world);

        let Some(mut net) = world.get_resource_mut::<crate::multiplayer::Multiplayer>() else {
            mods.net_published.clear();
            mods.net_remote.clear();
            mods.net_remote_wire.clear();
            mods.net_status.clear();
            mods.skater_remote.clear();
            mods.remote_cameras.clear();
            return;
        };
        if !net.active() {
            mods.net_published.clear();
            mods.net_remote.clear();
            mods.net_remote_wire.clear();
            mods.net_status.clear();
            mods.skater_remote.clear();
            mods.remote_cameras.clear();
            return;
        }

        let (_, local, is_host) = net.mod_identity();
        let host_peer = net.host_actor().to_string();
        let local_id = local.to_string();
        let mut published = BTreeSet::new();
        let mut failures = 0u32;
        for ((owner, key), value) in &mods.net_states {
            let Some(&fingerprint) = packages.get(owner) else {
                continue;
            };
            let wire = net_wire_key(owner, key);
            let bytes = match serde_json::to_vec(&NetWire {
                owner: owner.clone(),
                fingerprint,
                key: key.clone(),
                value: value.clone(),
            }) {
                Ok(b) if b.len() <= skate_net::lobby::MAX_APP_VALUE => b,
                _ => {
                    failures += 1;
                    continue;
                }
            };
            if !net.publish_application(&wire, bytes) {
                failures += 1;
            }
            published.insert(wire);
        }
        if let Some(bytes) = obs_bytes {
            if net.publish_application(observation::OBS_KEY, bytes) {
                published.insert(observation::OBS_KEY.to_owned());
            } else {
                failures += 1;
            }
        }
        if let Some(bytes) = view_bytes {
            if net.publish_application(camera_stream::KEY, bytes) { published.insert(camera_stream::KEY.into()); }
        }
        for (key, bytes) in session::outgoing(&mods, &local_id, is_host, &host_peer) {
            if net.publish_application(&key, bytes) {
                published.insert(key);
            } else {
                failures += 1;
            }
        }
        for key in mods.net_published.difference(&published).cloned().collect::<Vec<_>>() {
            net.publish_application(&key, vec![]);
        }
        mods.net_published = published;

        let records = net.application_records();
        let players = net.player_ids();
        let host = net.host_actor();
        drop(net);
        camera_stream::ingest(&mut mods.remote_cameras, &records, &players);
        session::ingest(&mut mods, &records, local, host, &players);
        session::apply_pending_remote(world, &mut mods);

        let mut live_wire = BTreeSet::new();
        let mut mismatches = BTreeSet::new();
        for (peer, wire, _seq, bytes) in records {
            if !wire.starts_with("net:") {
                continue;
            }
            if bytes.is_empty() {
                if let Some((owner, key)) = mods.net_remote_wire.remove(&(peer, wire)) {
                    mods.net_remote.remove(&(peer, owner, key));
                }
                continue;
            }
            let Ok(record) = serde_json::from_slice::<NetWire>(&bytes) else {
                continue;
            };
            let probe = Command::NetworkState {
                key: record.key.clone(),
                value: record.value.clone(),
            };
            if !probe.validate() {
                continue;
            }
            let Some(&fp) = packages.get(&record.owner) else {
                mismatches.insert(record.owner);
                continue;
            };
            if fp != record.fingerprint {
                mismatches.insert(record.owner);
                continue;
            }
            if record.value.is_null() {
                mods.net_remote
                    .remove(&(peer, record.owner.clone(), record.key.clone()));
                mods.net_remote_wire.remove(&(peer, wire));
                continue;
            }
            mods.net_remote.insert(
                (peer, record.owner.clone(), record.key.clone()),
                record.value,
            );
            mods.net_remote_wire
                .insert((peer, wire.clone()), (record.owner, record.key));
            live_wire.insert((peer, wire));
        }

        let stale: Vec<_> = mods
            .net_remote_wire
            .keys()
            .filter(|id| !live_wire.contains(*id))
            .cloned()
            .collect();
        for id in stale {
            if let Some((owner, key)) = mods.net_remote_wire.remove(&id) {
                mods.net_remote.remove(&(id.0, owner, key));
            }
        }
        mods.net_remote
            .retain(|(_, owner, _), _| packages.contains_key(owner));

        mods.net_status = if failures > 0 {
            format!("Mod network limit reached: {failures} records")
        } else if !mismatches.is_empty() {
            format!(
                "Mods missing, disabled or different: {}",
                mismatches.into_iter().collect::<Vec<_>>().join(", ")
            )
        } else {
            format!(
                "Matching mods synchronized | {} remote net keys",
                mods.net_remote.len()
            )
        };
        let solid_status=mods.replication.status.clone();
        mods.net_status.push_str(" | ");mods.net_status.push_str(&solid_status);
    });
}

fn camera_sample(mods: &Mods, owner: &str, body: &str) -> Option<vehicle_camera::Sample> {
    let id = *mods.bodies.get(&(owner.to_owned(), body.to_owned()))?;
    let snap = mods.world.read(id)?;
    Some(vehicle_camera::Sample {
        position: Vec3::from_array(snap.position),
        rotation: Quat::from_xyzw(snap.rotation[0], snap.rotation[1], snap.rotation[2], snap.rotation[3]).normalize(),
        velocity: Vec3::from_array(snap.linvel),
    })
}
fn record_camera(mods: &mut Mods, dt: f32) {
    let sample = mods.camera.owner.as_ref().and_then(|o| {
        mods.camera.rig.as_ref().and_then(|r| camera_sample(mods, o, &r.body))
    });
    if let Some(sample) = sample {
        if let Some(r) = mods.camera.rig.as_mut() { r.record(sample, dt); }
    } else if mods.camera.rig.is_some() { mods.camera.clear(); }
}

fn present_camera(world: &mut World) {
    let camera = world.query_filtered::<Entity, With<crate::camera::GameplayCamera>>()
        .iter(world).next();
    let Some(camera) = camera else { return; };
    let replay = world.resource::<crate::replay::Replay>().active;
    let debug_cam = crate::debug_cam::DebugCam::active(world.resource::<crate::camera::CameraRuntime>());
    let customizing = world.get_resource::<crate::customiser::Customiser>().is_some_and(|c| c.open);
    let paused = world.resource::<crate::graphics_menu::Menu>().open;
    let dt = if paused {0.0} else {world.resource::<Time<Real>>().delta_secs().clamp(0.0,0.1)};
    let alpha = if paused {1.0} else {world.resource::<Time<Fixed>>().overstep_fraction()};
    world.resource_scope(|world, mut mods: Mut<Mods>| {
        let suppress = replay || customizing || debug_cam;
        if mods.camera.rig.is_none() || suppress {
            if let Some(near) = mods.camera.saved_near.take() {
                if let Some(mut projection) = world.get_mut::<Projection>(camera) {
                    if let Projection::Perspective(p) = &mut *projection { p.near = near; }
                }
            }
        }
        if suppress { return; }
        if let Some(peer) = mods.camera.watch {
            let local_id = world
                .get_resource::<crate::multiplayer::Multiplayer>()
                .map(|n| n.mod_identity().1)
                .unwrap_or(0);
            if peer != local_id {
                if let Some(view) = mods.remote_cameras.get(&peer) {
                    if let Some(mut t) = world.get_mut::<Transform>(camera) { *t = view.transform(); }
                    if let Some(mut p) = world.get_mut::<Projection>(camera) {
                        if let Projection::Perspective(p) = &mut *p { p.fov = view.fov; }
                    }
                    return;
                }
                if let Some((position, rotation)) = watch_pose(world, peer, local_id) {
                    if let Some(mut t) = world.get_mut::<Transform>(camera) {
                        *t = spectator_view(position, rotation);
                    }
                    return;
                }
            } else {
                mods.camera.watch = None;
                if mods.camera.follow.is_none()
                    && mods.camera.fixed.is_none()
                    && mods.camera.rig.is_none()
                {
                    mods.camera.clear();
                }
            }
        }
        if let Some(mut rig) = mods.camera.rig.take() {
            let owner = mods.camera.owner.clone().unwrap_or_default();
            // A rig may be created during on_update, between fixed steps.
            if rig.sample(alpha).is_none() {
                if let Some(s) = camera_sample(&mods, &owner, &rig.body) {
                    rig.record(s, world.resource::<Time<Fixed>>().timestep().as_secs_f32());
                }
            }
            if let Some(sample) = rig.sample(alpha) {
                let view = rig.view(sample, dt, &mut mods.world);
                // Match camera and owned car visuals to the SAME interpolation sample.
                // Physics remains at the native current pose; only Transform is changed.
                let entities: Vec<_> = mods.graphics.iter()
                    .filter(|((o,_),g)| o == &owner && g.body.as_ref() == Some(&rig.body))
                    .map(|(_,g)|(g.entity,g.transform.clone())).collect();
                for (e,local) in entities {
                    if let Some(mut t) = world.get_mut::<Transform>(e) {
                        t.translation = sample.position + sample.rotation*Vec3::from_array(local.position);
                        t.rotation = (sample.rotation*Quat::from_array(local.rotation).normalize()).normalize();
                        t.scale=Vec3::from_array(local.scale);
                    }
                }
                if let Some(mut transform) = world.get_mut::<Transform>(camera) { *transform = view.transform; }
                if let Some(mut projection) = world.get_mut::<Projection>(camera) {
                    if let Projection::Perspective(p) = &mut *projection {
                        if mods.camera.saved_near.is_none() { mods.camera.saved_near = Some(p.near); }
                        p.near = view.near; p.fov = view.fov;
                    }
                }
            }
            mods.camera.rig = Some(rig);
            return;
        }
        let follow = mods.camera.follow.clone();
        if let Some((owner, body, offset)) = follow {
            if let Some(sample) = camera_sample(&mods, &owner, &body) {
                let eye = sample.position + sample.rotation * offset;
                if let Some(mut t) = world.get_mut::<Transform>(camera) {
                    *t = Transform::from_translation(eye).looking_at(sample.position + Vec3::Y * 0.5, Vec3::Y);
                }
            } else { mods.camera.clear(); }
        } else if let Some((position, look_at)) = mods.camera.fixed {
            if let Some(mut t) = world.get_mut::<Transform>(camera) {
                if let Some(target) = look_at { *t = Transform::from_translation(position).looking_at(target, Vec3::Y); }
                else { t.translation = position; }
            }
        }
    });
}

fn spectator_view(position: Vec3, rotation: Quat) -> Transform {
    let eye = position + rotation * Vec3::new(0.0, 2.4, -7.2);
    Transform::from_translation(eye).looking_at(position + Vec3::Y * 1.15, Vec3::Y)
}

fn watch_pose(world: &mut World, peer: u64, local_id: u64) -> Option<(Vec3, Quat)> {
    if peer == local_id {
        let obs = observation::local(world);
        return Some((Vec3::from_array(obs.p), Quat::from_array(obs.r).normalize()));
    }
    let from_actor = {
        let mut actors = world.query::<(&crate::multiplayer::NetworkActor, &Transform)>();
        actors
            .iter(world)
            .find(|(actor, _)| actor.0 == peer)
            .map(|(_, transform)| (transform.translation, transform.rotation.normalize()))
    };
    if from_actor.is_some() {
        return from_actor;
    }
    world
        .get_resource::<crate::multiplayer::Multiplayer>()
        .and_then(|net| net.actor_pose(peer))
}

impl Mods {
    /// A fresh host for a mod manager (the plugin's; tests build one over a temporary folder).
    pub(crate) fn new(manager: Manager) -> Self {
        Self {
            manager,
            native_snapshot: None,
            world: DynamicsWorld::default(),
            bodies: BTreeMap::new(),
            joints: BTreeMap::new(),
            graphics: BTreeMap::new(),
            overlays: BTreeMap::new(),
            canvases: BTreeMap::new(),
            attach: None,
            detach_error: None,
            detach_pending: None,
            camera: CameraOverride::default(),
            generation: u64::MAX,
            graphics_serial: 0,
            debug_owners: BTreeSet::new(),
            ground_ready: false,
            skater_proxies: BTreeMap::new(),
            dyn_published: BTreeSet::new(),
            replication: replication::State::default(),
            net_states: BTreeMap::new(),
            net_published: BTreeSet::new(),
            net_remote: BTreeMap::new(),
            net_remote_wire: BTreeMap::new(),
            net_status: String::new(),
            multiplayer_debug: BTreeMap::new(),
            last_contacts: Vec::new(),
            graph_gates: BTreeMap::new(),
            command_results: BTreeMap::new(), input_overrides: BTreeMap::new(),
            session: session::Runtime::default(),
            skater_remote: BTreeMap::new(),
            pending_remote_teleport: None,
            volumes: BTreeMap::new(),
            custom_menus: BTreeMap::new(),
            suspended_by: BTreeSet::new(),
            hidden_players: BTreeMap::new(),
            remote_cameras: BTreeMap::new(),
        }
    }

    pub(crate) fn multiplayer_debug_sections(&self) -> [String; 3] {
        let sync = format!("MOD REPLICATION\n{}\nLocal network keys: {} | Remote keys: {}\nPublished dynamics: {} | Local bodies: {} | Local scenes: {}",
            if self.net_status.is_empty() { "No active mod replication" } else { &self.net_status },
            self.net_states.len(), self.net_remote.len(), self.dyn_published.len(), self.bodies.len(), self.graphics.len());
        let packages = self.manager.packages.values().map(|p| format!("{} — {}{}",
            p.manifest.name, if p.running() { "running" } else if p.enabled { "enabled / stopped" } else { "disabled" },
            p.error.as_ref().map(|e| format!("\n{e}")).unwrap_or_default())).collect::<Vec<_>>().join("\n");
        let reports = self.multiplayer_debug.iter().map(|((owner,key),text)| {
            let name = self.manager.packages.get(owner).map_or(owner.as_str(), |p| p.manifest.name.as_str());
            format!("{name} / {key}\n{text}")
        }).collect::<Vec<_>>().join("\n\n");
        [sync, format!("MOD PACKAGES\n{}", if packages.is_empty() { "No packages installed" } else { &packages }),
            format!("MOD DIAGNOSTICS\n{}", if reports.is_empty() { "No mod multiplayer diagnostics reported" } else { &reports })]
    }
}

/// Read-only controller identity for `sdk.input.controller(s)`.
pub(crate) fn controllers_snapshot(input: &crate::input::ControllerInput) -> Value {
    json!({
        "active": input.active_slot(),
        "slots": input.kinds.iter()
            .map(|kind| kind.as_deref().map_or(Value::Null, |kind| {
                let mut value = serde_json::to_value(kind).unwrap_or(Value::Null);
                if let Some(object) = value.as_object_mut() {
                    object.insert("summary".into(), json!(kind.summary()));
                    object.insert("face_labels".into(), json!(kind.prompt_style.face_labels()));
                }
                value
            }))
            .collect::<Vec<_>>(),
    })
}

pub(crate) fn override_actions(mods: Option<&Mods>, values: &mut [f32;18]) {
    if let Some(mods)=mods { for (&id,(_,value)) in &mods.input_overrides {values[id-64]=*value;} }
}

/// Retain dispatch ordering while attributing each Lua callback in CPU captures.
fn dispatch_profiled(manager: &mut skate_mods::Manager, callback: &str, payload: Value) {
    let ids: Vec<_> = manager.packages.iter().filter(|(_, p)| p.running())
        .map(|(id, _)| id.clone()).collect();
    for id in ids {
        let _span = bevy::log::tracing::info_span!("mods.callback", mod_id = %id, callback).entered();
        manager.call(&id, callback, payload.clone());
    }
}

#[cfg(test)]
mod ground_tests {
    use super::*;

    fn mods() -> Mods {
        let dir = std::env::temp_dir().join(format!("skate-mods-ground-{}", std::process::id()));
        Mods::new(Manager::new(dir.clone(), dir))
    }

    #[test]
    fn the_dynamics_ground_waits_for_something_that_can_use_it() {
        let mut mods = mods();
        assert!(!mods.needs_ground());
        // This world has no physics: building the ground would panic on the missing resource.
        ensure_ground(&World::new(), &mut mods).unwrap();
        assert!(!mods.ground_ready && !mods.world.has_ground());
        mods.bodies.insert(("crates".into(), "box".into()), 7);
        assert!(mods.needs_ground());
        mods.bodies.clear();
        mods.joints.insert(("crates".into(), "hinge".into()), 3);
        assert!(mods.needs_ground());
    }

    #[test]
    fn a_map_change_releases_the_dynamics_ground() {
        let mut mods = mods();
        mods.world.set_ground([[[-1., 0., -1.], [1., 0., -1.], [1., 0., 1.]]].into_iter()).unwrap();
        mods.ground_ready = true;
        let mut world = World::new();
        world.insert_resource(mods);
        release_ground(&mut world);
        let mods = world.resource::<Mods>();
        assert!(!mods.ground_ready && !mods.world.has_ground());
        // The bare-world map-change tests have no mod runtime: nothing to release.
        release_ground(&mut World::new());
    }
}
