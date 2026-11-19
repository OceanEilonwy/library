// A small set of line icons drawn for the Player (24 × 24, 1.75 stroke).
// Stored as SVG markup so each use parses fresh inside its own <svg>.

const paths = {
  play: '<path d="M8 5.5v13l10.5-6.5z" fill="currentColor" stroke="none" />',
  pause:
    '<rect x="6.5" y="5" width="3.6" height="14" rx="1" fill="currentColor" stroke="none" /> <rect x="13.9" y="5" width="3.6" height="14" rx="1" fill="currentColor" stroke="none" />',
  next: '<path d="M6 6v12l8.5-6z" fill="currentColor" stroke="none" /> <path d="M17.5 6v12" />',
  previous: '<path d="M18 6v12L9.5 12z" fill="currentColor" stroke="none" /> <path d="M6.5 6v12" />',
  back10:
    '<path d="M4.5 12a7.5 7.5 0 1 0 2.2-5.3" /> <path d="M4.5 4v3.5H8" /> <text x="12.2" y="15.2" font-size="7" text-anchor="middle" fill="currentColor" stroke="none" font-family="inherit" font-weight="600">10</text>',
  forward10:
    '<path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3" /> <path d="M19.5 4v3.5H16" /> <text x="11.8" y="15.2" font-size="7" text-anchor="middle" fill="currentColor" stroke="none" font-family="inherit" font-weight="600">10</text>',
  shuffle:
    '<path d="M4 7h3.5c4.5 0 4.5 10 9 10H20" /> <path d="M4 17h3.5c1.6 0 2.6-1.3 3.4-3M13.1 10c.8-1.7 1.8-3 3.4-3H20" /> <path d="M17.5 4.5 20 7l-2.5 2.5M17.5 14.5 20 17l-2.5 2.5" />',
  repeat:
    '<path d="M5 11V9.5A2.5 2.5 0 0 1 7.5 7H19" /> <path d="M16.5 4.5 19 7l-2.5 2.5" /> <path d="M19 13v1.5a2.5 2.5 0 0 1-2.5 2.5H5" /> <path d="M7.5 19.5 5 17l2.5-2.5" />',
  repeatOne:
    '<path d="M5 11V9.5A2.5 2.5 0 0 1 7.5 7H19" /> <path d="M16.5 4.5 19 7l-2.5 2.5" /> <path d="M19 13v1.5a2.5 2.5 0 0 1-2.5 2.5H5" /> <path d="M7.5 19.5 5 17l2.5-2.5" /> <path d="M11.2 10.6 12.4 10v4.2" stroke-width="1.5" />',
  heart: '<path d="M12 19.5s-7.5-4.4-7.5-10A4.2 4.2 0 0 1 12 7a4.2 4.2 0 0 1 7.5 2.5c0 5.6-7.5 10-7.5 10z" />',
  heartFilled:
    '<path d="M12 19.5s-7.5-4.4-7.5-10A4.2 4.2 0 0 1 12 7a4.2 4.2 0 0 1 7.5 2.5c0 5.6-7.5 10-7.5 10z" fill="currentColor" />',
  more: '<circle cx="12" cy="5.5" r="1.4" fill="currentColor" stroke="none" /> <circle cx="12" cy="12" r="1.4" fill="currentColor" stroke="none" /> <circle cx="12" cy="18.5" r="1.4" fill="currentColor" stroke="none" />',
  download: '<path d="M12 4.5v10M7.5 10.5 12 15l4.5-4.5" /> <path d="M5 19.5h14" />',
  downloaded:
    '<circle cx="12" cy="12" r="8.5" fill="currentColor" stroke="none" /> <path d="M8.3 12.3 11 15l4.8-5.5" stroke="var(--on-accent, #fff)" stroke-width="2" />',
  check: '<path d="m5.5 12.5 4 4 9-9.5" />',
  close: '<path d="M6 6l12 12M18 6 6 18" />',
  home: '<path d="M4.5 10.5 12 4.5l7.5 6V19a1 1 0 0 1-1 1h-4v-5.5h-5V20h-4a1 1 0 0 1-1-1z" />',
  library: '<path d="M5 4.5v15M9 4.5v15" /> <path d="m13 5.2 3.6-1 4 14.8-3.6 1z" />',
  playlists:
    '<path d="M4 6.5h11M4 11h11M4 15.5h6" /> <circle cx="16.5" cy="17" r="2.5" /> <path d="M19 17V8.5l2 .8" />',
  downloads:
    '<path d="M12 4v9M8 9.5l4 4 4-4" /> <path d="M4.5 14.5V18a1.5 1.5 0 0 0 1.5 1.5h12a1.5 1.5 0 0 0 1.5-1.5v-3.5" />',
  settings:
    '<circle cx="12" cy="12" r="3" /> <path d="M12 3.5v2.2M12 18.3v2.2M3.5 12h2.2M18.3 12h2.2M6 6l1.6 1.6M16.4 16.4 18 18M6 18l1.6-1.6M16.4 7.6 18 6" />',
  history: '<path d="M4.5 12a7.5 7.5 0 1 0 2.2-5.3L4.5 9" /> <path d="M4.5 4.5V9H9" /> <path d="M12 8v4.3l3 1.8" />',
  queue: '<path d="M4 6h12M4 10.5h12M4 15h7" /> <path d="M15 14v6l5-3z" fill="currentColor" stroke="none" />',
  drag: '<path d="M5 9h14M5 15h14" />',
  search: '<circle cx="11" cy="11" r="6" /> <path d="m15.5 15.5 4 4" />',
  plus: '<path d="M12 5v14M5 12h14" />',
  trash: '<path d="M5 7h14M10 4.5h4M7 7l.8 12a1.5 1.5 0 0 0 1.5 1.4h5.4a1.5 1.5 0 0 0 1.5-1.4L17 7" />',
  chevronDown: '<path d="m6 9 6 6 6-6" />',
  chevronLeft: '<path d="m15 6-6 6 6 6" />',
  film: '<rect x="4" y="5" width="16" height="14" rx="1.5" /> <path d="M8 5v14M16 5v14M4 9.5h4M4 14.5h4M16 9.5h4M16 14.5h4" />',
  document:
    '<path d="M7 3.5h7l4 4v12a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1v-15a1 1 0 0 1 1-1z" /> <path d="M14 3.5V8h4M9 12h6M9 15.5h6" />',
  music: '<circle cx="8" cy="17" r="2.5" /> <circle cx="17" cy="15" r="2.5" /> <path d="M10.5 17V6.5l9-2V15" />',
  offline:
    '<path d="M4 4l16 16" /> <path d="M8.5 8.8A9 9 0 0 0 3.5 11M12 7.5a9 9 0 0 1 8.5 3.5M7 14a5.5 5.5 0 0 1 3.4-1.6M14.5 13a5.5 5.5 0 0 1 2.5 1.3" /> <circle cx="12" cy="18" r="1.2" fill="currentColor" stroke="none" />',
  edit: '<path d="m14.5 5.5 4 4L9 19H5v-4z" />',
  account: '<circle cx="12" cy="8.5" r="3.5" /> <path d="M5 19.5c1-3.5 3.8-5 7-5s6 1.5 7 5" />',
  install: '<rect x="6.5" y="3.5" width="11" height="17" rx="2" /> <path d="M12 8v6M9.5 11.5 12 14l2.5-2.5" />',
  share:
    '<path d="M12 4v11M8 7.5l4-4 4 4" /> <path d="M6.5 11H6a1.5 1.5 0 0 0-1.5 1.5v6A1.5 1.5 0 0 0 6 20h12a1.5 1.5 0 0 0 1.5-1.5v-6A1.5 1.5 0 0 0 18 11h-.5" />',
  subtitles:
    '<rect x="3.5" y="5.5" width="17" height="13" rx="2" /> <path d="M7 13.5h4M13 13.5h4M7 10.5h2M11 10.5h6" />',
  album: '<circle cx="12" cy="12" r="8.5" /> <circle cx="12" cy="12" r="2" />',
} satisfies Record<string, string>;

export type IconName = keyof typeof paths;

/** A decorative icon. Give the surrounding button an accessible label. */
export function Icon(props: { name: IconName; size?: number; class?: string }) {
  return (
    <svg
      class={['icon', props.class]}
      width={props.size ?? 24}
      height={props.size ?? 24}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.75"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
      innerHTML={paths[props.name]}
    />
  );
}
