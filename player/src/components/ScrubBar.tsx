// The progress bar you can drag (mouse, pen, touch) or move with the keyboard.
// It previews where you'll land while dragging and seeks on release.

import { createSignal } from 'solid-js';
import { formatClock } from '../format';

/** Seconds the arrow keys move; Page Up/Down move six times as far. */
export const SCRUB_STEP_SECONDS = 5;

export function ScrubBar(props: {
  position: number;
  duration: number;
  onSeek: (seconds: number) => void;
  label?: string;
}) {
  const [preview, setPreview] = createSignal<number | null>(null);
  let track: HTMLDivElement | undefined;
  /** Plain flag (not the signal) so back-to-back pointer events see it at once. */
  let dragging = false;

  const shown = () => preview() ?? props.position;
  const fraction = () => (props.duration > 0 ? Math.min(1, Math.max(0, shown() / props.duration)) : 0);
  const remaining = () => Math.max(0, props.duration - shown());

  function secondsAt(clientX: number): number {
    if (!track || props.duration <= 0) return 0;
    const rect = track.getBoundingClientRect();
    const ratio = rect.width > 0 ? (clientX - rect.left) / rect.width : 0;
    return Math.min(1, Math.max(0, ratio)) * props.duration;
  }

  function onPointerDown(event: PointerEvent) {
    if (props.duration <= 0 || event.button !== 0) return;
    const target = event.currentTarget as HTMLElement;
    target.setPointerCapture?.(event.pointerId);
    target.focus();
    dragging = true;
    setPreview(secondsAt(event.clientX));
  }

  function onPointerMove(event: PointerEvent) {
    if (!dragging) return;
    setPreview(secondsAt(event.clientX));
  }

  function onPointerUp(event: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    const target = secondsAt(event.clientX);
    setPreview(null);
    props.onSeek(target);
  }

  function onKeyDown(event: KeyboardEvent) {
    if (props.duration <= 0) return;
    let target: number | null = null;
    switch (event.key) {
      case 'ArrowLeft':
      case 'ArrowDown':
        target = props.position - SCRUB_STEP_SECONDS;
        break;
      case 'ArrowRight':
      case 'ArrowUp':
        target = props.position + SCRUB_STEP_SECONDS;
        break;
      case 'PageDown':
        target = props.position - SCRUB_STEP_SECONDS * 6;
        break;
      case 'PageUp':
        target = props.position + SCRUB_STEP_SECONDS * 6;
        break;
      case 'Home':
        target = 0;
        break;
      case 'End':
        target = props.duration;
        break;
      default:
        return;
    }
    event.preventDefault();
    event.stopPropagation();
    props.onSeek(Math.min(props.duration, Math.max(0, target)));
  }

  return (
    <div class={['scrub', { dragging: preview() !== null }]}>
      <div
        ref={(el) => (track = el)}
        class="scrub-track"
        role="slider"
        tabindex={props.duration > 0 ? '0' : '-1'}
        aria-label={props.label ?? 'Position in track'}
        aria-valuemin="0"
        aria-valuemax={String(Math.round(props.duration))}
        aria-valuenow={String(Math.round(shown()))}
        aria-valuetext={`${formatClock(shown())} of ${formatClock(props.duration)}`}
        aria-disabled={props.duration > 0 ? undefined : 'true'}
        style={{ '--progress': String(fraction()) }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={() => {
          dragging = false;
          setPreview(null);
        }}
        onKeyDown={onKeyDown}
      >
        <span class="scrub-rail" />
        <span class="scrub-fill" />
        <span class="scrub-thumb" />
      </div>
      <div class="scrub-times">
        <span>{formatClock(shown())}</span>
        <span aria-label={`${formatClock(remaining())} remaining`}>−{formatClock(remaining())}</span>
      </div>
    </div>
  );
}
