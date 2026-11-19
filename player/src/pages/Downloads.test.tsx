import { render, screen, within } from '@solidjs/testing-library';
import userEvent from '@testing-library/user-event';
import { ServicesContext } from '../services';
import { jsonResponse, makeServices } from '../test/services';
import { mediaFile } from '../test/fixtures';
import { testRouter } from '../test/router';
import { DOWNLOAD_CACHE } from '../downloads';
import Downloads from './Downloads';

async function setup() {
  const services = makeServices((url) => {
    if (url.endsWith('/stream'))
      return new Response(new Uint8Array(2_500_000), { headers: { 'Content-Type': 'audio/flac' } });
    return jsonResponse(200, {});
  });
  await services.downloads.store.download(mediaFile(1, { title: 'Blue Train' }));
  await services.downloads.store.download(mediaFile(2, { title: 'Locomotion' }));
  await services.downloads.store.download(
    mediaFile(9, { itemId: 7, itemTitle: 'A Love Supreme', title: 'Acknowledgement' }),
  );
  await services.history.record(mediaFile(1));
  const Router = testRouter(Downloads);
  render(() => (
    <ServicesContext value={services}>
      <Router />
    </ServicesContext>
  ));
  return services;
}

test('lists downloads grouped by item with sizes and storage use', async () => {
  await setup();
  expect(await screen.findAllByText('Blue Train')).toHaveLength(2); // the item, and its title track
  expect(screen.getByText('A Love Supreme')).toBeInTheDocument();
  expect(screen.getByText(/John Coltrane, 2 files, 5\.0 MB/)).toBeInTheDocument();
  expect(screen.getByText('7.5 MB')).toBeInTheDocument();
  expect(await screen.findByText(/The Player is using 5\.0 MB of this device/)).toBeInTheDocument();
  expect(screen.getByRole('meter', { name: 'Storage used' })).toBeInTheDocument();
});

test('removes a single download', async () => {
  const services = await setup();
  await userEvent.click(await screen.findByRole('button', { name: 'Remove download of Locomotion' }));
  await vi.waitFor(() => expect(screen.queryByText('Locomotion')).not.toBeInTheDocument());
  expect(services.caches.urls(DOWNLOAD_CACHE)).not.toContain('/api/media/2/stream');
});

test('removes a whole item', async () => {
  await setup();
  await userEvent.click(await screen.findByRole('button', { name: 'Remove all downloads of A Love Supreme' }));
  await vi.waitFor(() => expect(screen.queryByText('A Love Supreme')).not.toBeInTheDocument());
});

test('clearing everything takes two taps', async () => {
  const services = await setup();
  const clear = await screen.findByRole('button', { name: 'Clear all downloads' });
  await userEvent.click(clear);
  expect(services.downloads.records()).toHaveLength(3);
  await userEvent.click(await screen.findByRole('button', { name: 'Tap again to clear' }));
  expect(await screen.findByText('Nothing downloaded yet')).toBeInTheDocument();
  expect(services.caches.urls(DOWNLOAD_CACHE)).toEqual([]);
});

test('clears listen history', async () => {
  const services = await setup();
  const row = (await screen.findByText('Clear listen history')).closest('.danger-row') as HTMLElement;
  await userEvent.click(within(row).getByRole('button', { name: 'Clear history' }));
  await userEvent.click(within(row).getByRole('button', { name: 'Tap again to clear' }));
  await vi.waitFor(async () => expect(await services.history.store.list()).toHaveLength(0));
});
