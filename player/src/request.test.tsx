import { createSignal, Show } from 'solid-js';
import { render, screen } from '@solidjs/testing-library';
import userEvent from '@testing-library/user-event';
import { createRequest } from './request';

test('loads, refetches on source change and reports errors', async () => {
  const fetcher = vi.fn(async (id: number) => {
    if (id === 3) throw new Error('nope');
    return `item ${id}`;
  });
  function View() {
    const [id, setId] = createSignal(1);
    const request = createRequest(id, fetcher);
    return (
      <div>
        <button onClick={() => setId((n) => n + 1)}>next</button>
        <Show when={!request.loading()} fallback={<p>Loading</p>}>
          <p>{request.error() ? 'failed' : request.data()}</p>
        </Show>
      </div>
    );
  }
  render(() => <View />);
  expect(await screen.findByText('item 1')).toBeInTheDocument();
  await userEvent.click(screen.getByText('next'));
  expect(await screen.findByText('item 2')).toBeInTheDocument();
  await userEvent.click(screen.getByText('next'));
  expect(await screen.findByText('failed')).toBeInTheDocument();
  expect(fetcher).toHaveBeenCalledTimes(3);
});
