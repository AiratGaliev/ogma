//! Standalone M0 proof for frame snapshots and batch consumer host APIs.
//!
//! This crate is a benchmark fixture, not an Ogma production Runtime. It runs the
//! pinned CGE-style JavaScript consumer through rquickjs 0.14.0 and models the
//! host snapshot / command / ECS boundaries independently.

use rquickjs::{Context, Error as JsError, Function, Object, Persistent, Runtime, TypedArray};
use std::collections::{HashMap, HashSet};
use std::mem::{align_of, size_of};
use std::slice;
use std::sync::{Arc, Mutex, MutexGuard};

pub const CGE_REFERENCE_REVISION: &str = "c47ad08c3da9a3857f8429cebbed6ce88969715d";
const KEY_A: u32 = 0;
const KEY_D: u32 = 1;
const KEY_ARROW_LEFT: u32 = 2;
const KEY_SPACE: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollisionPair {
    pub entity_a: u32,
    pub entity_b: u32,
}

#[derive(Clone, Debug)]
pub struct InputSnapshot {
    pub keys_down: Arc<HashSet<String>>,
    pub key_down_bits: u64,
    pub key_pressed_bits: u64,
    pub gamepad_down_bits: u32,
    pub gamepad_pressed_bits: u32,
}

impl InputSnapshot {
    pub fn is_key_down(&self, key: &str) -> bool {
        self.keys_down.contains(key)
    }

    pub fn is_key_down_id(&self, key_id: u32) -> bool {
        key_id < u64::BITS && (self.key_down_bits & (1_u64 << key_id)) != 0
    }

    pub fn was_key_pressed(&self, key: &str) -> bool {
        key_name_id(key)
            .map(|id| self.was_key_pressed_id(id))
            .unwrap_or(false)
    }

    pub fn was_key_pressed_id(&self, key_id: u32) -> bool {
        key_id < u64::BITS && (self.key_pressed_bits & (1_u64 << key_id)) != 0
    }

    pub fn is_gamepad_down(&self, button: &str) -> bool {
        gamepad_button_id(button)
            .map(|id| self.is_gamepad_down_id(id))
            .unwrap_or(false)
    }

    pub fn is_gamepad_down_id(&self, button_id: u32) -> bool {
        button_id < u32::BITS && (self.gamepad_down_bits & (1_u32 << button_id)) != 0
    }

    pub fn was_gamepad_pressed(&self, button: &str) -> bool {
        gamepad_button_id(button)
            .map(|id| self.was_gamepad_pressed_id(id))
            .unwrap_or(false)
    }

    pub fn was_gamepad_pressed_id(&self, button_id: u32) -> bool {
        button_id < u32::BITS && (self.gamepad_pressed_bits & (1_u32 << button_id)) != 0
    }

    pub fn fixture() -> Self {
        Self {
            keys_down: Arc::new(HashSet::from([
                String::from("A"),
                String::from("ArrowLeft"),
                String::from("Space"),
            ])),
            key_down_bits: (1_u64 << KEY_A) | (1_u64 << KEY_ARROW_LEFT) | (1_u64 << KEY_SPACE),
            key_pressed_bits: 1_u64 << KEY_SPACE,
            gamepad_down_bits: 0b011,
            gamepad_pressed_bits: 0b001,
        }
    }
}

fn gamepad_button_id(button: &str) -> Option<u32> {
    match button {
        "South" => Some(0),
        "DPadLeft" => Some(1),
        "DPadRight" => Some(2),
        _ => None,
    }
}

fn key_name_id(key: &str) -> Option<u32> {
    match key {
        "A" => Some(KEY_A),
        "D" => Some(KEY_D),
        "ArrowLeft" => Some(KEY_ARROW_LEFT),
        "Space" => Some(KEY_SPACE),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct HostFrame {
    pub frame_id: u64,
    pub delta_seconds: f32,
    pub input: InputSnapshot,
    pub positions: HashMap<u32, [f32; 2]>,
    pub collisions: Vec<CollisionPair>,
}

impl HostFrame {
    pub fn fixture(frame_id: u64, entity_count: usize, collision_count: usize) -> Self {
        let positions = (1..=entity_count as u32)
            .map(|entity_id| {
                (
                    entity_id,
                    [entity_id as f32 * 0.5, entity_id as f32 * -0.25],
                )
            })
            .collect();
        let collisions = (0..collision_count)
            .map(|index| CollisionPair {
                entity_a: index as u32 + 1,
                entity_b: entity_count as u32 - index as u32,
            })
            .collect();

        Self {
            frame_id,
            delta_seconds: 1.0 / 60.0,
            input: InputSnapshot::fixture(),
            positions,
            collisions,
        }
    }
}

/// An immutable-per-update view of consumer input, transforms and collision pairs.
/// The host publishes it before entering script execution and does not mutate it
/// until that synchronous update returns.
#[derive(Clone, Debug)]
pub struct FrameSnapshot {
    frame_id: u64,
    delta_seconds: f32,
    input: InputSnapshot,
    positions: HashMap<u32, [f32; 2]>,
    collisions: Vec<CollisionPair>,
}

impl FrameSnapshot {
    pub fn from_host_frame(frame: &HostFrame) -> Self {
        Self {
            frame_id: frame.frame_id,
            delta_seconds: frame.delta_seconds,
            input: frame.input.clone(),
            positions: frame.positions.clone(),
            collisions: frame.collisions.clone(),
        }
    }

    /// Copies a new frame into reusable host-owned storage. HashMap/Vec capacity is
    /// retained; the small immutable key-name set is shared for the frame lifetime.
    pub fn refresh_from(&mut self, frame: &HostFrame) {
        self.frame_id = frame.frame_id;
        self.delta_seconds = frame.delta_seconds;
        self.input = frame.input.clone();
        self.positions.clear();
        self.positions.reserve(frame.positions.len());
        self.positions.extend(
            frame
                .positions
                .iter()
                .map(|(id, position)| (*id, *position)),
        );
        self.collisions.clear();
        self.collisions.extend_from_slice(&frame.collisions);
    }

    pub fn frame_id(&self) -> u64 {
        self.frame_id
    }

    pub fn delta_seconds(&self) -> f32 {
        self.delta_seconds
    }

    pub fn input(&self) -> &InputSnapshot {
        &self.input
    }

    pub fn positions(&self) -> &HashMap<u32, [f32; 2]> {
        &self.positions
    }

    pub fn collisions(&self) -> &[CollisionPair] {
        &self.collisions
    }

    pub fn position(&self, entity_id: u32) -> [f32; 2] {
        self.positions
            .get(&entity_id)
            .copied()
            .unwrap_or([0.0, 0.0])
    }

    pub fn collision(&self, index: u32) -> Option<CollisionPair> {
        self.collisions.get(index as usize).copied()
    }

    pub fn read_positions_into(
        &self,
        entity_ids: &[u32],
        output: &mut [f32],
    ) -> Result<(), &'static str> {
        let required = entity_ids
            .len()
            .checked_mul(2)
            .ok_or("position output length overflow")?;
        if output.len() != required {
            return Err("position output length mismatch");
        }
        for (index, entity_id) in entity_ids.iter().copied().enumerate() {
            let position = self.position(entity_id);
            output[index * 2] = position[0];
            output[index * 2 + 1] = position[1];
        }
        Ok(())
    }

    pub fn read_collisions_into(&self, output: &mut [u32]) -> Result<(), &'static str> {
        let required = self
            .collisions
            .len()
            .checked_mul(2)
            .ok_or("collision output length overflow")?;
        if output.len() != required {
            return Err("collision output length mismatch");
        }
        for (index, pair) in self.collisions.iter().enumerate() {
            output[index * 2] = pair.entity_a;
            output[index * 2 + 1] = pair.entity_b;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionChange {
    pub entity_id: u32,
    pub position: [f32; 2],
}

/// Consumer-side model of the CGE active-pose/deactivation cache. It deliberately
/// does not depend on Jolt and is never reported as a Jolt physics measurement.
pub struct PhysicsPoseCache {
    positions: HashMap<u32, [f32; 2]>,
    changes: Vec<PositionChange>,
    change_indices: HashMap<u32, usize>,
}

impl PhysicsPoseCache {
    pub fn new(entity_count: usize) -> Self {
        let positions = (1..=entity_count as u32)
            .map(|id| (id, [id as f32 * 0.5, id as f32 * -0.25]))
            .collect();
        Self {
            positions,
            changes: Vec::with_capacity(entity_count),
            change_indices: HashMap::with_capacity(entity_count),
        }
    }

    pub fn apply_active_poses(
        &mut self,
        active_poses: &[(u32, [f32; 2])],
        deactivated_poses: &[(u32, [f32; 2])],
    ) {
        self.changes.clear();
        self.change_indices.clear();
        for (entity_id, position) in active_poses.iter().chain(deactivated_poses) {
            self.record_position_change(*entity_id, *position);
        }
    }

    fn record_position_change(&mut self, entity_id: u32, position: [f32; 2]) {
        if self.positions.get(&entity_id).copied() == Some(position) {
            return;
        }
        self.positions.insert(entity_id, position);
        if let Some(index) = self.change_indices.get(&entity_id).copied() {
            self.changes[index].position = position;
        } else {
            let index = self.changes.len();
            self.changes.push(PositionChange {
                entity_id,
                position,
            });
            self.change_indices.insert(entity_id, index);
        }
    }

    pub fn positions(&self) -> &HashMap<u32, [f32; 2]> {
        &self.positions
    }

    pub fn changes(&self) -> &[PositionChange] {
        &self.changes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EcsSyncMode {
    Sparse,
    Dense,
}

/// Small host-side ECS synchronization model. It mirrors the consumer's sparse
/// changed-entity path and dense cached-transform query without importing Flecs.
pub struct HostEcsSync {
    transforms: Vec<[f32; 2]>,
}

impl HostEcsSync {
    pub fn new(entity_count: usize) -> Self {
        Self {
            transforms: (1..=entity_count as u32)
                .map(|id| [id as f32 * 0.5, id as f32 * -0.25])
                .collect(),
        }
    }

    pub fn sync_sparse(&mut self, changes: &[PositionChange]) {
        for change in changes {
            if let Some(transform) = change
                .entity_id
                .checked_sub(1)
                .and_then(|index| self.transforms.get_mut(index as usize))
            {
                *transform = change.position;
            }
        }
    }

    pub fn sync_dense(&mut self, positions: &HashMap<u32, [f32; 2]>) {
        for (index, transform) in self.transforms.iter_mut().enumerate() {
            if let Some(position) = positions.get(&(index as u32 + 1)) {
                *transform = *position;
            }
        }
    }

    /// Mirrors the CGE reference adapter's current 50% sparse/dense selector.
    /// This is a consumer-specific model, not an Ogma Runtime policy.
    pub fn sync_adaptive(
        &mut self,
        changes: &[PositionChange],
        positions: &HashMap<u32, [f32; 2]>,
    ) -> Option<EcsSyncMode> {
        if changes.is_empty() || positions.is_empty() {
            return None;
        }
        if changes.len().saturating_mul(2) >= positions.len() {
            self.sync_dense(positions);
            Some(EcsSyncMode::Dense)
        } else {
            self.sync_sparse(changes);
            Some(EcsSyncMode::Sparse)
        }
    }

    pub fn transforms(&self) -> &[[f32; 2]] {
        &self.transforms
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostCommand {
    pub entity_id: u32,
    pub opcode: u32,
}

/// Reusable host command storage; draining clears the caller buffer and swaps
/// ownership instead of allocating or cloning commands.
pub struct CommandBuffer {
    pending: Vec<HostCommand>,
}

impl CommandBuffer {
    pub fn with_commands(command_count: usize, seed: u32) -> Self {
        let pending = (0..command_count)
            .map(|index| HostCommand {
                entity_id: index as u32 + 1,
                opcode: seed.wrapping_add(index as u32),
            })
            .collect();
        Self { pending }
    }

    pub fn drain_into(&mut self, output: &mut Vec<HostCommand>) {
        output.clear();
        std::mem::swap(output, &mut self.pending);
    }
}

#[derive(Clone, Copy, Debug)]
pub enum UpdateMode {
    Scalar,
    BatchSet,
    BatchBulk,
    PureJavaScript,
}

/// Runs the pinned reference consumer script on rquickjs 0.14.0. All registered
/// callbacks read the same immutable FrameSnapshot for one synchronous update.
pub struct ScriptHarness {
    // Persistent JS values must be dropped before Context and Runtime.
    scalar: Persistent<Function<'static>>,
    batch_set: Persistent<Function<'static>>,
    batch_bulk: Persistent<Function<'static>>,
    pure_javascript: Persistent<Function<'static>>,
    context: Context,
    _runtime: Runtime,
    state: Arc<Mutex<FrameSnapshot>>,
}

impl ScriptHarness {
    pub fn new(
        frame: FrameSnapshot,
        entity_count: usize,
        collision_count: usize,
    ) -> Result<Self, String> {
        let runtime = Runtime::new().map_err(|error| error.to_string())?;
        let context = Context::full(&runtime).map_err(|error| error.to_string())?;
        let state = Arc::new(Mutex::new(frame));

        let hooks = context
            .with(|ctx| -> rquickjs::Result<_> {
                let engine = Object::new(ctx.clone())?;

                let input = Object::new(ctx.clone())?;
                {
                    let state = Arc::clone(&state);
                    input.set(
                        "isDown",
                        Function::new(ctx.clone(), move |key: String| {
                            lock(&state).input().is_key_down(&key)
                        })?,
                    )?;
                }
                {
                    let state = Arc::clone(&state);
                    input.set(
                        "isDownId",
                        Function::new(ctx.clone(), move |key_id: u32| {
                            lock(&state).input().is_key_down_id(key_id)
                        })?,
                    )?;
                }
                {
                    let state = Arc::clone(&state);
                    input.set(
                        "wasPressed",
                        Function::new(ctx.clone(), move |key: String| {
                            lock(&state).input().was_key_pressed(&key)
                        })?,
                    )?;
                }
                {
                    let state = Arc::clone(&state);
                    input.set(
                        "wasPressedId",
                        Function::new(ctx.clone(), move |key_id: u32| {
                            lock(&state).input().was_key_pressed_id(key_id)
                        })?,
                    )?;
                }

                let gamepad = Object::new(ctx.clone())?;
                {
                    let state = Arc::clone(&state);
                    gamepad.set(
                        "isDown",
                        Function::new(ctx.clone(), move |button: String| {
                            lock(&state).input().is_gamepad_down(&button)
                        })?,
                    )?;
                }
                {
                    let state = Arc::clone(&state);
                    gamepad.set(
                        "isDownId",
                        Function::new(ctx.clone(), move |button_id: u32| {
                            lock(&state).input().is_gamepad_down_id(button_id)
                        })?,
                    )?;
                }
                {
                    let state = Arc::clone(&state);
                    gamepad.set(
                        "wasPressed",
                        Function::new(ctx.clone(), move |button: String| {
                            lock(&state).input().was_gamepad_pressed(&button)
                        })?,
                    )?;
                }
                {
                    let state = Arc::clone(&state);
                    gamepad.set(
                        "wasPressedId",
                        Function::new(ctx.clone(), move |button_id: u32| {
                            lock(&state).input().was_gamepad_pressed_id(button_id)
                        })?,
                    )?;
                }
                input.set("gamepad", gamepad)?;
                engine.set("input", input)?;

                let entities = Object::new(ctx.clone())?;
                {
                    let state = Arc::clone(&state);
                    entities.set(
                        "getPositionX",
                        Function::new(ctx.clone(), move |entity_id: u32| {
                            lock(&state).position(entity_id)[0]
                        })?,
                    )?;
                }
                {
                    let state = Arc::clone(&state);
                    entities.set(
                        "getPositionY",
                        Function::new(ctx.clone(), move |entity_id: u32| {
                            lock(&state).position(entity_id)[1]
                        })?,
                    )?;
                }
                {
                    let state = Arc::clone(&state);
                    entities.set(
                        "readPositionsSet",
                        Function::new(
                            ctx.clone(),
                            move |ids: TypedArray<'_, u32>,
                                  output: TypedArray<'_, f32>|
                                  -> rquickjs::Result<()> {
                                let id_count = ids.len();
                                let required = id_count.checked_mul(2).ok_or_else(|| {
                                    JsError::new_from_js("Uint32Array", "position length overflow")
                                })?;
                                if output.len() != required {
                                    return Err(JsError::new_from_js(
                                        "Float32Array",
                                        "position output length mismatch",
                                    ));
                                }
                                validate_distinct_arrays(&ids, &output)?;
                                let snapshot = lock(&state);
                                for index in 0..id_count {
                                    let entity_id = ids.get::<_, u32>(index as u32)?;
                                    let position = snapshot.position(entity_id);
                                    output.set((index * 2) as u32, position[0])?;
                                    output.set((index * 2 + 1) as u32, position[1])?;
                                }
                                Ok(())
                            },
                        )?,
                    )?;
                }
                {
                    let state = Arc::clone(&state);
                    entities.set(
                        "readPositionsBulk",
                        Function::new(
                            ctx.clone(),
                            move |ids: TypedArray<'_, u32>,
                                  output: TypedArray<'_, f32>|
                                  -> rquickjs::Result<()> {
                                let id_count = ids.len();
                                let output_len = output.len();
                                let required = id_count.checked_mul(2).ok_or_else(|| {
                                    JsError::new_from_js("Uint32Array", "position length overflow")
                                })?;
                                if output_len != required {
                                    return Err(JsError::new_from_js(
                                        "Float32Array",
                                        "position output length mismatch",
                                    ));
                                }
                                if output_len == 0 {
                                    return Ok(());
                                }
                                validate_distinct_arrays(&ids, &output)?;
                                let raw = output.as_raw().ok_or_else(|| {
                                    JsError::new_from_js(
                                        "Float32Array",
                                        "position output buffer is detached",
                                    )
                                })?;
                                let output_ptr = raw.as_ptr() as *mut f32;
                                if !(output_ptr as usize).is_multiple_of(align_of::<f32>()) {
                                    return Err(JsError::new_from_js(
                                        "Float32Array",
                                        "position output is misaligned",
                                    ));
                                }

                                // SAFETY: both arguments were converted to the exact typed
                                // array classes, their lengths were checked, and their byte
                                // ranges were proven disjoint. This synchronous callback makes
                                // no JavaScript calls, yields, or re-enters the runtime while
                                // either backing-store slice is live.
                                let ids_slice = unsafe { ids.as_slice() };
                                // SAFETY: output is an attached Float32Array and output_len
                                // equals its element length. No JS executes until the slice drops.
                                let output_slice =
                                    unsafe { slice::from_raw_parts_mut(output_ptr, output_len) };
                                let snapshot = lock(&state);
                                for (index, entity_id) in ids_slice.iter().copied().enumerate() {
                                    let position = snapshot.position(entity_id);
                                    output_slice[index * 2] = position[0];
                                    output_slice[index * 2 + 1] = position[1];
                                }
                                Ok(())
                            },
                        )?,
                    )?;
                }
                engine.set("entities", entities)?;

                let physics = Object::new(ctx.clone())?;
                {
                    let state = Arc::clone(&state);
                    physics.set(
                        "collisionCount",
                        Function::new(ctx.clone(), move || lock(&state).collisions().len() as u32)?,
                    )?;
                }
                {
                    let state = Arc::clone(&state);
                    physics.set(
                        "collisionEntityA",
                        Function::new(ctx.clone(), move |index: u32| {
                            lock(&state).collision(index).map(|pair| pair.entity_a)
                        })?,
                    )?;
                }
                {
                    let state = Arc::clone(&state);
                    physics.set(
                        "collisionEntityB",
                        Function::new(ctx.clone(), move |index: u32| {
                            lock(&state).collision(index).map(|pair| pair.entity_b)
                        })?,
                    )?;
                }
                {
                    let state = Arc::clone(&state);
                    physics.set(
                        "readCollisionsSet",
                        Function::new(
                            ctx.clone(),
                            move |output: TypedArray<'_, u32>| -> rquickjs::Result<()> {
                                let snapshot = lock(&state);
                                let required =
                                    snapshot.collisions().len().checked_mul(2).ok_or_else(
                                        || {
                                            JsError::new_from_js(
                                                "Uint32Array",
                                                "collision length overflow",
                                            )
                                        },
                                    )?;
                                if output.len() != required {
                                    return Err(JsError::new_from_js(
                                        "Uint32Array",
                                        "collision output length mismatch",
                                    ));
                                }
                                for (index, pair) in snapshot.collisions().iter().enumerate() {
                                    output.set((index * 2) as u32, pair.entity_a)?;
                                    output.set((index * 2 + 1) as u32, pair.entity_b)?;
                                }
                                Ok(())
                            },
                        )?,
                    )?;
                }
                {
                    let state = Arc::clone(&state);
                    physics.set(
                        "readCollisionsBulk",
                        Function::new(
                            ctx.clone(),
                            move |output: TypedArray<'_, u32>| -> rquickjs::Result<()> {
                                let snapshot = lock(&state);
                                let output_len = output.len();
                                let required =
                                    snapshot.collisions().len().checked_mul(2).ok_or_else(
                                        || {
                                            JsError::new_from_js(
                                                "Uint32Array",
                                                "collision length overflow",
                                            )
                                        },
                                    )?;
                                if output_len != required {
                                    return Err(JsError::new_from_js(
                                        "Uint32Array",
                                        "collision output length mismatch",
                                    ));
                                }
                                if output_len == 0 {
                                    return Ok(());
                                }
                                let raw = output.as_raw().ok_or_else(|| {
                                    JsError::new_from_js(
                                        "Uint32Array",
                                        "collision output buffer is detached",
                                    )
                                })?;
                                let output_ptr = raw.as_ptr() as *mut u32;
                                if !(output_ptr as usize).is_multiple_of(align_of::<u32>()) {
                                    return Err(JsError::new_from_js(
                                        "Uint32Array",
                                        "collision output is misaligned",
                                    ));
                                }

                                // SAFETY: output is an attached Uint32Array with an exact
                                // checked length. This callback performs no JavaScript calls
                                // or re-entry while writing its backing store.
                                let output_slice =
                                    unsafe { slice::from_raw_parts_mut(output_ptr, output_len) };
                                for (index, pair) in snapshot.collisions().iter().enumerate() {
                                    output_slice[index * 2] = pair.entity_a;
                                    output_slice[index * 2 + 1] = pair.entity_b;
                                }
                                Ok(())
                            },
                        )?,
                    )?;
                }
                engine.set("physics", physics)?;

                let time = Object::new(ctx.clone())?;
                {
                    let state = Arc::clone(&state);
                    time.set(
                        "deltaSeconds",
                        Function::new(ctx.clone(), move || lock(&state).delta_seconds())?,
                    )?;
                }
                engine.set("time", time)?;

                let snapshot_api = Object::new(ctx.clone())?;
                {
                    let state = Arc::clone(&state);
                    snapshot_api.set(
                        "frameId",
                        Function::new(ctx.clone(), move || lock(&state).frame_id())?,
                    )?;
                }
                engine.set("snapshot", snapshot_api)?;
                ctx.globals().set("engine", engine)?;

                let entity_ids = TypedArray::<u32>::new(
                    ctx.clone(),
                    (1..=entity_count as u32).collect::<Vec<_>>(),
                )?;
                ctx.globals().set("entityIds", entity_ids)?;
                let positions_output =
                    TypedArray::<f32>::new(ctx.clone(), vec![0.0; entity_count * 2])?;
                ctx.globals().set("positionOutput", positions_output)?;
                let collisions_output =
                    TypedArray::<u32>::new(ctx.clone(), vec![0; collision_count * 2])?;
                ctx.globals().set("collisionOutput", collisions_output)?;

                let frame = lock(&state);
                let key_down = (0..4)
                    .map(|id| u8::from(frame.input().is_key_down_id(id)))
                    .collect::<Vec<_>>();
                let gamepad_down = (0..3)
                    .map(|id| u8::from(frame.input().is_gamepad_down_id(id)))
                    .collect::<Vec<_>>();
                let pure_keys = TypedArray::<u8>::new(ctx.clone(), key_down)?;
                ctx.globals().set("pureKeyDown", pure_keys)?;
                let key_pressed = (0..4)
                    .map(|id| u8::from(frame.input().was_key_pressed_id(id)))
                    .collect::<Vec<_>>();
                let pure_key_pressed = TypedArray::<u8>::new(ctx.clone(), key_pressed)?;
                ctx.globals().set("pureKeyPressed", pure_key_pressed)?;
                let pure_gamepad = TypedArray::<u8>::new(ctx.clone(), gamepad_down)?;
                ctx.globals().set("pureGamepadDown", pure_gamepad)?;
                let gamepad_pressed = (0..3)
                    .map(|id| u8::from(frame.input().was_gamepad_pressed_id(id)))
                    .collect::<Vec<_>>();
                let pure_gamepad_pressed = TypedArray::<u8>::new(ctx.clone(), gamepad_pressed)?;
                ctx.globals()
                    .set("pureGamepadPressed", pure_gamepad_pressed)?;
                ctx.globals()
                    .set("deltaSecondsValue", frame.delta_seconds())?;
                drop(frame);

                ctx.eval::<(), _>(include_str!("../fixtures/consumer-update.js"))?;
                let globals = ctx.globals();
                let scalar = globals.get::<_, Function>("runScalar")?;
                let batch_set = globals.get::<_, Function>("runBatchSet")?;
                let batch_bulk = globals.get::<_, Function>("runBatchBulk")?;
                let pure_javascript = globals.get::<_, Function>("runPureJavaScript")?;

                Ok((
                    Persistent::save(&ctx, scalar),
                    Persistent::save(&ctx, batch_set),
                    Persistent::save(&ctx, batch_bulk),
                    Persistent::save(&ctx, pure_javascript),
                ))
            })
            .map_err(|error| error.to_string())?;

        let harness = Self {
            scalar: hooks.0,
            batch_set: hooks.1,
            batch_bulk: hooks.2,
            pure_javascript: hooks.3,
            context,
            _runtime: runtime,
            state,
        };
        // Seed the reusable output arrays so the JS-only update baseline has the
        // same input data but performs no host callback during its timed loop.
        harness.run(UpdateMode::BatchBulk)?;
        Ok(harness)
    }

    pub fn publish_snapshot(&self, frame: &FrameSnapshot) {
        lock(&self.state).clone_from(frame);
    }

    pub fn run(&self, mode: UpdateMode) -> Result<f64, String> {
        let function = match mode {
            UpdateMode::Scalar => self.scalar.clone(),
            UpdateMode::BatchSet => self.batch_set.clone(),
            UpdateMode::BatchBulk => self.batch_bulk.clone(),
            UpdateMode::PureJavaScript => self.pure_javascript.clone(),
        };
        self.context
            .with(|ctx| function.restore(&ctx)?.call::<_, f64>(()))
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    fn eval(&self, source: &str) -> rquickjs::Result<()> {
        self.context.with(|ctx| ctx.eval::<(), _>(source))
    }
}

fn validate_distinct_arrays(
    ids: &TypedArray<'_, u32>,
    output: &TypedArray<'_, f32>,
) -> rquickjs::Result<()> {
    let ids_bytes = ids
        .len()
        .checked_mul(size_of::<u32>())
        .ok_or_else(|| JsError::new_from_js("Uint32Array", "input byte length overflow"))?;
    let output_bytes = output
        .len()
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| JsError::new_from_js("Float32Array", "output byte length overflow"))?;
    if ids_bytes == 0 || output_bytes == 0 {
        return Ok(());
    }
    let ids_raw = ids
        .as_raw()
        .ok_or_else(|| JsError::new_from_js("Uint32Array", "input buffer is detached"))?;
    let output_raw = output
        .as_raw()
        .ok_or_else(|| JsError::new_from_js("Float32Array", "output buffer is detached"))?;
    let ids_start = ids_raw.as_ptr() as *mut u8 as usize;
    let output_start = output_raw.as_ptr() as *mut u8 as usize;
    let ids_end = ids_start
        .checked_add(ids_bytes)
        .ok_or_else(|| JsError::new_from_js("Uint32Array", "input address overflow"))?;
    let output_end = output_start
        .checked_add(output_bytes)
        .ok_or_else(|| JsError::new_from_js("Float32Array", "output address overflow"))?;
    if ids_start < output_end && output_start < ids_end {
        return Err(JsError::new_from_js(
            "Float32Array",
            "input IDs and output positions must not alias",
        ));
    }
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(entity_count: usize, collision_count: usize) -> FrameSnapshot {
        FrameSnapshot::from_host_frame(&HostFrame::fixture(7, entity_count, collision_count))
    }

    #[test]
    fn scalar_and_batch_snapshot_reads_are_equivalent() {
        let snapshot = fixture(40, 5);
        let ids = (1..=40).collect::<Vec<u32>>();
        let mut positions = vec![0.0; ids.len() * 2];
        snapshot.read_positions_into(&ids, &mut positions).unwrap();
        for (index, id) in ids.iter().copied().enumerate() {
            let scalar = snapshot.position(id);
            assert_eq!(positions[index * 2], scalar[0]);
            assert_eq!(positions[index * 2 + 1], scalar[1]);
        }

        let mut collisions = vec![0; snapshot.collisions().len() * 2];
        snapshot.read_collisions_into(&mut collisions).unwrap();
        for (index, pair) in snapshot.collisions().iter().enumerate() {
            assert_eq!(collisions[index * 2], pair.entity_a);
            assert_eq!(collisions[index * 2 + 1], pair.entity_b);
        }
        for (name, id) in [
            ("A", KEY_A),
            ("D", KEY_D),
            ("ArrowLeft", KEY_ARROW_LEFT),
            ("Space", KEY_SPACE),
        ] {
            assert_eq!(
                snapshot.input().is_key_down(name),
                snapshot.input().is_key_down_id(id)
            );
            assert_eq!(
                snapshot.input().was_key_pressed(name),
                snapshot.input().was_key_pressed_id(id)
            );
        }
        for (name, id) in [("South", 0), ("DPadLeft", 1), ("DPadRight", 2)] {
            assert_eq!(
                snapshot.input().is_gamepad_down(name),
                snapshot.input().is_gamepad_down_id(id)
            );
            assert_eq!(
                snapshot.input().was_gamepad_pressed(name),
                snapshot.input().was_gamepad_pressed_id(id)
            );
        }
    }

    #[test]
    fn snapshot_keeps_one_frame_consistent_after_host_state_changes() {
        let mut host = HostFrame::fixture(10, 4, 2);
        let snapshot = FrameSnapshot::from_host_frame(&host);
        host.frame_id = 11;
        host.delta_seconds = 0.5;
        host.positions.insert(1, [99.0, 101.0]);
        host.collisions.clear();
        host.input.keys_down = Arc::new(HashSet::from([String::from("D")]));
        assert_eq!(snapshot.frame_id(), 10);
        assert_eq!(snapshot.delta_seconds(), 1.0 / 60.0);
        assert_eq!(snapshot.position(1), [0.5, -0.25]);
        assert_eq!(snapshot.collisions().len(), 2);
        assert!(snapshot.input().is_key_down("A"));
        assert!(!snapshot.input().is_key_down("D"));

        let mut reusable = snapshot.clone();
        reusable.refresh_from(&host);
        assert_eq!(reusable.frame_id(), 11);
        assert_eq!(reusable.position(1), [99.0, 101.0]);
        assert!(reusable.collisions().is_empty());
        assert!(!reusable.input().is_key_down("A"));
        assert!(reusable.input().is_key_down("D"));
    }

    #[test]
    fn reused_snapshot_buffers_keep_capacity() {
        let mut host = HostFrame::fixture(1, 1024, 128);
        let mut snapshot = FrameSnapshot::from_host_frame(&host);
        let positions_capacity = snapshot.positions.capacity();
        let collisions_capacity = snapshot.collisions.capacity();
        host.frame_id = 2;
        snapshot.refresh_from(&host);
        assert!(snapshot.positions.capacity() >= positions_capacity);
        assert!(snapshot.collisions.capacity() >= collisions_capacity);
    }

    #[test]
    fn input_numeric_ids_reject_out_of_range_values() {
        let snapshot = fixture(1, 0);
        assert!(!snapshot.input().is_key_down_id(64));
        assert!(!snapshot.input().is_gamepad_down_id(32));
        assert!(!snapshot.input().is_gamepad_down("Unknown"));
    }

    #[test]
    fn physics_cache_coalesces_updates_and_keeps_final_deactivation_pose() {
        let mut cache = PhysicsPoseCache::new(3);
        cache.apply_active_poses(&[(1, [1.0, 2.0]), (2, [3.0, 4.0])], &[(1, [5.0, 6.0])]);
        assert_eq!(cache.changes().len(), 2);
        assert_eq!(cache.positions()[&1], [5.0, 6.0]);
        assert_eq!(
            cache
                .changes()
                .iter()
                .find(|change| change.entity_id == 1)
                .unwrap()
                .position,
            [5.0, 6.0]
        );
    }

    #[test]
    fn sparse_and_dense_ecs_sync_models_produce_equivalent_transforms() {
        let mut positions = HashMap::from([
            (1, [10.0, 11.0]),
            (2, [1.0, -0.5]),
            (3, [30.0, 31.0]),
            (4, [2.0, -1.0]),
        ]);
        let changes = vec![
            PositionChange {
                entity_id: 1,
                position: positions[&1],
            },
            PositionChange {
                entity_id: 3,
                position: positions[&3],
            },
        ];
        let mut sparse = HostEcsSync::new(4);
        let mut dense = HostEcsSync::new(4);
        sparse.sync_sparse(&changes);
        dense.sync_dense(&positions);
        assert_eq!(sparse.transforms(), dense.transforms());
        assert_eq!(
            sparse.sync_adaptive(&changes, &positions),
            Some(EcsSyncMode::Dense)
        );

        positions.insert(4, [88.0, 99.0]);
        let mut sparse_only = HostEcsSync::new(4);
        sparse_only.sync_sparse(&changes);
        assert_eq!(sparse_only.transforms()[3], [2.0, -1.0]);
    }

    #[test]
    fn command_drain_swaps_reusable_storage_and_preserves_order() {
        let mut pending = CommandBuffer::with_commands(3, 10);
        let mut output = Vec::with_capacity(8);
        output.push(HostCommand {
            entity_id: 99,
            opcode: 99,
        });
        pending.drain_into(&mut output);
        assert_eq!(
            output,
            vec![
                HostCommand {
                    entity_id: 1,
                    opcode: 10
                },
                HostCommand {
                    entity_id: 2,
                    opcode: 11
                },
                HostCommand {
                    entity_id: 3,
                    opcode: 12
                },
            ]
        );
        assert!(pending.pending.is_empty());
        assert!(pending.pending.capacity() >= 8);
    }

    #[test]
    fn reference_javascript_scalar_batch_set_and_bulk_results_match() {
        let snapshot = fixture(40, 5);
        let harness = ScriptHarness::new(snapshot, 40, 5).unwrap();
        let scalar = harness.run(UpdateMode::Scalar).unwrap();
        let set = harness.run(UpdateMode::BatchSet).unwrap();
        let bulk = harness.run(UpdateMode::BatchBulk).unwrap();
        let pure = harness.run(UpdateMode::PureJavaScript).unwrap();
        assert!((scalar - set).abs() < 0.000_001);
        assert!((scalar - bulk).abs() < 0.000_001);
        assert!((scalar - pure).abs() < 0.000_001);
    }

    #[test]
    fn bulk_typed_array_writes_reject_length_mismatch_and_aliasing() {
        let snapshot = fixture(4, 2);
        let harness = ScriptHarness::new(snapshot, 4, 2).unwrap();
        let length_error = harness.eval(
            "engine.entities.readPositionsBulk(new Uint32Array([1, 2]), new Float32Array(3));",
        );
        assert!(length_error.is_err());

        let alias_error = harness.eval(
            "(() => { const buffer = new ArrayBuffer(16); const ids = new Uint32Array(buffer, 0, 2); ids[0] = 1; ids[1] = 2; const output = new Float32Array(buffer, 0, 4); engine.entities.readPositionsBulk(ids, output); })();",
        );
        assert!(alias_error.is_err());
    }

    #[test]
    fn batch_collision_reads_write_expected_pair_order() {
        let snapshot = fixture(8, 3);
        let harness = ScriptHarness::new(snapshot, 8, 3).unwrap();
        harness
            .eval(
                "engine.physics.readCollisionsSet(collisionOutput); if (collisionOutput.length !== 6 || collisionOutput[0] !== 1 || collisionOutput[1] !== 8 || collisionOutput[2] !== 2 || collisionOutput[3] !== 7 || collisionOutput[4] !== 3 || collisionOutput[5] !== 6) throw new Error('TypedArray.set collision order mismatch');",
            )
            .unwrap();
        harness
            .eval(
                "engine.physics.readCollisionsBulk(collisionOutput); if (collisionOutput.length !== 6 || collisionOutput[0] !== 1 || collisionOutput[1] !== 8 || collisionOutput[2] !== 2 || collisionOutput[3] !== 7 || collisionOutput[4] !== 3 || collisionOutput[5] !== 6) throw new Error('bulk collision order mismatch');",
            )
            .unwrap();
    }
}
