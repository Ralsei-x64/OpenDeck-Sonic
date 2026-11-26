// macOS can defer hidden WebKit work by 100 ms even while the physical Infobar is animating.
use objc2::rc::autoreleasepool;
use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
use std::sync::{
	LazyLock,
	mpsc::{Receiver, SyncSender, sync_channel},
};
use std::time::Duration;

const BURST_GAP: Duration = Duration::from_millis(100);

static FRAMES: LazyLock<SyncSender<()>> = LazyLock::new(|| {
	// Only the latest pulse matters. Device input never waits for this worker.
	let (tx, rx) = sync_channel(1);
	if let Err(error) = std::thread::Builder::new().name("infobar-activity".into()).spawn(move || monitor(rx)) {
		log::warn!("Could not start the Infobar scheduling worker: {error}");
	}
	tx
});

pub fn record_frame() {
	let _ = FRAMES.try_send(());
}

fn monitor(frames: Receiver<()>) {
	while frames.recv().is_ok() {
		// An isolated clock update needs no activity assertion.
		if frames.recv_timeout(BURST_GAP).is_err() {
			continue;
		}
		autoreleasepool(|_| {
			let info = NSProcessInfo::processInfo();
			let options = NSActivityOptions::UserInitiatedAllowingIdleSystemSleep | NSActivityOptions::LatencyCritical | NSActivityOptions::AnimationTrackingEnabled;
			let activity = info.beginActivityWithOptions_reason(options, &NSString::from_str("Animating the Stream Deck Infobar"));
			while frames.recv_timeout(BURST_GAP).is_ok() {}
			// SAFETY: this worker owns the token returned above and ends it exactly once.
			unsafe { info.endActivity(&activity) };
		});
	}
}
