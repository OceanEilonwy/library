import { render, screen } from '@solidjs/testing-library';
import userEvent from '@testing-library/user-event';
import { ServicesContext } from '../services';
import { jsonResponse, makeServices } from '../test/services';
import { mediaFile, playlistSummary } from '../test/fixtures';
import { testRouter } from '../test/router';
import type { Playlist as PlaylistData } from '../api';
import Playlist from './Playlist';

function setup(mine: boolean) {
  let playlist: PlaylistData = {
    ...playlistSummary(4, { name: 'Sunday Jazz', mine, owner: mine ? 'alice' : 'bob' }),
    entries: [
      mediaFile(1, { title: 'So What' }),
      mediaFile(2, { title: 'Naima' }),
      mediaFile(3, { title: 'Footprints' }),
    ],
  };
  const puts: number[][] = [];
  const services = makeServices((url, init) => {
    if (url === '/api/playlists/4' && (init?.method ?? 'GET') === 'GET') return jsonResponse(200, playlist);
    if (url === '/api/playlists/4/entries' && init?.method === 'PUT') {
      const ids = (JSON.parse(String(init.body)) as { mediaIds: number[] }).mediaIds;
      puts.push(ids);
      playlist = { ...playlist, entries: ids.map((id) => playlist.entries.find((f) => f.id === id)!) };
      return jsonResponse(200, playlist);
    }
    if (url === '/api/playlists/4' && init?.method === 'PATCH') {
      playlist = { ...playlist, name: (JSON.parse(String(init.body)) as { name: string }).name };
      return jsonResponse(200, playlist);
    }
    return jsonResponse(404);
  });
  const Router = testRouter(Playlist, '/playlist/:id', '/playlist/4');
  render(() => (
    <ServicesContext value={services}>
      <Router />
    </ServicesContext>
  ));
  return { services, puts };
}

test('owners can reorder from the keyboard', async () => {
  const { puts } = setup(true);
  const handle = await screen.findByRole('button', { name: /Move “So What”/ });
  handle.focus();
  await userEvent.keyboard('{ArrowDown}');
  await vi.waitFor(() => expect(puts).toEqual([[2, 1, 3]]));
});

test('owners can remove an entry', async () => {
  const { puts } = setup(true);
  await userEvent.click(await screen.findByRole('button', { name: 'More options for “Naima”' }));
  await userEvent.click(await screen.findByRole('menuitem', { name: 'Remove from this playlist' }));
  await vi.waitFor(() => expect(puts).toEqual([[1, 3]]));
  await vi.waitFor(() => expect(screen.queryByText('Naima')).not.toBeInTheDocument());
});

test('owners can rename', async () => {
  setup(true);
  await userEvent.click(await screen.findByRole('button', { name: 'Rename' }));
  const input = await screen.findByLabelText('Playlist name');
  await userEvent.clear(input);
  await userEvent.type(input, 'Sunday Bop');
  await userEvent.click(screen.getByRole('button', { name: 'Save' }));
  expect(await screen.findByRole('heading', { name: 'Sunday Bop' })).toBeInTheDocument();
});

test("other people's playlists can be played but not edited", async () => {
  const { services } = setup(false);
  expect(await screen.findByText('By bob')).toBeInTheDocument();
  expect(screen.queryByRole('button', { name: /Move/ })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Rename' })).not.toBeInTheDocument();
  await userEvent.click(screen.getByRole('button', { name: 'Play' }));
  expect(services.player.state().queue.entries.map((entry) => entry.track.id)).toEqual([1, 2, 3]);
});
