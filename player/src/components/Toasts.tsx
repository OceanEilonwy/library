import { For, useContext } from 'solid-js';
import { ServicesContext } from '../services';
import { Icon } from './Icon';

/** Brief confirmations ("Added to Sunday Jazz") announced politely to screen readers. */
export function Toasts() {
  const { ui } = useContext(ServicesContext);
  return (
    <div class="toasts" role="status" aria-live="polite">
      <For each={ui.toasts()}>
        {(toast) => (
          <div class="toast">
            <span>{toast.message}</span>
            <button type="button" class="icon-button" aria-label="Dismiss" onClick={() => ui.dismissToast(toast.id)}>
              <Icon name="close" size={18} />
            </button>
          </div>
        )}
      </For>
    </div>
  );
}
