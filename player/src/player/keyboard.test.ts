import { handleShortcut, type ShortcutKey } from './keyboard';

function setup() {
  const controls = { togglePlay: vi.fn(), seekBy: vi.fn(), next: vi.fn(), previous: vi.fn() };
  const press = (key: string, options: Partial<ShortcutKey> = {}) =>
    handleShortcut(
      { key, shiftKey: false, ctrlKey: false, metaKey: false, altKey: false, target: document.body, ...options },
      controls,
    );
  return { controls, press };
}

test('space toggles playback', () => {
  const { controls, press } = setup();
  expect(press(' ')).toBe(true);
  expect(controls.togglePlay).toHaveBeenCalledOnce();
});

test('arrows seek ten seconds', () => {
  const { controls, press } = setup();
  press('ArrowLeft');
  press('ArrowRight');
  expect(controls.seekBy.mock.calls).toEqual([[-10], [10]]);
});

test('shift+N and shift+P skip', () => {
  const { controls, press } = setup();
  press('N', { shiftKey: true });
  press('P', { shiftKey: true });
  expect(controls.next).toHaveBeenCalledOnce();
  expect(controls.previous).toHaveBeenCalledOnce();
});

test('plain N does nothing; modifier combos are left alone', () => {
  const { controls, press } = setup();
  expect(press('n')).toBe(false);
  expect(press(' ', { ctrlKey: true })).toBe(false);
  expect(controls.togglePlay).not.toHaveBeenCalled();
});

test('keys typed into fields, sliders and video are ignored', () => {
  const { controls, press } = setup();
  const input = document.createElement('input');
  const slider = document.createElement('div');
  slider.setAttribute('role', 'slider');
  const video = document.createElement('video');
  expect(press(' ', { target: input })).toBe(false);
  expect(press('ArrowLeft', { target: slider })).toBe(false);
  expect(press(' ', { target: video })).toBe(false);
  expect(controls.togglePlay).not.toHaveBeenCalled();
  expect(controls.seekBy).not.toHaveBeenCalled();
});

test('space on a button belongs to the button, but arrows still seek', () => {
  const { controls, press } = setup();
  const button = document.createElement('button');
  expect(press(' ', { target: button })).toBe(false);
  expect(press('ArrowRight', { target: button })).toBe(true);
  expect(controls.seekBy).toHaveBeenCalledWith(10);
});
