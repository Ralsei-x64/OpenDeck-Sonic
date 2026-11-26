import assert from "node:assert/strict";
import test from "node:test";
import { FramePacer } from "../src/lib/framePacer.ts";

const wait = (ms) => new Promise((resolve) => setTimeout(resolve, Math.ceil(ms)));
const drain = () => new Promise((resolve) => setImmediate(resolve));

for (const fps of [30, 60]) {
	test(`paced delivery at ${fps} fps sends the newest complete frame and flushes the settled image`, async (t) => {
		t.mock.timers.enable({ apis: ["Date", "setTimeout"], now: 1000 });
		t.mock.method(performance, "now", () => Date.now());
		const pacer = new FramePacer(wait);
		pacer.configure("neo", fps);
		const writes = [];
		const frame = (id) => pacer.submit("neo", async () => { writes.push(id); });
		await frame(1);
		await frame(2);
		await frame(3);
		await drain();
		const period = Math.ceil(1000 / fps);
		t.mock.timers.tick(period - 1);
		await drain();
		assert.deepEqual(writes, [1]);
		t.mock.timers.tick(1);
		await drain();
		assert.deepEqual(writes, [1, 3]);
		await frame(4);
		pacer.close();
		t.mock.timers.tick(period);
		await drain();
		assert.deepEqual(writes, [1, 3]);
	});
}

test("turning off pacing during a device write preserves the order of subsequent images", async () => {
	const pacer = new FramePacer(wait);
	pacer.configure("neo", 60);
	const writes = [];
	const device = Promise.withResolvers();
	await pacer.submit("neo", async () => {
		writes.push(1);
		await device.promise;
	});
	await pacer.submit("neo", async () => { writes.push(2); });
	pacer.configure("neo", 0);
	const last = pacer.submit("neo", async () => { writes.push(3); });
	assert.deepEqual(writes, [1]);
	device.resolve();
	await last;
	assert.deepEqual(writes, [1, 2, 3]);
	pacer.close();
});

test("switching profiles discards queued frames for the old profile", async () => {
	const pacer = new FramePacer(wait);
	pacer.configure("old", 30);
	const writes = [];
	await pacer.submit("old", async () => { writes.push(1); });
	await pacer.submit("old", async () => { writes.push(2); });
	pacer.configure("new", 60);
	await pacer.submit("old", async () => { writes.push(3); });
	await pacer.submit("new", async () => { writes.push(4); });
	await drain();
	assert.deepEqual(writes, [1, 4]);
	pacer.close();
});

test("a new complete frame can meet its deadline while the previous wake is delayed", async (t) => {
	let now = 100;
	t.mock.method(performance, "now", () => now);
	const wakes = [];
	const pacer = new FramePacer(() => {
		const wake = Promise.withResolvers();
		wakes.push(wake);
		return wake.promise;
	});
	const writes = [];
	const send = (id) => pacer.submit("neo", async () => { writes.push(id); });
	pacer.configure("neo", 60);
	await send(1);
	await drain();
	now = 110;
	await send(2);
	assert.equal(wakes.length, 1);
	now = 117;
	await send(3);
	await drain();
	now = 120;
	await send(4);
	wakes[0].resolve();
	await drain();
	assert.deepEqual(writes, [1, 3]);
	now = 135;
	wakes[1].resolve();
	await drain();
	assert.deepEqual(writes, [1, 3, 4]);
	pacer.close();
});
