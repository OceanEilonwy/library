import { Show, useContext } from 'solid-js';
import { useParams } from '@solidjs/router';
import { to } from '../links';
import { createRequest } from '../request';
import { ServicesContext } from '../services';
import { ErrorNote, LoadingNote } from '../components/Common';
import { HeartButton } from '../components/HeartButton';
import { Icon } from '../components/Icon';
import { loadMediaFile } from './media';

export default function Document() {
  const services = useContext(ServicesContext);
  const params = useParams();
  const file = createRequest(
    () => (Number.isInteger(Number(params.id)) ? Number(params.id) : null),
    (id) => loadMediaFile(services, id),
  );
  return (
    <div class="page document-page">
      <Show
        when={file.data()}
        fallback={
          <Show when={file.error()} fallback={<LoadingNote />}>
            <ErrorNote error={file.error()} onRetry={file.reload} />
          </Show>
        }
      >
        {(doc) => (
          <>
            <header class="document-bar">
              <a class="back-link" href={to.item(doc().itemId)}>
                <Icon name="chevronLeft" size={20} /> {doc().itemTitle}
              </a>
              <h1 class="document-title">{doc().title}</h1>
              <div class="row-actions">
                <HeartButton file={doc()} />
                <a class="button" href={doc().streamUrl} download target="_self">
                  <Icon name="download" size={20} /> Download
                </a>
                <a class="button ghost" href={doc().streamUrl} target="_blank" rel="noopener">
                  Open in a new tab
                </a>
              </div>
            </header>
            <iframe class="document-frame" src={doc().streamUrl} title={doc().title} />
          </>
        )}
      </Show>
    </div>
  );
}
