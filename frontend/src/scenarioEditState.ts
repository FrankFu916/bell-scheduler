import { scenarioTimetableKey, type ScenarioActivityLockState, type ScenarioTimetablePage } from "./scenarioTimetableApi.ts";
import type { ScenarioReceipt } from "./scenarioApi.ts";
import type { TimetableGridCell, TimetableRow } from "./timetableApi.ts";

/** Only a temporary selection of a row returned by the application. It never edits a timetable. */
export interface ScenarioEditSelection {
  readonly receipt: ScenarioReceipt;
  readonly row: TimetableRow;
  readonly lock: ScenarioActivityLockState;
  readonly calendar: readonly TimetableGridCell[];
}

export function selectScenarioActivity(page: ScenarioTimetablePage, activityId: string): ScenarioEditSelection {
  const row = page.rows.find((item) => item.activityId === activityId);
  const lock = page.activityLocks.find((item) => item.activityId === activityId);
  if (!row || !lock) throw { schemaVersion: 1, code: "DESKTOP_SCENARIO_EDIT_SELECTION_MISSING",
    message: "所选课次不在当前已校验页面中，请重新打开课次。", details: null };
  return { receipt: page.receipt, row, lock, calendar: page.calendar };
}

export function sameEditRevision(left: ScenarioEditSelection, right: ScenarioEditSelection): boolean {
  return scenarioTimetableKey(left.receipt) === scenarioTimetableKey(right.receipt);
}

/** The full receipt and form selection scope every response, including failures and finally blocks. */
export function createScenarioEditGuard() {
  let current: { readonly token: symbol; readonly key: string } | null = null;
  return {
    begin(key: string): symbol | null {
      if (current !== null) return null;
      const token = Symbol("scenario edit");
      current = { token, key };
      return token;
    },
    accepts(token: symbol, key: string): boolean { return current?.token === token && current.key === key; },
    finish(token: symbol): void { if (current?.token === token) current = null; },
    invalidate(): void { current = null; },
  };
}
