import { flush } from 'solid-js';
import { render, screen } from '@solidjs/testing-library';
import userEvent from '@testing-library/user-event';
import { ServicesContext } from '../services';
import { jsonResponse, makeServices } from '../test/services';
import { mediaFile } from '../test/fixtures';
import { testRouter } from '../test/router';
import { TrackRow } from './TrackRow';

function setup(file = mediaFile(5, { title: 'Lazy Bird' }), unavailable = false) {
  const services = makeServices((url, init) => {
    if (url === '/api/favourites/5' && init?.method === 'PUT') return jsonResponse(204);
    return jsonResponse(500, { error: 'Something broke.' });
  });
  const onPlay = vi.fn();
  const Router = testRouter(() => (
    <ul>
      <TrackRow file={file} onPlay={onPlay} unavailable={unavailable} />
    </ul>
  ));
  render(() => (
    <ServicesContext value={services}>
      <Router />
    </ServicesContext>
  ));
  return { services, onPlay };
}

test('tapping the row plays it', async () => {
  const { onPlay } = setup();
  await userEvent.click((await screen.findByText('Lazy Bird')).closest('button') as HTMLElement);
  expect(onPlay).toHaveBeenCalledOnce();
});

test('an unavailable row explains instead of playing', async () => {
  const { onPlay, services } = setup(undefined, true);
  await userEvent.click((await screen.findByText('Lazy Bird')).closest('button') as HTMLElement);
  expect(onPlay).not.toHaveBeenCalled();
  expect(services.ui.toasts()[0]?.message).toMatch(/offline/);
});

test('the menu offers play next and add to queue', async () => {
  const { services } = setup();
  services.player.playFiles([mediaFile(1), mediaFile(2)]);
  flush();
  await userEvent.click(await screen.findByRole('button', { name: 'More options for “Lazy Bird”' }));
  const menu = await screen.findByRole('menu');
  expect(menu).toBeInTheDocument();
  await userEvent.click(screen.getByRole('menuitem', { name: 'Play next' }));
  expect(services.player.state().queue.entries.map((entry) => entry.track.id)).toEqual([1, 5, 2]);

  await userEvent.click(screen.getByRole('button', { name: 'More options for “Lazy Bird”' }));
  await userEvent.click(await screen.findByRole('menuitem', { name: 'Add to queue' }));
  expect(services.player.state().queue.entries.map((entry) => entry.track.id)).toEqual([1, 5, 2, 5]);
});

test('menu keyboard: arrows move, Escape closes and returns focus', async () => {
  setup();
  const button = await screen.findByRole('button', { name: 'More options for “Lazy Bird”' });
  button.focus();
  await userEvent.keyboard('{Enter}');
  await vi.waitFor(() => expect(screen.getByRole('menuitem', { name: 'Play next' })).toHaveFocus());
  await userEvent.keyboard('{ArrowDown}');
  expect(screen.getByRole('menuitem', { name: 'Add to queue' })).toHaveFocus();
  await userEvent.keyboard('{ArrowUp}{ArrowUp}');
  expect(screen.getByRole('menuitem', { name: /favourites/ })).toHaveFocus();
  await userEvent.keyboard('{Escape}');
  await vi.waitFor(() => expect(screen.queryByRole('menu')).not.toBeInTheDocument());
  expect(button).toHaveFocus();
});

test('the menu opens the playlist sheet', async () => {
  const { services } = setup();
  await userEvent.click(await screen.findByRole('button', { name: 'More options for “Lazy Bird”' }));
  await userEvent.click(await screen.findByRole('menuitem', { name: 'Add to playlist…' }));
  expect(services.ui.playlistSheet()?.map((file) => file.id)).toEqual([5]);
});

test('the heart favourites the track', async () => {
  const { services } = setup();
  await userEvent.click(await screen.findByRole('button', { name: 'Add “Lazy Bird” to favourites' }));
  expect(await screen.findByRole('button', { name: 'Remove “Lazy Bird” from favourites' })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  expect(services.favourites.has(5)).toBe(true);
});
