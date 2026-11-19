// Choose subtitles for a film: Off, or one of the languages it has.

import { For } from 'solid-js';
import type { Subtitle } from '../api';

/** "off", or the id of the chosen subtitle track. */
export type SubtitleChoice = 'off' | number;

const STORAGE_KEY = 'player.subtitleLanguage';

/** The remembered language ("off" when subtitles were turned off), if any. */
export function rememberedLanguage(storage: Pick<Storage, 'getItem'> | null): string | null {
  try {
    return storage?.getItem(STORAGE_KEY) ?? null;
  } catch {
    return null;
  }
}

export function rememberLanguage(storage: Pick<Storage, 'setItem'> | null, language: string): void {
  try {
    storage?.setItem(STORAGE_KEY, language);
  } catch {
    // Not remembered; no harm done.
  }
}

/**
 * Which subtitle to start with: the remembered language if this film has
 * it (an exact match, else the same base language: "en" for "en-GB"), else off.
 */
export function initialChoice(subtitles: readonly Subtitle[], remembered: string | null): SubtitleChoice {
  if (!remembered || remembered === 'off') return 'off';
  const exact = subtitles.find((subtitle) => subtitle.language.toLowerCase() === remembered.toLowerCase());
  if (exact) return exact.id;
  const base = remembered.split('-')[0]?.toLowerCase();
  const loose = subtitles.find((subtitle) => subtitle.language.split('-')[0]?.toLowerCase() === base);
  return loose ? loose.id : 'off';
}

export function SubtitlePicker(props: {
  subtitles: readonly Subtitle[];
  choice: SubtitleChoice;
  onChange: (choice: SubtitleChoice) => void;
}) {
  return (
    <fieldset class="subtitle-picker">
      <legend>Subtitles</legend>
      <div class="chips">
        <button
          type="button"
          class="chip"
          aria-pressed={props.choice === 'off' ? 'true' : 'false'}
          onClick={() => props.onChange('off')}
        >
          Off
        </button>
        <For each={props.subtitles}>
          {(subtitle) => (
            <button
              type="button"
              class="chip"
              lang={subtitle.language}
              aria-pressed={props.choice === subtitle.id ? 'true' : 'false'}
              onClick={() => props.onChange(subtitle.id)}
            >
              {subtitle.label}
            </button>
          )}
        </For>
      </div>
    </fieldset>
  );
}
