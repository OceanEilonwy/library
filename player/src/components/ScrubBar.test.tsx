import { createSignal, flush } from 'solid-js';
import { fireEvent, render, screen } from '@solidjs/testing-library';
import userEvent from '@testing-library/user-event';
import { ScrubBar } from './ScrubBar';

function setup(position = 60, duration = 200) {
  const onSeek = vi.fn();
  const [pos, setPos] = createSignal(position);
  render(() => (
    <ScrubBar
      position={pos()}
      duration={duration}
      onSeek={(seconds) => {
        onSeek(seconds);
        setPos(seconds);
      }}
    />
  ));
  const slider = screen.getByRole('slider', { name: 'Position in track' });
  return { slider, onSeek };
}

test('describes itself to assistive technology', () => {
  const { slider } = setup();
  expect(slider).toHaveAttribute('aria-valuemin', '0');
  expect(slider).toHaveAttribute('aria-valuemax', '200');
  expect(slider).toHaveAttribute('aria-valuenow', '60');
  expect(slider).toHaveAttribute('aria-valuetext', '1:00 of 3:20');
  expect(screen.getByText('1:00')).toBeInTheDocument();
  expect(screen.getByText('−2:20')).toBeInTheDocument();
});

test('arrow keys move five seconds; page keys thirty; home and end jump', async () => {
  const { slider, onSeek } = setup();
  slider.focus();
  await userEvent.keyboard('{ArrowRight}');
  await userEvent.keyboard('{ArrowLeft}');
  await userEvent.keyboard('{PageUp}');
  await userEvent.keyboard('{PageDown}');
  await userEvent.keyboard('{End}');
  await userEvent.keyboard('{Home}');
  expect(onSeek.mock.calls.map(([seconds]) => seconds)).toEqual([65, 60, 90, 60, 200, 0]);
});

test('keys never seek outside the track', async () => {
  const { slider, onSeek } = setup(2, 200);
  slider.focus();
  await userEvent.keyboard('{ArrowLeft}');
  expect(onSeek).toHaveBeenCalledWith(0);
});

test('dragging previews and seeks once on release', () => {
  const { slider, onSeek } = setup(0, 100);
  slider.getBoundingClientRect = () => ({
    left: 0,
    width: 200,
    top: 0,
    right: 200,
    bottom: 20,
    height: 20,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  });
  fireEvent.pointerDown(slider, { clientX: 50, button: 0, pointerId: 1 });

  fireEvent.pointerMove(slider, { clientX: 100, pointerId: 1 });
  flush();
  expect(onSeek).not.toHaveBeenCalled();
  expect(slider).toHaveAttribute('aria-valuenow', '50');
  fireEvent.pointerUp(slider, { clientX: 150, pointerId: 1 });
  expect(onSeek).toHaveBeenCalledOnce();
  expect(onSeek).toHaveBeenCalledWith(75);
});

test('is inert while the length is unknown', async () => {
  const { slider, onSeek } = setup(0, 0);
  expect(slider).toHaveAttribute('aria-disabled', 'true');
  slider.focus();
  await userEvent.keyboard('{ArrowRight}');
  expect(onSeek).not.toHaveBeenCalled();
});
