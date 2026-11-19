// Desktop keyboard shortcuts: Space play/pause, ←/→ seek 10 s, Shift+N / Shift+P next / previous.

export type ShortcutControls = {
  togglePlay: () => void;
  seekBy: (seconds: number) => void;
  next: () => void;
  previous: () => void;
};

export type ShortcutKey = Pick<KeyboardEvent, 'key' | 'shiftKey' | 'ctrlKey' | 'metaKey' | 'altKey'> & {
  target: EventTarget | null;
};

const typingRoles = new Set(['slider', 'textbox', 'searchbox', 'combobox', 'menuitem', 'option', 'spinbutton']);

/** True when the key press belongs to whatever has focus (a field, button, slider, video…). */
function focusOwnsKey(target: EventTarget | null, key: string): boolean {
  if (!target || typeof (target as Element).closest !== 'function') return false;
  const element = target as HTMLElement;
  if (element.isContentEditable) return true;
  if (element.closest('input, textarea, select, video, audio, [contenteditable="true"]')) return true;
  const role = element.getAttribute('role');
  if (role && typingRoles.has(role)) return true;
  // Space activates buttons and links; leave it to them.
  if (key === ' ' && element.closest('button, a[href], summary, [role="button"], [role="tab"]')) return true;
  return false;
}

/** Runs the matching shortcut. Returns true if the key was handled (the caller prevents the default). */
export function handleShortcut(event: ShortcutKey, controls: ShortcutControls): boolean {
  if (event.ctrlKey || event.metaKey || event.altKey) return false;
  if (focusOwnsKey(event.target, event.key)) return false;
  if (event.key === ' ' && !event.shiftKey) {
    controls.togglePlay();
    return true;
  }
  if (event.key === 'ArrowLeft' && !event.shiftKey) {
    controls.seekBy(-10);
    return true;
  }
  if (event.key === 'ArrowRight' && !event.shiftKey) {
    controls.seekBy(10);
    return true;
  }
  if (event.shiftKey && (event.key === 'N' || event.key === 'n')) {
    controls.next();
    return true;
  }
  if (event.shiftKey && (event.key === 'P' || event.key === 'p')) {
    controls.previous();
    return true;
  }
  return false;
}
