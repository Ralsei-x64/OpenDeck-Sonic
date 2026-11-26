// WebKit also drives the physical Infobar when the editor is hidden. App Nap can defer
// the first frames of a swipe if the activity is acquired after those frames arrive.
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};
use std::cell::RefCell;

thread_local! {
	static ACTIVITY: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> = const { RefCell::new(None) };
}

pub fn refresh() {
	let Some(app) = crate::APP_HANDLE.get() else { return };
	let _ = app.run_on_main_thread(|| {
		let enabled = crate::events::outbound::misc::frame_rate() == 60
			&& crate::shared::DEVICES.iter().any(|device| device.plugin.is_empty() && device.infobars > 0 && !crate::device_sleep::is_device_sleeping(&device.id));
		ACTIVITY.with_borrow_mut(|activity| {
			if enabled == activity.is_some() {
				return;
			}
			let info = NSProcessInfo::processInfo();
			if let Some(token) = activity.take() {
				// SAFETY: the token was created by this process and is ended exactly once on the main thread.
				unsafe { info.endActivity(&token) };
			} else {
				let options = NSActivityOptions::UserInitiatedAllowingIdleSystemSleep | NSActivityOptions::LatencyCritical | NSActivityOptions::AnimationTrackingEnabled;
				*activity = Some(info.beginActivityWithOptions_reason(options, &NSString::from_str("Driving a Stream Deck Neo at 60 fps")));
			}
		});
	});
}
