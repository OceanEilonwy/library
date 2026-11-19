import { useContext } from 'solid-js';
import type { MediaFile } from '../api';
import { ServicesContext } from '../services';
import { Icon } from './Icon';

/** The favourite toggle shown beside tracks and on Now Playing. */
export function HeartButton(props: { file: MediaFile; size?: number }) {
  const { favourites } = useContext(ServicesContext);
  const on = () => favourites.has(props.file.id);
  return (
    <button
      type="button"
      class={['icon-button', 'heart', { on: on() }]}
      aria-pressed={on() ? 'true' : 'false'}
      aria-label={on() ? `Remove “${props.file.title}” from favourites` : `Add “${props.file.title}” to favourites`}
      onClick={(event) => {
        event.stopPropagation();
        void favourites.toggle(props.file);
      }}
    >
      <Icon name={on() ? 'heartFilled' : 'heart'} size={props.size ?? 22} />
    </button>
  );
}
