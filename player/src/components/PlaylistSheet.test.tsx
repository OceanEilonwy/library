import { flush } from 'solid-js';
import { render, screen } from '@solidjs/testing-library';
import userEvent from '@testing-library/user-event';
import { ServicesContext } from '../services';
import { jsonResponse, makeServices } from '../test/services';
import { mediaFile, playlistSummary } from '../test/fixtures';
import { PlaylistSheet } from './PlaylistSheet';

function setup() {
  const calls: { url: string; method: string; body: unknown }[] = [];
  const services = makeServices((url, init) => {
    const method = init?.method ?? 'GET';
    calls.push({ url, method, body: init?.body ? JSON.parse(String(init.body)) : undefined });
    if (url === '/api/playlists?mine=1')
      return jsonResponse(200, [playlistSummary(4, { name: 'Sunday Jazz', mine: true })]);
    if (url === '/api/playlists' && method === 'POST')
      return jsonResponse(201, { ...playlistSummary(9, { name: 'Road trip', mine: true }), entries: [] });
    if (url.endsWith('/entries')) return jsonResponse(200, { ...playlistSummary(4), entries: [] });
    return jsonResponse(404);
  });
  services.ui.openPlaylistSheet([mediaFile(7, { title: 'Teardrop' })]);
  flush();
  render(() => (
    <ServicesContext value={services}>
      <PlaylistSheet />
    </ServicesContext>
  ));
  return { services, calls };
}

test('adds to an existing playlist', async () => {
  const { services, calls } = setup();
  expect(await screen.findByRole('dialog', { name: 'Add “Teardrop” to a playlist' })).toBeInTheDocument();
  await userEvent.click(await screen.findByText('Sunday Jazz'));
  await vi.waitFor(() => expect(services.ui.playlistSheet()).toBeNull());
  expect(calls).toContainEqual({ url: '/api/playlists/4/entries', method: 'POST', body: { mediaIds: [7] } });
  expect(services.ui.toasts()[0]?.message).toBe('Added “Teardrop” to Sunday Jazz.');
});

test('creates a playlist and adds to it', async () => {
  const { services, calls } = setup();
  await userEvent.type(await screen.findByLabelText('New playlist name'), 'Road trip');
  await userEvent.click(screen.getByRole('button', { name: 'Create' }));
  await vi.waitFor(() => expect(services.ui.playlistSheet()).toBeNull());
  expect(calls).toContainEqual({ url: '/api/playlists', method: 'POST', body: { name: 'Road trip' } });
  expect(calls).toContainEqual({ url: '/api/playlists/9/entries', method: 'POST', body: { mediaIds: [7] } });
});

test('asks for a name', async () => {
  setup();
  await userEvent.click(await screen.findByRole('button', { name: 'Create' }));
  expect(await screen.findByRole('alert')).toHaveTextContent('Give the playlist a name.');
});
