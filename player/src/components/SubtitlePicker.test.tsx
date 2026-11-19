import { createSignal } from 'solid-js';
import { render, screen } from '@solidjs/testing-library';
import userEvent from '@testing-library/user-event';
import type { Subtitle } from '../api';
import {
  SubtitlePicker,
  initialChoice,
  rememberLanguage,
  rememberedLanguage,
  type SubtitleChoice,
} from './SubtitlePicker';

const subtitles: Subtitle[] = [
  { id: 1, language: 'en', label: 'English', url: '/api/subtitles/1' },
  { id: 2, language: 'en-GB', label: 'English (SDH)', url: '/api/subtitles/2' },
  { id: 3, language: 'fr', label: 'Français', url: '/api/subtitles/3' },
];

describe('initial choice', () => {
  test('off when nothing is remembered or it was turned off', () => {
    expect(initialChoice(subtitles, null)).toBe('off');
    expect(initialChoice(subtitles, 'off')).toBe('off');
  });
  test('exact language first, then the same base language', () => {
    expect(initialChoice(subtitles, 'en-GB')).toBe(2);
    expect(initialChoice(subtitles, 'FR')).toBe(3);
    expect(initialChoice(subtitles, 'fr-CA')).toBe(3);
  });
  test('off when the film lacks the language', () => {
    expect(initialChoice(subtitles, 'de')).toBe('off');
  });
});

test('remembers the chosen language in storage', () => {
  const store = new Map<string, string>();
  const storage = {
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => void store.set(key, value),
  };
  expect(rememberedLanguage(storage)).toBeNull();
  rememberLanguage(storage, 'fr');
  expect(rememberedLanguage(storage)).toBe('fr');
  expect(rememberedLanguage(null)).toBeNull();
  const broken = {
    getItem: () => {
      throw new Error('blocked');
    },
    setItem: () => {
      throw new Error('blocked');
    },
  };
  expect(() => rememberLanguage(broken, 'en')).not.toThrow();
  expect(rememberedLanguage(broken)).toBeNull();
});

test('offers Off and each language, and reports the choice', async () => {
  const onChange = vi.fn();
  function Harness() {
    const [choice, setChoice] = createSignal<SubtitleChoice>('off');
    return (
      <SubtitlePicker
        subtitles={subtitles}
        choice={choice()}
        onChange={(next) => {
          onChange(next);
          setChoice(next);
        }}
      />
    );
  }
  render(() => <Harness />);
  const group = screen.getByRole('group', { name: 'Subtitles' });
  expect(group).toBeInTheDocument();
  expect(screen.getByRole('button', { name: 'Off' })).toHaveAttribute('aria-pressed', 'true');
  await userEvent.click(screen.getByRole('button', { name: 'Français' }));
  expect(onChange).toHaveBeenCalledWith(3);
  expect(await screen.findByRole('button', { name: 'Français', pressed: true })).toHaveAttribute('lang', 'fr');
  expect(screen.getByRole('button', { name: 'Off' })).toHaveAttribute('aria-pressed', 'false');
  await userEvent.click(screen.getByRole('button', { name: 'Off' }));
  expect(onChange).toHaveBeenLastCalledWith('off');
});
