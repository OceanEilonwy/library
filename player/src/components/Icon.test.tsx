import { render } from '@solidjs/testing-library';
import { Icon } from './Icon';

test('icons render as SVG and can be reused', () => {
  const { container } = render(() => (
    <div>
      <Icon name="heart" />
      <Icon name="heart" />
      <Icon name="back10" />
    </div>
  ));
  const svgs = container.querySelectorAll('svg');
  expect(svgs).toHaveLength(3);
  const svgNs = 'http://www.w3.org/2000/svg';
  expect(svgs[0]?.querySelector('path')?.namespaceURI).toBe(svgNs);
  expect(svgs[1]?.querySelector('path')?.namespaceURI).toBe(svgNs);
  expect(svgs[2]?.querySelector('text')?.namespaceURI).toBe(svgNs);
  expect(svgs[0]?.getAttribute('aria-hidden')).toBe('true');
});
