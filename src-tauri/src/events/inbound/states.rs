use super::ContextAndPayloadEvent;

use crate::events::frontend::instances::{update_state, update_state_image};
use crate::shared::ActionInstance;
use crate::store::profiles::{acquire_locks_mut, get_instance_mut, mark_profile_stale};

use anyhow::bail;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
pub struct SetTitlePayload {
	title: Option<String>,
	state: Option<u16>,
}

#[derive(Deserialize)]
pub struct SetImagePayload {
	image: Option<String>,
	state: Option<u16>,
}

impl SetImagePayload {
	fn apply(&self, instance: &mut ActionInstance) -> anyhow::Result<()> {
		if let Some(index) = self.state
			&& index as usize >= instance.states.len()
		{
			bail!("State index {index} out of bounds for {} states", instance.states.len());
		}
		for (index, state) in instance.states.iter_mut().enumerate() {
			if self.state.is_none_or(|target| target as usize == index) {
				let image = self.image.as_ref().unwrap_or_else(|| instance.action.states.get(index).map(|s| &s.image).unwrap_or(&instance.action.icon));
				state.image.clone_from(image);
			}
		}
		Ok(())
	}
}

#[derive(Deserialize)]
pub struct SetStatePayload {
	state: u16,
}

#[derive(Debug, Deserialize)]
pub struct SetFeedbackLayoutPayload {
	layout: String,
}

pub async fn set_title(event: ContextAndPayloadEvent<SetTitlePayload>) -> Result<(), anyhow::Error> {
	let mut locks = acquire_locks_mut().await;

	if let Some(instance) = get_instance_mut(&event.context, &mut locks).await? {
		if let Some(state) = event.payload.state {
			if state as usize >= instance.states.len() {
				return Err(anyhow::anyhow!("State index out of bounds ({} > {})", state, instance.states.len() - 1));
			}

			let text = event.payload.title.unwrap_or(instance.action.states[state as usize].text.clone());
			if instance.states[state as usize].text == text {
				return Ok(());
			}
			instance.states[state as usize].text = text;
		} else {
			if instance
				.states
				.iter()
				.enumerate()
				.all(|(index, state)| state.text == event.payload.title.clone().unwrap_or(instance.action.states[index].text.clone()))
			{
				return Ok(());
			}

			for (index, state) in instance.states.iter_mut().enumerate() {
				state.text = event.payload.title.clone().unwrap_or(instance.action.states[index].text.clone());
			}
		}
		update_state(crate::APP_HANDLE.get().unwrap(), instance.context.clone(), &mut locks).await?;
	}
	mark_profile_stale(&event.context.device, &mut locks).await?;

	Ok(())
}

pub async fn set_image(mut event: ContextAndPayloadEvent<SetImagePayload>) -> Result<(), anyhow::Error> {
	let mut locks = acquire_locks_mut().await;

	if let Some(instance) = get_instance_mut(&event.context, &mut locks).await? {
		if let Some(image) = &event.payload.image {
			if image.trim().is_empty() {
				event.payload.image = None;
			} else if !image.trim().starts_with("data:") {
				event.payload.image = Some(crate::shared::convert_icon(
					crate::shared::config_dir()
						.join("plugins")
						.join(&instance.action.plugin)
						.join(image.trim())
						.to_str()
						.unwrap()
						.to_owned(),
				));
			}
		}

		event.payload.apply(instance)?;
		#[cfg(target_os = "macos")]
		if instance.context.controller == "Infobar" && crate::events::outbound::misc::frame_rate() == 60 {
			crate::display_activity::record_frame();
		}
		update_state_image(crate::APP_HANDLE.get().unwrap(), instance, event.payload.state, event.payload.image.as_deref())?;
	}

	mark_profile_stale(&event.context.device, &mut locks).await?;
	Ok(())
}

pub async fn set_feedback(event: ContextAndPayloadEvent<Value>) -> Result<(), anyhow::Error> {
	let mut locks = acquire_locks_mut().await;

	if let Some(instance) = get_instance_mut(&event.context, &mut locks).await?
		&& let Some(encoder) = &mut instance.action.encoder
	{
		let Some(layout) = &mut encoder.layout_parsed else {
			bail!("Layout is not loaded; cannot set feedback");
		};

		layout.set_feedback(event.payload)?;
		update_state(crate::APP_HANDLE.get().unwrap(), instance.context.clone(), &mut locks).await?;
	}

	Ok(())
}

pub async fn set_feedback_layout(event: ContextAndPayloadEvent<SetFeedbackLayoutPayload>) -> Result<(), anyhow::Error> {
	let mut locks = acquire_locks_mut().await;
	if let Some(instance) = get_instance_mut(&event.context, &mut locks).await? {
		// We need to replace the existing parsed layout with the new one
		let layout_name = event.payload.layout.clone();
		crate::shared::initialise_encoder_layout(&mut instance.action, Some(layout_name))?;

		// Trigger a state update; should cause a redraw
		update_state(crate::APP_HANDLE.get().unwrap(), instance.context.clone(), &mut locks).await?;
	}
	Ok(())
}

pub async fn set_state(event: ContextAndPayloadEvent<SetStatePayload>) -> Result<(), anyhow::Error> {
	let mut locks = acquire_locks_mut().await;

	if let Some(instance) = get_instance_mut(&event.context, &mut locks).await? {
		if event.payload.state >= instance.states.len() as u16 {
			return Ok(());
		}
		instance.current_state = event.payload.state;
		update_state(crate::APP_HANDLE.get().unwrap(), instance.context.clone(), &mut locks).await?;
	}
	mark_profile_stale(&event.context.device, &mut locks).await?;

	Ok(())
}

#[cfg(test)]
mod image_tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn frames_only_change_the_target_images() {
		let mut instance: ActionInstance = serde_json::from_value(json!({
			"context": "qa.Default.Infobar.0.0", "current_state": 0, "children": null,
			"settings": { "wallpaper": "w".repeat(1_090_000) },
			"action": { "Name": "Clock", "UUID": "qa.clock", "Icon": "icon", "States": [{"Image": "a"}, {"Image": "b"}] },
			"states": [{"image": "one", "image_override": "user"}, {"image": "two"}, {"image": "dynamic"}]
		})).unwrap();
		let wallpaper = instance.settings["wallpaper"].as_str().unwrap().as_ptr();
		let images = |i: &ActionInstance| i.states.iter().map(|s| s.image.clone()).collect::<Vec<_>>();
		SetImagePayload { image: Some("frame".into()), state: Some(1) }.apply(&mut instance).unwrap();
		assert_eq!(images(&instance), ["one", "frame", "dynamic"]);
		SetImagePayload { image: Some("all".into()), state: None }.apply(&mut instance).unwrap();
		assert_eq!(images(&instance), ["all", "all", "all"]);
		assert!(SetImagePayload { image: None, state: Some(9) }.apply(&mut instance).is_err());
		assert_eq!(images(&instance), ["all", "all", "all"]);
		SetImagePayload { image: None, state: None }.apply(&mut instance).unwrap();
		assert_eq!(images(&instance), ["a", "b", "icon"]);
		assert_eq!(instance.states[0].image_override.as_deref(), Some("user"));
		assert_eq!(instance.settings["wallpaper"].as_str().unwrap().as_ptr(), wallpaper);
	}
}
