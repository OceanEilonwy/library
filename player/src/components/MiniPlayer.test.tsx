import { flush } from 'solid-js';
import { render, screen } from '@solidjs/testing-library';
import userEvent from '@testing-library/user-event';
import { ServicesContext } from '../services';
import { makeServices } from '../test/services';
import { mediaFile } from '../test/fixtures';
import { MiniPlayer } from './MiniPlayer';

function setup() {
  const services = makeServices();
  render(() => (
    <ServicesContext value={services}>
      <MiniPlayer />
    </ServicesContext>
  ));
  return services;
}

test('stays hidden until something is loaded', () => {
  setup();
  expect(screen.queryByRole('region', { name: 'Player' })).not.toBeInTheDocument();
});

test('shows the track and drives the player', async () => {
  const services = setup();
  services.player.playFiles([mediaFile(1, { title: 'Moment’s Notice' }), mediaFile(2, { title: 'Locomotion' })]);
  flush();
  expect(await screen.findByText('Moment’s Notice')).toBeInTheDocument();

  await userEvent.click(screen.getByRole('button', { name: 'Pause' }));
  expect(services.audio.paused).toBe(true);
  expect(await screen.findByRole('button', { name: 'Play' })).toBeInTheDocument();

  await userEvent.click(screen.getByRole('button', { name: 'Play' }));
  expect(services.audio.paused).toBe(false);

  await userEvent.click(screen.getByRole('button', { name: 'Next' }));
  expect(services.audio.src).toBe('/api/media/2/stream');
  expect(await screen.findByText('Locomotion')).toBeInTheDocument();

  services.audio.currentTime = 1;
  await userEvent.click(screen.getByRole('button', { name: 'Previous' }));
  expect(services.audio.src).toBe('/api/media/1/stream');
});

test('tapping the track opens Now Playing', async () => {
  const services = setup();
  services.player.playFiles([mediaFile(1)]);
  flush();
  await userEvent.click(await screen.findByRole('button', { name: /Open Now Playing/ }));
  expect(services.ui.nowPlayingOpen()).toBe(true);
});

test('shows a playback error in place of the artist', async () => {
  const services = setup();
  services.player.playFiles([mediaFile(1)]);
  services.audio.emit('error');
  expect(await screen.findByText(/Couldn't play/)).toBeInTheDocument();
});
