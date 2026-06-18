// Svelte bootstrap. Mounts App into #app and installs the
// step-plugin host contract (`window.__boss_register_step_plugin`).
//
// Plugins are plain-DOM mount functions — no framework runtime is
// shipped by the host. See src/steps/pluginHost.ts for the contract.

import { mount } from 'svelte';
import App from './App.svelte';
import { installStepPluginHost } from './steps/pluginHost';

installStepPluginHost();

const target = document.getElementById('app');
if (!target) throw new Error('#app element missing from index.html');

mount(App, { target });
