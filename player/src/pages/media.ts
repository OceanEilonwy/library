import { isApiError, type MediaFile } from '../api';
import type { Services } from '../services';

/** Loads a file's details, falling back to the downloaded copy when offline. */
export async function loadMediaFile(services: Services, id: number): Promise<MediaFile> {
  try {
    return await services.api.media(id);
  } catch (error) {
    if (isApiError(error) && error.kind === 'offline') {
      const record = await services.downloads.store.get(id);
      if (record) return record.file;
    }
    throw error;
  }
}
