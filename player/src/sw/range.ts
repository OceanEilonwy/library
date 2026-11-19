// Answering HTTP Range requests from a cached blob, for the service worker.
//
// Media elements ask for byte ranges ("bytes=0-", "bytes=1048576-") so they
// can seek. A downloaded file lives in the Cache API as one full response;
// these helpers slice it so the answer looks just like the server's.

/** An inclusive byte range within a file. */
export type ByteRange = { start: number; end: number };

/** The parsed outcome of a Range header against a file of a known size. */
export type RangeRequest =
  | { type: 'full' } // no (usable) Range header: send the whole file
  | { type: 'partial'; range: ByteRange }
  | { type: 'unsatisfiable' };

/**
 * Parses a single-range `Range` header (`bytes=a-b`, `bytes=a-`, `bytes=-n`).
 * Multi-range and malformed headers are ignored (served in full), as servers may.
 */
export function parseRange(header: string | null, size: number): RangeRequest {
  if (!header) return { type: 'full' };
  const match = /^\s*bytes\s*=\s*(\d*)\s*-\s*(\d*)\s*$/i.exec(header);
  if (!match) return { type: 'full' };
  const [, startText = '', endText = ''] = match;
  if (startText === '' && endText === '') return { type: 'full' };

  if (startText === '') {
    // Suffix range: the last n bytes.
    const length = Number(endText);
    if (length === 0 || size === 0) return { type: 'unsatisfiable' };
    return { type: 'partial', range: { start: Math.max(0, size - length), end: size - 1 } };
  }

  const start = Number(startText);
  const end = endText === '' ? size - 1 : Math.min(Number(endText), size - 1);
  if (start >= size || end < start) return { type: 'unsatisfiable' };
  return { type: 'partial', range: { start, end } };
}

/**
 * Builds the response for `rangeHeader` from a cached `blob`: 200 with the
 * whole file, 206 with the slice, or 416 when the range is outside the file.
 */
export function rangeResponse(blob: Blob, rangeHeader: string | null, contentType: string): Response {
  const size = blob.size;
  const parsed = parseRange(rangeHeader, size);
  const common = { 'Accept-Ranges': 'bytes', 'Content-Type': contentType };
  switch (parsed.type) {
    case 'full':
      return new Response(blob, {
        status: 200,
        headers: { ...common, 'Content-Length': String(size) },
      });
    case 'partial': {
      const { start, end } = parsed.range;
      const slice = blob.slice(start, end + 1, contentType);
      return new Response(slice, {
        status: 206,
        headers: {
          ...common,
          'Content-Length': String(end - start + 1),
          'Content-Range': `bytes ${start}-${end}/${size}`,
        },
      });
    }
    case 'unsatisfiable':
      return new Response(null, {
        status: 416,
        headers: { ...common, 'Content-Range': `bytes */${size}` },
      });
  }
}

/** True for `/api/media/{id}/stream` paths, the only URLs served from downloads. */
export function isStreamPath(pathname: string): boolean {
  return /^\/api\/media\/\d+\/stream$/.test(pathname);
}
