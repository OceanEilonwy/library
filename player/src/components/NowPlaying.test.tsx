import { flush } from 'solid-js';
import { render, screen } from '@solidjs/testing-library';
import userEvent from '@testing-library/user-event';
import { ServicesContext } from '../services';
import { makeServices } from '../test/services';
import { mediaFile } from '../test/fixtures';
import { testRouter } from '../test/router';
import { NowPlaying } from './NowPlaying';

function setup() {
  const services = makeServices();
  services.player.playFiles([mediaFile(1, { title: 'So What' }), mediaFile(2, { title: 'All Blues' }), mediaFile(3)]);
  services.ui.setNowPlayingOpen(true);
  flush();
  const Router = testRouter(() => <NowPlaying />);
  render(() => (
    <ServicesContext value={services}>
      <Router />
    </ServicesContext>
  ));
  return services;
}

test('shows the track and every transport control', async () => {
  setup();
  expect(await screen.findByRole('dialog', { name: 'Now playing' })).toBeInTheDocument();
  expect(screen.getByRole('heading', { name: 'So What' })).toBeInTheDocument();
  for (const name of [
    'Shuffle',
    'Back 10 seconds',
    'Previous',
    'Pause',
    'Next',
    'Forward 10 seconds',
    'Repeat is off',
  ]) {
    expect(screen.getByRole('button', { name })).toBeInTheDocument();
  }
  expect(screen.getByRole('slider', { name: 'Position in track' })).toBeInTheDocument();
});

test('back and forward jump ten seconds', async () => {
  const services = setup();
  services.audio.currentTime = 30;
  await userEvent.click(await screen.findByRole('button', { name: 'Forward 10 seconds' }));
  expect(services.audio.currentTime).toBe(40);
  await userEvent.click(screen.getByRole('button', { name: 'Back 10 seconds' }));
  expect(services.audio.currentTime).toBe(30);
});

test('shuffle and repeat toggle and say what they are doing', async () => {
  const services = setup();
  await userEvent.click(await screen.findByRole('button', { name: 'Shuffle' }));
  expect(await screen.findByRole('button', { name: 'Shuffle', pressed: true })).toBeInTheDocument();
  expect(services.player.state().queue.shuffled).toBe(true);
  await userEvent.click(screen.getByRole('button', { name: 'Repeat is off' }));
  await userEvent.click(await screen.findByRole('button', { name: 'Repeating all' }));
  expect(await screen.findByRole('button', { name: 'Repeating this track' })).toBeInTheDocument();
});

test('the Up next tab lists what follows and can jump to it', async () => {
  const services = setup();
  await userEvent.click(await screen.findByRole('tab', { name: 'Up next' }));
  expect(await screen.findByText('All Blues')).toBeInTheDocument();
  await userEvent.click(screen.getByText('All Blues'));
  expect(services.audio.src).toBe('/api/media/2/stream');
});

test('the queue reorders from the keyboard on the drag handle', async () => {
  const services = setup();
  await userEvent.click(await screen.findByRole('tab', { name: 'Up next' }));
  const handle = await screen.findByRole('button', { name: /Move “All Blues”/ });
  handle.focus();
  await userEvent.keyboard('{ArrowDown}');
  expect(services.player.state().queue.entries.map((entry) => entry.track.id)).toEqual([1, 3, 2]);
});

test('Escape closes it', async () => {
  const services = setup();
  await screen.findByRole('dialog');
  await userEvent.keyboard('{Escape}');
  expect(services.ui.nowPlayingOpen()).toBe(false);
});
