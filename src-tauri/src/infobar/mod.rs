mod render;

use crate::shared::{ActionContext, ActionInstance, Context};
use anyhow::{Result, ensure};
use render::{FULL, Rect, Renderer};
use resvg::tiny_skia::Pixmap;
use std::{
	collections::HashMap,
	sync::{
		Arc, LazyLock, Mutex as StdMutex,
		atomic::{AtomicBool, AtomicU32, Ordering},
	},
	time::{Duration, Instant},
};
use tauri::Emitter;
use tokio::{
	sync::{Mutex, mpsc},
	task::JoinHandle,
};

pub use render::Scene;
pub const ACTION: &str = "dev.clocky.neo.clockface";
pub const PLUGIN: &str = "dev.clocky.neo.sdPlugin";

struct Slot {
	compatible: bool,
	requested: bool,
	enabled: bool,
	worker: Option<Worker>,
}

struct Worker {
	active: Arc<AtomicBool>,
	input: mpsc::Sender<Command>,
	task: JoinHandle<()>,
	snapshot: Arc<StdMutex<Snapshot>>,
}

#[derive(Default)]
struct Snapshot {
	frame: Option<Pixmap>,
	rect: Option<Rect>,
	revision: u32,
	previous: u32,
}

enum Command {
	Scene(Scene),
	Refresh,
}
static SLOTS: LazyLock<Mutex<HashMap<ActionContext, Slot>>> = LazyLock::new(Default::default);
static WATCHING: LazyLock<dashmap::DashSet<String>> = LazyLock::new(Default::default);
static REVISION: AtomicU32 = AtomicU32::new(1);

fn compatible(instance: &ActionInstance) -> bool {
	let Some(state) = instance.states.get(instance.current_state as usize) else { return false };
	state.image_override.is_none() && matches!(state.image_scale, 0 | 100) && (!state.show || state.text_override.as_deref().unwrap_or(&state.text).is_empty())
}

pub async fn appear(instance: &ActionInstance) -> bool {
	if instance.action.uuid != ACTION || instance.action.plugin != PLUGIN || instance.context.controller != "Infobar" {
		return false;
	}
	let neo = crate::shared::DEVICES.get(&instance.context.device).is_some_and(|d| d.plugin.is_empty() && d.infobars > 0);
	if !neo {
		return false;
	}
	stop(&instance.context).await;
	SLOTS.lock().await.insert(
		instance.context.clone(),
		Slot {
			compatible: compatible(instance),
			requested: false,
			enabled: false,
			worker: None,
		},
	);
	true
}

fn notify_preview(key: &ActionContext) {
	let context = key.to_string();
	if WATCHING.contains(&context)
		&& let Some(app) = crate::APP_HANDLE.get()
	{
		let _ = app.emit_to("main", "sonic_infobar_preview", context);
	}
}

async fn stop_worker(slot: &mut Slot, key: &ActionContext) {
	if let Some(worker) = slot.worker.take() {
		worker.task.abort();
		let _ = worker.task.await;
		notify_preview(key);
	}
}

pub async fn stop(key: &ActionContext) {
	let mut slots = SLOTS.lock().await;
	if let Some(mut slot) = slots.remove(key) {
		stop_worker(&mut slot, key).await;
	}
}

pub async fn disconnected() {
	let mut slots = SLOTS.lock().await;
	for (key, slot) in slots.iter_mut() {
		slot.requested = false;
		slot.enabled = false;
		stop_worker(slot, key).await;
	}
}

async fn send_mode(key: &ActionContext, enabled: bool) -> Result<()> {
	crate::events::outbound::send_to_plugin(PLUGIN, &serde_json::json!({"event":"sonicInfobarMode","context":key,"payload":{"enabled":enabled}})).await
}

async fn configure(key: &ActionContext, slot: &mut Slot, rotation: u16, force: bool) -> Result<()> {
	if !slot.requested {
		return Ok(());
	}
	let enabled = slot.compatible && rotation == 0;
	if enabled != slot.enabled || force {
		slot.enabled = enabled;
		if !enabled || slot.worker.as_ref().is_some_and(|w| !w.active.load(Ordering::Acquire)) {
			stop_worker(slot, key).await;
		}
		send_mode(key, enabled).await?;
	}
	Ok(())
}

pub async fn request(key: &ActionContext) -> Result<()> {
	if let Some(slot) = SLOTS.lock().await.get_mut(key) {
		slot.requested = true;
		configure(key, slot, crate::store::get_settings().value.rotation, true).await?;
	}
	Ok(())
}

pub async fn appearance_changed(instance: &ActionInstance) -> Result<()> {
	if instance.action.uuid == ACTION
		&& let Some(slot) = SLOTS.lock().await.get_mut(&instance.context)
	{
		slot.compatible = compatible(instance);
		configure(&instance.context, slot, crate::store::get_settings().value.rotation, false).await?;
	}
	Ok(())
}

pub async fn settings_changed(rotation: u16) -> Result<()> {
	for (key, slot) in SLOTS.lock().await.iter_mut() {
		configure(key, slot, rotation, false).await?;
		if let Some(worker) = &slot.worker {
			let _ = worker.input.send(Command::Refresh).await;
		}
	}
	Ok(())
}

pub async fn refresh_device(device: &str) {
	for (key, slot) in SLOTS.lock().await.iter() {
		if key.device == device
			&& let Some(worker) = &slot.worker
		{
			let _ = worker.input.send(Command::Refresh).await;
		}
	}
}

pub async fn owns(context: &Context) -> bool {
	if context.controller != "Infobar" {
		return false;
	}
	SLOTS
		.lock()
		.await
		.iter()
		.any(|(k, s)| k.device == context.device && k.profile == context.profile && k.position == context.position && s.worker.as_ref().is_some_and(|w| w.active.load(Ordering::Acquire)))
}

pub async fn submit(key: ActionContext, scene: Scene) -> Result<()> {
	let mut slots = SLOTS.lock().await;
	let Some(slot) = slots.get_mut(&key) else { return Ok(()) };
	if !slot.enabled {
		return Ok(());
	}
	if slot.worker.is_none() {
		ensure!(scene.direction.is_some(), "Missing initial Infobar scene");
		let (input, rx) = mpsc::channel(8);
		let snapshot = Arc::new(StdMutex::new(Snapshot::default()));
		let saved = snapshot.clone();
		let ctx = key.clone();
		let active = Arc::new(AtomicBool::new(true));
		let running = active.clone();
		let task = tokio::spawn(async move {
			let result = run(&ctx, rx, saved).await;
			running.store(false, Ordering::Release);
			if let Err(error) = result {
				log::warn!("Falling back to standard Infobar rendering: {error}");
				let _ = send_mode(&ctx, false).await;
				notify_preview(&ctx);
			}
		});
		slot.worker = Some(Worker { active, input, task, snapshot });
	}
	let _ = slot.worker.as_ref().unwrap().input.send(Command::Scene(scene)).await;
	Ok(())
}

async fn deadline(t: Option<Instant>) {
	match t {
		Some(t) => tokio::time::sleep_until(t.into()).await,
		None => std::future::pending::<()>().await,
	}
}

async fn run(key: &ActionContext, mut input: mpsc::Receiver<Command>, saved: Arc<StdMutex<Snapshot>>) -> Result<()> {
	let mut renderer = Renderer::new(Instant::now());
	let mut due = None;
	let mut next_write: Option<Instant> = None;
	let mut force = true;
	let mut settings = crate::store::get_settings().value;
	loop {
		let command = tokio::select! {
			command = input.recv() => match command { Some(c) => Some(c), None => return Ok(()) },
			_ = deadline(due) => None,
		};
		if let Some(c) = command {
			match c {
				Command::Scene(s) => renderer.update(s, Instant::now())?,
				Command::Refresh => {
					settings = crate::store::get_settings().value;
					force = true;
				}
			}
		}
		if crate::device_sleep::is_device_sleeping(&key.device) {
			due = None;
			force = true;
			continue;
		}
		let interval = Duration::from_secs_f64(1. / if settings.animation_fps == 60 { 60. } else { 30. });
		let now = Instant::now();
		if settings.reduce_tearing
			&& let Some(next) = next_write
			&& now < next
		{
			due = Some(next);
			continue;
		}
		let frame = renderer.render(now);
		let rect = {
			let snap = saved.lock().unwrap();
			render::damage(if force { None } else { snap.frame.as_ref() }, &frame, renderer.media_partial())
		};
		if let Some(rect) = rect {
			let window = rect.jpeg_blocks();
			let jpeg = render::encode(&frame, window)?;
			crate::elgato::write_infobar(&key.device, window, jpeg).await?;
			let mut snap = saved.lock().unwrap();
			snap.previous = snap.revision;
			snap.revision = REVISION.fetch_add(1, Ordering::Relaxed);
			snap.frame = Some(frame);
			snap.rect = Some(rect);
			drop(snap);
			notify_preview(key);
			// Anchor normal ticks to their deadlines so timer wakeup jitter does not accumulate.
			// After a stall, resume from the frame being shown and discard missed ticks.
			let tick = next_write.filter(|&t| t <= now && now - t <= interval / 3).unwrap_or(now);
			next_write = Some(tick + interval);
			force = false;
		}
		due = renderer.next(now, interval).map(|delay| {
			if delay == interval {
				next_write.filter(|&t| t > now).unwrap_or(now + interval)
			} else {
				now + delay
			}
		});
	}
}

pub(crate) use render::Rect as WindowRect;

#[tauri::command]
pub async fn watch_infobar(context: String, visible: bool) {
	if visible {
		WATCHING.insert(context);
	} else {
		WATCHING.remove(&context);
	}
}

#[tauri::command]
pub async fn infobar_preview(context: ActionContext, revision: u32) -> tauri::ipc::Response {
	let slots = SLOTS.lock().await;
	let Some(worker) = slots.get(&context).and_then(|s| s.worker.as_ref()).filter(|w| w.active.load(Ordering::Acquire)) else {
		return tauri::ipc::Response::new(Vec::new());
	};
	let snap = worker.snapshot.lock().unwrap();
	let Some(frame) = snap.frame.as_ref() else { return tauri::ipc::Response::new(Vec::new()) };
	if revision == snap.revision {
		return tauri::ipc::Response::new(vec![0u8]);
	}
	let r = if revision != 0 && snap.previous == revision { snap.rect.unwrap() } else { FULL };
	let mut bytes = Vec::with_capacity(8 + (r.w * r.h * 4) as usize);
	bytes.extend(snap.revision.to_le_bytes());
	bytes.extend([r.x as u8, r.y as u8, r.w as u8, r.h as u8]);
	for y in r.y..r.y + r.h {
		let i = ((y * render::WIDTH + r.x) * 4) as usize;
		bytes.extend_from_slice(&frame.data()[i..i + (r.w * 4) as usize]);
	}
	tauri::ipc::Response::new(bytes)
}
