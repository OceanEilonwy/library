// @vitest-environment node
import { isStreamPath, parseRange, rangeResponse } from './range';

describe('parseRange', () => {
  test('no header means the whole file', () => {
    expect(parseRange(null, 100)).toEqual({ type: 'full' });
    expect(parseRange('', 100)).toEqual({ type: 'full' });
  });

  test('open-ended range', () => {
    expect(parseRange('bytes=10-', 100)).toEqual({ type: 'partial', range: { start: 10, end: 99 } });
  });

  test('closed range, clamped to the file', () => {
    expect(parseRange('bytes=0-9', 100)).toEqual({ type: 'partial', range: { start: 0, end: 9 } });
    expect(parseRange('bytes=90-500', 100)).toEqual({ type: 'partial', range: { start: 90, end: 99 } });
  });

  test('suffix range', () => {
    expect(parseRange('bytes=-10', 100)).toEqual({ type: 'partial', range: { start: 90, end: 99 } });
    expect(parseRange('bytes=-500', 100)).toEqual({ type: 'partial', range: { start: 0, end: 99 } });
  });

  test('ranges outside the file are unsatisfiable', () => {
    expect(parseRange('bytes=100-', 100)).toEqual({ type: 'unsatisfiable' });
    expect(parseRange('bytes=50-10', 100)).toEqual({ type: 'unsatisfiable' });
    expect(parseRange('bytes=-0', 100)).toEqual({ type: 'unsatisfiable' });
    expect(parseRange('bytes=0-', 0)).toEqual({ type: 'unsatisfiable' });
  });

  test('malformed and multi-range headers are served in full', () => {
    expect(parseRange('bytes=0-1,5-9', 100)).toEqual({ type: 'full' });
    expect(parseRange('items=0-1', 100)).toEqual({ type: 'full' });
    expect(parseRange('bytes=-', 100)).toEqual({ type: 'full' });
  });

  test('tolerates case and spaces', () => {
    expect(parseRange(' Bytes = 5 - 6 ', 100)).toEqual({ type: 'partial', range: { start: 5, end: 6 } });
  });
});

describe('rangeResponse', () => {
  const blob = new Blob(['0123456789'], { type: 'audio/flac' });

  test('206 with the requested slice', async () => {
    const response = rangeResponse(blob, 'bytes=2-5', 'audio/flac');
    expect(response.status).toBe(206);
    expect(response.headers.get('Content-Range')).toBe('bytes 2-5/10');
    expect(response.headers.get('Content-Length')).toBe('4');
    expect(response.headers.get('Accept-Ranges')).toBe('bytes');
    expect(response.headers.get('Content-Type')).toBe('audio/flac');
    expect(await response.text()).toBe('2345');
  });

  test('200 with everything when there is no range', async () => {
    const response = rangeResponse(blob, null, 'audio/flac');
    expect(response.status).toBe(200);
    expect(response.headers.get('Content-Length')).toBe('10');
    expect(await response.text()).toBe('0123456789');
  });

  test('416 when the range is outside the file', () => {
    const response = rangeResponse(blob, 'bytes=10-', 'audio/flac');
    expect(response.status).toBe(416);
    expect(response.headers.get('Content-Range')).toBe('bytes */10');
  });

  test('suffix slice', async () => {
    expect(await rangeResponse(blob, 'bytes=-3', 'audio/flac').text()).toBe('789');
  });
});

test('recognises stream paths only', () => {
  expect(isStreamPath('/api/media/12/stream')).toBe(true);
  expect(isStreamPath('/api/media/12')).toBe(false);
  expect(isStreamPath('/api/media/x/stream')).toBe(false);
});
