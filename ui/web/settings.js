import { $, getJson, lsGet, lsSet, request } from './api.js';
import { playSound } from './sound.js';

export function textToHex(text) {
  const out = [];
  for (const byte of new TextEncoder().encode(text)) {
    out.push(byte.toString(16).toUpperCase().padStart(2, '0'));
  }
  return out.join(' ');
}

export function hexToText(text) {
  const cleaned = text.split(/[\s:\-]+/).map(token => token.replace(/^0[xX]/, '')).join('');
  if (!cleaned.length || cleaned.length % 2 || !/^[0-9a-fA-F]+$/.test(cleaned)) return null;
  const bytes = new Uint8Array(cleaned.length / 2);
  for (let i = 0; i < bytes.length; i++) bytes[i] = parseInt(cleaned.slice(i * 2, i * 2 + 2), 16);
  return new TextDecoder().decode(bytes);
}

export function initDisplaySettings() {
  let logFontSize = +(lsGet('log-fs') || localStorage.getItem('web-log-fs') || 14);
  const applyFontSize = () => {
    document.documentElement.style.setProperty('--log-fs', logFontSize + 'px');
    document.documentElement.style.setProperty('--log-lh', Math.round(logFontSize * 1.5) + 'px');
    lsSet('log-fs', String(logFontSize));
  };
  $('A-minus').onclick = () => {
    const next = Math.max(9, logFontSize - 1);
    if (next === logFontSize) return;
    logFontSize = next;
    applyFontSize();
    playSound('adjust');
  };
  $('A-plus').onclick = () => {
    const next = Math.min(30, logFontSize + 1);
    if (next === logFontSize) return;
    logFontSize = next;
    applyFontSize();
    playSound('adjust');
  };
  applyFontSize();

  const themeSelect = $('theme');
  const builtinThemes = [
    'dark', 'light', 'oled', 'sepia',
    'instrument', 'console', 'laboratory',
  ];
  let customThemeStyle = null;
  const setCustomThemeCss = css => {
    customThemeStyle?.remove();
    customThemeStyle = null;
    if (css != null) {
      customThemeStyle = document.createElement('style');
      customThemeStyle.setAttribute('data-custom-theme', '');
      customThemeStyle.textContent = css;
      document.head.appendChild(customThemeStyle);
    }
  };
  let themeGeneration = 0;
  let currentTheme = themeSelect.value;
  const applyTheme = id => {
    currentTheme = id;
    const generation = ++themeGeneration;
    if (builtinThemes.includes(id)) {
      document.documentElement.dataset.theme = id;
      setCustomThemeCss(null);
      return;
    }
    request('/api/theme-css/' + encodeURIComponent(id))
      .then(response => response.text())
      .then(css => {
        if (generation !== themeGeneration || currentTheme !== id) return;
        document.documentElement.removeAttribute('data-theme');
        setCustomThemeCss(css);
      })
      .catch(() => {
        if (generation !== themeGeneration || currentTheme !== id) return;
        document.documentElement.dataset.theme = 'dark';
        setCustomThemeCss(null);
      });
  };
  themeSelect.onchange = () => {
    localStorage.setItem('web-theme', themeSelect.value);
    Promise.resolve(applyTheme(themeSelect.value)).then(() => playSound('setting'));
  };
  const savedTheme = localStorage.getItem('web-theme') || 'dark';
  if (builtinThemes.includes(savedTheme)) themeSelect.value = savedTheme;
  applyTheme(savedTheme);

  getJson('/api/themes').then(list => {
    for (const theme of list) {
      if (builtinThemes.includes(theme.id)) continue;
      const option = document.createElement('option');
      option.value = theme.id;
      option.textContent = theme.name;
      themeSelect.appendChild(option);
    }
    const saved = localStorage.getItem('web-theme') || 'dark';
    if (!builtinThemes.includes(saved)) {
      themeSelect.value = saved;
      if (themeSelect.value !== saved) {
        themeSelect.value = 'dark';
        localStorage.setItem('web-theme', 'dark');
      }
    }
  }).catch(() => {});
}
