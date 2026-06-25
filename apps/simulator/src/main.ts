// Svelte bootstrap for the Simulator UX. Mounts App into #app.
//
// No step-plugin host here — the Simulator app is a self-contained
// cockpit + controls surface served under /simulator; it does not
// render Step plugins the way apps/web does.

import { mount } from 'svelte';
import App from './App.svelte';

const target = document.getElementById('app');
if (!target) throw new Error('#app element missing from index.html');

mount(App, { target });
