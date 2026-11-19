// SPDX-License-Identifier: AGPL-3.0-or-later
import { render, screen } from '@solidjs/testing-library';
import { ServicesContext } from '../services';
import { jsonResponse, makeServices, type Responder } from '../test/services';
import { testRouter } from '../test/router';
import Settings from './Settings';

function setup(respond: Responder) {
  const services = makeServices(respond);
  const Router = testRouter(Settings, '/settings', '/settings');
  render(() => (
    <ServicesContext value={services}>
      <Router />
    </ServicesContext>
  ));
  return services;
}

describe('about', () => {
  test('links to the source code the server names', async () => {
    setup((url) =>
      url === '/api/info'
        ? jsonResponse(200, {
            name: 'The Library',
            version: '0.2.0',
            sourceUrl: 'https://example.org/library/source',
          })
        : jsonResponse(404),
    );
    const link = await screen.findByRole('link', { name: 'Source code' });
    expect(link).toHaveAttribute('href', 'https://example.org/library/source');
    expect(screen.getByText('The Library, version 0.2.0')).toBeInTheDocument();
    expect(screen.getByText(/GNU Affero General Public License/)).toBeInTheDocument();
  });

  test('still shows the licence when the server cannot be asked', async () => {
    const services = setup(() => jsonResponse(404));
    await vi.waitFor(() => expect(services.fetch).toHaveBeenCalledWith('/api/info', expect.anything()));
    expect(screen.getByRole('heading', { name: 'About' })).toBeInTheDocument();
    expect(screen.getByText(/GNU Affero General Public License/)).toBeInTheDocument();
    expect(screen.queryByRole('link', { name: 'Source code' })).not.toBeInTheDocument();
  });
});
