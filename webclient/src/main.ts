import { mount } from 'svelte';
import './app.css';
import App from './App.svelte';
import { initializeLocale } from './i18n.svelte';

initializeLocale();
export default mount(App, { target: document.getElementById('app')! });
