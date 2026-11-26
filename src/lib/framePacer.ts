// Each device slot owns one presenter. The pending task captures an already encoded JPEG.
type Write = () => Promise<void>;

export class FramePacer {
	route = "";
	private interval = 0;
	private deadline = 0;
	private pending?: Write;
	private writing?: Promise<void>;
	private wake?: Promise<void>;
	private closed = false;

	private readonly wait: (milliseconds: number) => Promise<void>;

	constructor(wait: (milliseconds: number) => Promise<void>) {
		this.wait = wait;
	}

	configure(route: string, fps: number) {
		const interval = fps ? 1000 / fps : 0;
		if (route === this.route && interval === this.interval) return;
		if (route !== this.route) this.pending = undefined;
		this.route = route;
		this.interval = interval;
		this.deadline = 0;
		this.wake = undefined;
		if (!interval && this.pending) {
			const write = this.pending;
			this.pending = undefined;
			void this.start(write).catch((error) => console.error("Device frame write failed", error));
			return;
		}
		this.pump();
	}

	submit(route: string, write: Write): Promise<void> {
		if (this.closed || route !== this.route) return Promise.resolve();
		const ifCurrent = () => (this.closed || route !== this.route ? Promise.resolve() : write());
		if (!this.interval) return this.start(ifCurrent);
		this.pending = ifCurrent;
		this.pump();
		return Promise.resolve();
	}

	private start(write: Write) {
		const transfer = (this.writing ?? Promise.resolve()).then(write);
		this.writing = transfer.catch(() => {});
		const current = this.writing;
		void current.then(() => {
			if (current === this.writing) {
				this.writing = undefined;
				this.pump();
			}
		});
		return transfer;
	}

	private pump() {
		if (this.closed || !this.pending || this.writing) return;
		const now = performance.now();
		if (now < this.deadline) {
			if (!this.wake) {
				const wake = this.wait(this.deadline - now);
				this.wake = wake;
				void wake.catch((error) => {
					console.error("Frame pacing timer failed", error);
					if (this.wake === wake) this.deadline = 0;
				}).then(() => {
					if (this.wake !== wake) return;
					this.wake = undefined;
					this.pump();
				});
			}
			return;
		}

		this.wake = undefined;
		const write = this.pending;
		this.pending = undefined;
		// Anchor to the previous slot through timer jitter. Rebase after a stall or idle period.
		this.deadline = (!this.deadline || now - this.deadline > this.interval / 4 ? now : this.deadline) + this.interval;
		void this.start(write).catch((error) => console.error("Device frame write failed", error));
	}

	close() {
		this.closed = true;
		this.pending = undefined;
		this.wake = undefined;
	}
}
