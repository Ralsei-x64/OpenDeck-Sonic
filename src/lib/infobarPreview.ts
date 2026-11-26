import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { CanvasLock } from "./rendererHelper";

// A profile refresh can mount a new canvas before its previous instance finishes
// unsubscribing. Order the native calls across all canvases for this window.
const viewers = new Map<string, number>();
let subscriptions = Promise.resolve();
function watch(context: string, visible: boolean): Promise<void> {
    const count = viewers.get(context) ?? 0;
    const next = Math.max(0, count + (visible ? 1 : -1));
    if (next) viewers.set(context, next);
    else viewers.delete(context);
    if ((next === 0) !== (count === 0)) {
        subscriptions = subscriptions.then(async () => {
            if (viewers.has(context) === visible) await invoke("watch_infobar", {context, visible});
        }).catch(error => console.error("Infobar subscription failed", error));
    }
    return subscriptions;
}

// The device renderer already has the logical pixels. Request a patch only while this
// canvas is visible. A missed revision (including reopening the editor) gets a full image.
export function watchPreview(canvas: HTMLCanvasElement, lock: CanvasLock, setOwned: (owned: boolean) => void, initial: string) {
	let context = "";
	let revision = 0;
	let generation = 0;
	let busy = false;
	let pending = false;
	let off: (() => void) | undefined;
	let disposed = false;
	let watching = "";

	async function refresh() {
		pending = true;
		if (busy) return;
		busy = true;
		try {
			while (pending && watching && !disposed) {
				pending = false;
				const current = generation;
				const bytes = new Uint8Array(await invoke<ArrayBuffer>("infobar_preview", {context: watching, revision}));
				if (current !== generation) continue;
				if (bytes.length === 0) {
					revision = 0;
					setOwned(false);
					continue;
				}
				if (bytes.length < 8) continue;
				setOwned(true);
				const unlock = await lock.lock();
				try {
					if (current !== generation) continue;
					const [x,y,w,h] = bytes.subarray(4,8);
					canvas.getContext("2d")?.putImageData(new ImageData(new Uint8ClampedArray(bytes.buffer, 8), w, h), x, y);
					revision = new DataView(bytes.buffer).getUint32(0,true);
				} finally { unlock(); }
			}
		} catch (error) { console.error("Infobar preview failed", error); }
		finally { busy = false; }
	}

	function visibility() {
		const next = !disposed && !document.hidden ? context : "";
		if (watching === next) return;
		generation++;
		revision = 0;
		if (watching) void watch(watching, false);
		watching = next;
		if (watching) {
			const current = generation;
			void watch(watching, true).then(() => { if (current === generation) void refresh(); });
		}
	}

	function update(next: string) {
		if (context === next) return;
		context = next;
		setOwned(false);
		visibility();
	}
	// Install the listener before the initial snapshot to avoid missing the first frame.
	void listen<string>("sonic_infobar_preview", ({payload}) => { if (payload === watching) void refresh(); }, { target: "main" }).then((stop) => {
		if (disposed) { stop(); return; }
		off = stop;
		if (watching) void refresh();
	});
	document.addEventListener("visibilitychange", visibility);
	update(initial);
	return { update, destroy() {
		disposed = true;
		visibility();
		off?.();
		document.removeEventListener("visibilitychange", visibility);
	} };
}
