// Shared debug-mode state — DebugGear writes, anyone reads.
//
// Debug mode gates operator-only UI affordances (PersonaSwitcher,
// convenience action buttons, etc.) behind a single toggle so the
// default view stays clean.

const STORAGE_KEY = 'boss.debug.enabled';

function readStored(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) === 'true';
  } catch {
    return false;
  }
}

export const debugState = $state<{ enabled: boolean }>({
  enabled: readStored(),
});

export function setDebugMode(on: boolean): void {
  debugState.enabled = on;
  try {
    localStorage.setItem(STORAGE_KEY, String(on));
  } catch {
    // localStorage unavailable — state still flips for this tab.
  }
}
