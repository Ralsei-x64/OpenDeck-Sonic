use super::send_to_all_plugins;

use serde::Serialize;

#[derive(Serialize)]
struct SystemDidWakeUpEvent {
	event: &'static str,
}

pub async fn system_did_wake_up() -> Result<(), anyhow::Error> {
	send_to_all_plugins(&SystemDidWakeUpEvent { event: "systemDidWakeUp" }).await
}

// Plugins opt in at registration. A new connection gets the current rate immediately.
static FRAME_RATE: std::sync::LazyLock<tokio::sync::watch::Sender<u8>> =
	std::sync::LazyLock::new(|| tokio::sync::watch::channel(crate::store::get_settings().value.animation_fps).0);

#[cfg(target_os = "macos")]
pub fn frame_rate() -> u8 {
	*FRAME_RATE.borrow()
}

pub fn set_frame_rate(fps: u8) {
	let changed = FRAME_RATE.send_if_modified(|current| {
		let changed = *current != fps;
		*current = fps;
		changed
	});
	#[cfg(target_os = "macos")]
	if changed {
		crate::display_activity::refresh();
	}
	#[cfg(not(target_os = "macos"))]
	let _ = changed;
}

pub async fn stream_frame_rate(plugin: String) {
	let mut rates = FRAME_RATE.subscribe();
	loop {
		let fps = *rates.borrow_and_update();
		let event = serde_json::json!({"event": "sonicFrameRateDidChange", "payload": {"fps": fps}});
		if super::send_to_plugin(&plugin, &event).await.is_err() || rates.changed().await.is_err() {
			break;
		}
	}
}
