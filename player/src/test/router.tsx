import type { Component } from 'solid-js';
import { createRouter, memoryHistory } from '@solidjs/router';

/** A one-route router so components using router hooks can render in tests. */
export function testRouter(component: Component, path = '*any', url = '/') {
  return createRouter({ routes: [{ path, component }], history: memoryHistory(url) });
}
