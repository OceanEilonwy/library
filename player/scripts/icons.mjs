// Renders the app icons (PNG) from the SVG sources in icons/.
// Run with `npm run icons`; the PNGs are committed in public/icons.
import { readFileSync, writeFileSync, copyFileSync, mkdirSync } from 'node:fs';
import { Resvg } from '@resvg/resvg-js';

const here = new URL('..', import.meta.url);
const out = new URL('public/icons/', here);
mkdirSync(out, { recursive: true });

const render = (source, size, name) => {
  const svg = readFileSync(new URL(`icons/${source}`, here), 'utf8');
  const png = new Resvg(svg, { fitTo: { mode: 'width', value: size } }).render().asPng();
  writeFileSync(new URL(name, out), png);
  console.log(`icons/${source} → public/icons/${name} (${size}px)`);
};

render('icon.svg', 192, 'icon-192.png');
render('icon.svg', 512, 'icon-512.png');
render('icon-maskable.svg', 512, 'icon-maskable-512.png');
render('icon-maskable.svg', 180, 'apple-touch-icon.png');
copyFileSync(new URL('icons/icon.svg', here), new URL('icon.svg', out));
