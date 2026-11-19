// Pointer-driven drag reordering for lists (works with mouse, pen and touch).

/** Where a row dragged to `pointerY` should land, given every row's vertical extent. */
export function dropIndex(pointerY: number, rows: readonly { top: number; bottom: number }[], from: number): number {
  if (rows.length === 0) return from;
  for (let index = 0; index < rows.length; index += 1) {
    const row = rows[index];
    if (!row) continue;
    const middle = (row.top + row.bottom) / 2;
    if (pointerY < middle) return index > from ? index - 1 : index;
  }
  return rows.length - 1;
}

export type ReorderOptions = {
  /** The list element whose direct children are the rows. */
  list: () => HTMLElement | undefined;
  onMove: (from: number, to: number) => void;
};

/**
 * Starts a drag from a handle's pointerdown. The row follows the pointer
 * (via a CSS custom property) and `onMove` fires once on release.
 */
export function startDrag(event: PointerEvent, from: number, options: ReorderOptions): void {
  const list = options.list();
  const handle = event.currentTarget as HTMLElement | null;
  if (!list || !handle || event.button !== 0) return;
  const rows = Array.from(list.children) as HTMLElement[];
  const row = rows[from];
  if (!row) return;
  event.preventDefault();
  const rects = rows.map((element) => element.getBoundingClientRect());
  const startY = event.clientY;
  handle.setPointerCapture?.(event.pointerId);
  row.dataset.dragging = 'true';

  const onPointerMove = (moveEvent: PointerEvent) => {
    row.style.setProperty('--drag-offset', `${moveEvent.clientY - startY}px`);
  };
  const finish = (endEvent: PointerEvent, commit: boolean) => {
    handle.removeEventListener('pointermove', onPointerMove);
    handle.removeEventListener('pointerup', onPointerUp);
    handle.removeEventListener('pointercancel', onPointerCancel);
    row.style.removeProperty('--drag-offset');
    delete row.dataset.dragging;
    if (!commit) return;
    const to = dropIndex(endEvent.clientY, rects, from);
    if (to !== from) options.onMove(from, to);
  };
  const onPointerUp = (upEvent: PointerEvent) => finish(upEvent, true);
  const onPointerCancel = (cancelEvent: PointerEvent) => finish(cancelEvent, false);
  handle.addEventListener('pointermove', onPointerMove);
  handle.addEventListener('pointerup', onPointerUp);
  handle.addEventListener('pointercancel', onPointerCancel);
}

/** Keyboard reordering on a drag handle: ↑/↓ move one place. Returns true if handled. */
export function handleReorderKey(
  event: KeyboardEvent,
  index: number,
  count: number,
  onMove: (from: number, to: number) => void,
): boolean {
  if (event.key === 'ArrowUp' && index > 0) {
    onMove(index, index - 1);
    return true;
  }
  if (event.key === 'ArrowDown' && index < count - 1) {
    onMove(index, index + 1);
    return true;
  }
  return false;
}
