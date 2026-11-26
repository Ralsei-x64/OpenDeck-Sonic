use super::{GenericInstancePayload, send_to_plugin};

use crate::shared::{ActionContext, ActionInstance};

#[derive(serde::Serialize)]
struct AppearEvent {
	event: &'static str,
	action: String,
	context: ActionContext,
	device: String,
	payload: GenericInstancePayload,
	#[serde(rename="sonicInfobar", skip_serializing_if="Option::is_none")]
	sonic_infobar: Option<bool>,
}

pub async fn will_appear(instance: &ActionInstance) -> Result<(), anyhow::Error> {
	send_to_plugin(
		&instance.action.plugin,
		&AppearEvent {
			event: "willAppear",
			sonic_infobar: crate::infobar::appear(instance).await.then_some(true),
			action: instance.action.uuid.clone(),
			context: instance.context.clone(),
			device: instance.context.device.clone(),
			payload: GenericInstancePayload::new(instance),
		},
	)
	.await?;

	super::states::title_parameters_did_change(instance, instance.current_state).await?;

	Ok(())
}

pub async fn will_disappear(instance: &ActionInstance, clear_on_device: bool) -> Result<(), anyhow::Error> {
	if instance.action.uuid == crate::infobar::ACTION { crate::infobar::stop(&instance.context).await; }
	send_to_plugin(
		&instance.action.plugin,
		&AppearEvent {
			event: "willDisappear",
			sonic_infobar: None,
			action: instance.action.uuid.clone(),
			context: instance.context.clone(),
			device: instance.context.device.clone(),
			payload: GenericInstancePayload::new(instance),
		},
	)
	.await?;

	if clear_on_device && let Err(error) = crate::events::outbound::devices::update_image((&instance.context).into(), None).await {
		log::warn!("Failed to clear device image: {}", error);
	}

	Ok(())
}
