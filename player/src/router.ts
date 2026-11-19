// The Player's routes, served under /player/.

import { createRouter } from '@solidjs/router';
import Home from './pages/Home';
import Library from './pages/Library';
import Item from './pages/Item';
import Playlists from './pages/Playlists';
import Playlist from './pages/Playlist';
import Favourites from './pages/Favourites';
import History from './pages/History';
import Downloads from './pages/Downloads';
import Video from './pages/Video';
import Document from './pages/Document';
import Settings from './pages/Settings';
import NotFound from './pages/NotFound';
import AccountRedirect from './pages/AccountRedirect';
import { BASE } from './links';

export const routes = [
  { path: '/', component: Home },
  { path: '/library', component: Library },
  { path: '/item/:id', component: Item },
  { path: '/playlists', component: Playlists },
  { path: '/playlist/:id', component: Playlist },
  { path: '/favourites', component: Favourites },
  { path: '/history', component: History },
  { path: '/downloads', component: Downloads },
  { path: '/video/:id', component: Video },
  { path: '/document/:id', component: Document },
  { path: '/settings', component: Settings },
  { path: '/account/*', component: AccountRedirect },
  { path: '*404', component: NotFound },
];

export const Router = createRouter({ routes, base: BASE });
