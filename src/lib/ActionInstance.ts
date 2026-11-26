import type { Action } from "./Action.ts";
import type { ActionState } from "./ActionState.ts";

export type ActionInstance = {
	action: Action;
	context: string;
	states: ActionState[];
	current_state: number;
	settings: any;
	children: ActionInstance[] | null;
};

export type StateUpdate =
	| { context: string; contents: ActionInstance | null }
	| { context: string; action: string; state: number | null; image: string | null };

export function applyStateUpdate(slot: ActionInstance, update: StateUpdate): ActionInstance | null {
	if (update.context !== slot.context) return slot;
	if ("contents" in update) return update.contents;
	if (update.action !== slot.action.uuid) return slot;
	const states = slot.states.map((state, i) =>
		update.state === null || update.state === i ? { ...state, image: update.image ?? slot.action.states[i]?.image ?? slot.action.icon } : state,
	);
	return { ...slot, states };
}
