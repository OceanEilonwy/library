// A small in-memory Cache API for tests.

function keyOf(request: RequestInfo | URL): string {
  if (typeof request === 'string') return request;
  if (request instanceof URL) return request.pathname + request.search;
  return new URL(request.url).pathname;
}

export class FakeCache {
  readonly entries = new Map<string, Response>();

  async match(request: RequestInfo | URL): Promise<Response | undefined> {
    return this.entries.get(keyOf(request))?.clone();
  }

  async put(request: RequestInfo | URL, response: Response): Promise<void> {
    this.entries.set(keyOf(request), response);
  }

  async delete(request: RequestInfo | URL): Promise<boolean> {
    return this.entries.delete(keyOf(request));
  }

  async keys(): Promise<Request[]> {
    return [...this.entries.keys()].map((url) => new Request(new URL(url, 'http://localhost')));
  }
}

export class FakeCacheStorage {
  readonly caches = new Map<string, FakeCache>();

  async open(name: string): Promise<Cache> {
    let cache = this.caches.get(name);
    if (!cache) {
      cache = new FakeCache();
      this.caches.set(name, cache);
    }
    return cache as unknown as Cache;
  }

  async delete(name: string): Promise<boolean> {
    return this.caches.delete(name);
  }

  async keys(): Promise<string[]> {
    return [...this.caches.keys()];
  }

  async has(name: string): Promise<boolean> {
    return this.caches.has(name);
  }

  /** Test helper: the URLs stored in a cache. */
  urls(name: string): string[] {
    return [...(this.caches.get(name)?.entries.keys() ?? [])];
  }
}
