import { Show, createSignal, onSettled, useContext } from 'solid-js';
import { useParams } from '@solidjs/router';
import type { MediaFile } from '../api';
import { LISTEN_THRESHOLD_SECONDS } from '../history';
import { to } from '../links';
import { bindMediaSession, createSessionUpdater } from '../player/mediaSession';
import { createRequest } from '../request';
import { ServicesContext } from '../services';
import { ErrorNote, LoadingNote } from '../components/Common';
import { DownloadAllButton } from '../components/DownloadState';
import { HeartButton } from '../components/HeartButton';
import { Icon } from '../components/Icon';
import { Menu } from '../components/Menu';
import {
  SubtitlePicker,
  initialChoice,
  rememberLanguage,
  rememberedLanguage,
  type SubtitleChoice,
} from '../components/SubtitlePicker';
import { trackMenuItems } from '../components/TrackMenu';
import { loadMediaFile } from './media';

function storage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function FilmPlayer(props: { file: MediaFile }) {
  const services = useContext(ServicesContext);
  const { player, history } = services;
  const [choice, setChoice] = createSignal<SubtitleChoice>(
    initialChoice(props.file.subtitles, rememberedLanguage(storage())),
  );
  let video: HTMLVideoElement | undefined;

  function applySubtitles(selected: SubtitleChoice) {
    if (!video) return;
    const tracks = video.textTracks;
    props.file.subtitles.forEach((subtitle, index) => {
      const track = tracks[index];
      if (track) track.mode = subtitle.id === selected ? 'showing' : 'disabled';
    });
  }

  function choose(next: SubtitleChoice) {
    setChoice(next);
    applySubtitles(next);
    const language = next === 'off' ? 'off' : props.file.subtitles.find((subtitle) => subtitle.id === next)?.language;
    if (language) rememberLanguage(storage(), language);
  }

  onSettled(() => {
    const element = video;
    if (!element) return;
    const release = player.claimSessionForVideo();
    const session = player.mediaSession;
    const unbind = session
      ? bindMediaSession(session, {
          play: () => void element.play(),
          pause: () => element.pause(),
          stop: () => {
            element.pause();
            element.currentTime = 0;
          },
          seekBy: (seconds) => {
            element.currentTime = Math.max(0, element.currentTime + seconds);
          },
          seekTo: (seconds) => {
            element.currentTime = seconds;
          },
        })
      : () => {};
    const update = session ? createSessionUpdater(session) : null;
    let recorded = false;
    const sync = () => {
      player.setVideoPlaying(!element.paused && !element.ended);
      update?.({
        file: props.file,
        playing: !element.paused,
        position: element.currentTime,
        duration: element.duration,
      });
      if (!recorded && (element.currentTime >= LISTEN_THRESHOLD_SECONDS || element.ended)) {
        recorded = true;
        void history.record(props.file);
      }
    };
    const events = ['play', 'pause', 'ended', 'seeked', 'loadedmetadata', 'timeupdate'] as const;
    let lastTick = 0;
    const onEvent = (event: Event) => {
      if (event.type === 'timeupdate') {
        const now = Date.now();
        if (now - lastTick < 1000) return;
        lastTick = now;
      }
      if (event.type === 'loadedmetadata') applySubtitles(choice());
      sync();
    };
    for (const type of events) element.addEventListener(type, onEvent);
    applySubtitles(choice());
    return () => {
      for (const type of events) element.removeEventListener(type, onEvent);
      element.pause();
      unbind();
      release();
    };
  });

  return (
    <>
      <div class="video-frame">
        <video
          ref={(el) => (video = el)}
          src={props.file.streamUrl}
          controls
          playsinline
          preload="metadata"
          poster={props.file.cover ?? undefined}
        >
          {props.file.subtitles.map((subtitle) => (
            <track kind="subtitles" src={subtitle.url} srclang={subtitle.language} label={subtitle.label} />
          ))}
        </video>
      </div>
      <div class="video-meta">
        <div class="now-titles">
          <h1 class="now-title">{props.file.title}</h1>
          <p class="now-sub">
            <a href={to.item(props.file.itemId)}>
              {props.file.creator}
              <Show when={props.file.itemTitle && props.file.itemTitle !== props.file.title}>
                {' '}
                — {props.file.itemTitle}
              </Show>
            </a>
          </p>
        </div>
        <HeartButton file={props.file} />
        <DownloadAllButton files={[props.file]} />
        <Menu label={`More options for “${props.file.title}”`} items={trackMenuItems(services, props.file)} />
      </div>
      <Show when={props.file.subtitles.length > 0}>
        <SubtitlePicker subtitles={props.file.subtitles} choice={choice()} onChange={choose} />
      </Show>
    </>
  );
}

export default function Video() {
  const services = useContext(ServicesContext);
  const params = useParams();
  const file = createRequest(
    () => (Number.isInteger(Number(params.id)) ? Number(params.id) : null),
    (id) => loadMediaFile(services, id),
  );
  return (
    <div class="page video-page">
      <a
        class="back-link"
        href={to.library()}
        onClick={(event) => {
          if (window.history.length > 1) {
            event.preventDefault();
            window.history.back();
          }
        }}
      >
        <Icon name="chevronLeft" size={20} /> Back
      </a>
      <Show
        when={file.data()}
        keyed
        fallback={
          <Show when={file.error()} fallback={<LoadingNote />}>
            <ErrorNote error={file.error()} onRetry={file.reload} />
          </Show>
        }
      >
        {(loaded) => <FilmPlayer file={loaded} />}
      </Show>
    </div>
  );
}
