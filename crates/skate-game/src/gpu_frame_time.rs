//! The GPU's time for a whole frame, beside the per-pass times of `RenderDiagnosticsPlugin`.
//! On with them (`SKATE_PERF_GPU=1`), when the device has timestamp queries inside encoders:
//! Bevy asks for every feature the adapter offers unless told otherwise (its default
//! `WgpuSettingsPriority::Functionality`), so on NVK they are there already, and
//! `SKATE_GPU_TIMING=1` asks for them by name.
//!
//! The pass timers cover only their passes. On the Switch they summed to 39.5 ms of a 43.7 ms
//! frame (sk8 findings 26 §6.1), and nothing said whether the other 4 ms was GPU work no timer
//! covers (clears, barriers, untimed nodes) or the GPU waiting for the next frame. Two
//! timestamps around the whole render graph tell those apart:
//!
//! - `render/skate_frame/busy_gpu`: from the frame's first command to its last. Less the sum of
//!   the pass times, it is the work inside the frame that no pass timer covers.
//! - `render/skate_frame/wait_gpu`: from the previous frame's last command to this frame's
//!   first. Idle time, plus what runs between two frames' graphs: the buffer and texture
//!   uploads wgpu submits ahead of a frame's commands, the query resolves after its last, and
//!   any wait for the compositor to release the swapchain image.
//!
//! wgpu writes both at the bottom of the pipe, so the first marks the moment everything before
//! the frame had finished. The performance report prints the two with the pass total as
//! `SKATE_PERF_GPU_FRAME`, and their spread over the same frames as
//! `SKATE_PERF_GPU_FRAME_SPREAD`.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use bevy::{
    diagnostic::{Diagnostic, DiagnosticMeasurement, DiagnosticPath, DiagnosticsStore},
    platform::time::Instant,
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        graph::CameraDriverLabel,
        render_graph::{Node, NodeRunError, RenderGraph, RenderGraphContext, RenderLabel},
        renderer::{RenderContext, RenderDevice, RenderQueue},
    },
};

pub(crate) const BUSY: DiagnosticPath = DiagnosticPath::const_new("render/skate_frame/busy_gpu");
pub(crate) const WAIT: DiagnosticPath = DiagnosticPath::const_new("render/skate_frame/wait_gpu");

/// Frames whose timestamps can be on their way back at once. A frame's read-back completes a
/// frame or two after its submit; a frame that finds every slot taken goes untimed.
const SLOTS: usize = 6;
const FREE: u8 = 0;
const MAPPING: u8 = 1;
const MAPPED: u8 = 2;
const FAILED: u8 = 3;

pub(crate) struct GpuFrameTimePlugin;

impl Plugin for GpuFrameTimePlugin {
    fn build(&self, app: &mut App) {
        let latest = Latest::default();
        app.insert_resource(latest.clone()).add_systems(PreUpdate, publish);
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.insert_resource(latest).init_resource::<Acquire>().add_systems(
                Render,
                (
                    acquire_started.before(bevy::render::view::prepare_windows),
                    acquire_finished.after(bevy::render::view::prepare_windows),
                )
                    .in_set(RenderSystems::ManageViews),
            );
        }
    }

    fn finish(&self, app: &mut App) {
        let Some(render) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        let needed =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        let device = render.world().resource::<RenderDevice>();
        if !device.features().contains(needed) {
            warn!("SKATE_PERF_GPU: no timestamp queries inside encoders, so no whole-frame GPU time");
            return;
        }
        let stamps = Stamps::new(device, render.world().resource::<RenderQueue>());
        render.insert_resource(stamps).add_systems(
            Render,
            (
                start.in_set(RenderSystems::Prepare),
                read_back.after(RenderSystems::Render).before(RenderSystems::Cleanup),
            ),
        );
        let mut graph = render.world_mut().resource_mut::<RenderGraph>();
        graph.add_node(FrameStartLabel, Stamp::Start);
        graph.add_node(FrameEndLabel, Stamp::End);
        // The camera driver is the root graph's only other node, and every camera's own graph
        // runs inside it, so these two bracket all the frame's commands.
        graph.add_node_edge(FrameStartLabel, CameraDriverLabel);
        graph.add_node_edge(CameraDriverLabel, FrameEndLabel);
    }
}

#[derive(Clone, Copy)]
struct FrameTime {
    busy: f64,
    wait: Option<f64>,
}

/// The newest frame read back, waiting for the main world. One that is not taken in time is
/// replaced, as `RenderDiagnosticsPlugin` replaces its own.
#[derive(Resource, Clone, Default)]
struct Latest(Arc<Mutex<Option<FrameTime>>>);

struct Slot {
    readback: wgpu::Buffer,
    state: Arc<AtomicU8>,
    frame: u64,
}

#[derive(Resource)]
struct Stamps {
    /// Two timestamps a slot: the frame's first and last.
    queries: wgpu::QuerySet,
    /// Where the queries are resolved, a slot every `QUERY_RESOLVE_BUFFER_ALIGNMENT` bytes.
    resolve: wgpu::Buffer,
    slots: Vec<Slot>,
    ms_per_tick: f64,
    /// This frame's slot; none when every slot is still on its way back.
    current: Option<usize>,
    /// Set by the end node, so a frame whose graph stopped short is not read back.
    ended: AtomicBool,
    frame: u64,
    /// The last frame read back and its final timestamp, for the next frame's wait.
    previous: Option<(u64, u64)>,
}

impl Stamps {
    fn new(device: &RenderDevice, queue: &RenderQueue) -> Self {
        let device = device.wgpu_device();
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("skate_frame_timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: 2 * SLOTS as u32,
        });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("skate_frame_timestamps_resolve"),
            size: wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT * SLOTS as u64,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let slots = (0..SLOTS)
            .map(|_| Slot {
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("skate_frame_timestamps_readback"),
                    size: 2 * wgpu::QUERY_SIZE as u64,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(FREE)),
                frame: 0,
            })
            .collect();
        Self {
            queries,
            resolve,
            slots,
            ms_per_tick: f64::from(queue.get_timestamp_period()) / 1e6,
            current: None,
            ended: AtomicBool::new(false),
            frame: 0,
            previous: None,
        }
    }
}

/// Takes the frames the GPU has finished, oldest first so each wait has the end of the frame
/// before it, then gives this frame a free slot.
fn start(mut stamps: ResMut<Stamps>, latest: Res<Latest>) {
    let stamps = &mut *stamps;
    while let Some(index) = (0..SLOTS)
        .filter(|&i| stamps.slots[i].state.load(Ordering::Acquire) != FREE)
        .min_by_key(|&i| stamps.slots[i].frame)
    {
        let slot = &stamps.slots[index];
        match slot.state.load(Ordering::Acquire) {
            MAPPED => {
                let (first, last) = {
                    let view = slot.readback.slice(..).get_mapped_range();
                    let stamp = |i: usize| {
                        u64::from_le_bytes(view[8 * i..8 * i + 8].try_into().expect("8 bytes"))
                    };
                    (stamp(0), stamp(1))
                };
                slot.readback.unmap();
                let ms = |ticks: u64| ticks as f64 * stamps.ms_per_tick;
                let wait = match stamps.previous {
                    Some((frame, end)) if frame + 1 == slot.frame => first.checked_sub(end).map(ms),
                    _ => None,
                };
                stamps.previous = Some((slot.frame, last));
                if let (Some(busy), Ok(mut latest)) = (last.checked_sub(first).map(ms), latest.0.lock())
                {
                    *latest = Some(FrameTime { busy, wait });
                }
            }
            FAILED => stamps.previous = None,
            _ => break,
        }
        slot.state.store(FREE, Ordering::Relaxed);
    }
    stamps.current = (0..SLOTS).find(|&i| stamps.slots[i].state.load(Ordering::Acquire) == FREE);
    if let Some(index) = stamps.current {
        stamps.slots[index].frame = stamps.frame;
    }
    stamps.frame += 1;
}

/// After the submit, so the copy into the slot's buffer is queued before it is mapped.
fn read_back(mut stamps: ResMut<Stamps>) {
    let Some(index) = stamps.current.take() else {
        return;
    };
    if !stamps.ended.swap(false, Ordering::Relaxed) {
        return;
    }
    let slot = &stamps.slots[index];
    let state = slot.state.clone();
    state.store(MAPPING, Ordering::Relaxed);
    slot.readback.map_async(wgpu::MapMode::Read, .., move |result| {
        state.store(if result.is_ok() { MAPPED } else { FAILED }, Ordering::Release);
    });
}

/// Ten seconds of one measurement, for a `[GPU]` line in log.txt.
#[derive(Default)]
struct Averages {
    since: Option<Instant>,
    frames: u32,
    total: f64,
    worst: f64,
}

impl Averages {
    /// Adds a frame's `ms`; after ten seconds, the frames, their average and the worst.
    fn add(&mut self, ms: f64) -> Option<(u32, f64, f64)> {
        let now = Instant::now();
        let since = *self.since.get_or_insert(now);
        self.frames += 1;
        self.total += ms;
        self.worst = self.worst.max(ms);
        if now.duration_since(since).as_secs() < 10 {
            return None;
        }
        let window = (self.frames, self.total / f64::from(self.frames), self.worst);
        *self = Self { since: Some(now), ..default() };
        Some(window)
    }
}

/// Each frame's wait for the window's next image: Bevy's `prepare_windows` acquires it, and
/// waits there while every image is still the GPU's or the screen's.
#[derive(Resource, Default)]
struct Acquire {
    started: Option<Instant>,
    waits: Averages,
}

fn acquire_started(mut acquire: ResMut<Acquire>) {
    acquire.started = Some(Instant::now());
}

fn acquire_finished(mut acquire: ResMut<Acquire>) {
    let Some(started) = acquire.started.take() else {
        return;
    };
    if let Some((frames, average, worst)) = acquire.waits.add(started.elapsed().as_secs_f64() * 1e3) {
        info!(
            "[GPU] the window's next image took {average:.2} ms on average to come over the last {frames} frames, {worst:.2} ms at worst (prepare_windows)"
        );
    }
}

fn publish(
    latest: Res<Latest>,
    mut store: ResMut<DiagnosticsStore>,
    mut busy: Local<Averages>,
    mut idle: Local<(f64, u32)>,
) {
    let Some(frame) = latest.0.lock().ok().and_then(|mut latest| latest.take()) else {
        return;
    };
    if let Some(wait) = frame.wait {
        idle.0 += wait;
        idle.1 += 1;
    }
    if let Some((frames, average, worst)) = busy.add(frame.busy) {
        let idle = std::mem::take(&mut *idle);
        info!(
            "[GPU] {frames} frames timed: busy {average:.2} ms on average ({worst:.2} at worst), idle {:.2} ms before each",
            if idle.1 == 0 { 0. } else { idle.0 / f64::from(idle.1) },
        );
    }
    let time = Instant::now();
    for (path, value) in [(&BUSY, Some(frame.busy)), (&WAIT, frame.wait)] {
        let Some(value) = value else {
            continue;
        };
        if store.get(path).is_none() {
            store.add(Diagnostic::new(path.clone()).with_suffix("ms"));
        }
        if let Some(diagnostic) = store.get_mut(path) {
            diagnostic.add_measurement(DiagnosticMeasurement { time, value });
        }
    }
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
struct FrameStartLabel;

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
struct FrameEndLabel;

enum Stamp {
    Start,
    End,
}

impl Node for Stamp {
    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        let Some(stamps) = world.get_resource::<Stamps>() else {
            return Ok(());
        };
        let Some(index) = stamps.current else {
            return Ok(());
        };
        let first = 2 * index as u32;
        let encoder = render_context.command_encoder();
        match self {
            Stamp::Start => encoder.write_timestamp(&stamps.queries, first),
            Stamp::End => {
                encoder.write_timestamp(&stamps.queries, first + 1);
                let offset = wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT * index as u64;
                encoder.resolve_query_set(&stamps.queries, first..first + 2, &stamps.resolve, offset);
                encoder.copy_buffer_to_buffer(
                    &stamps.resolve,
                    offset,
                    &stamps.slots[index].readback,
                    0,
                    2 * wgpu::QUERY_SIZE as u64,
                );
                stamps.ended.store(true, Ordering::Relaxed);
            }
        }
        Ok(())
    }
}
