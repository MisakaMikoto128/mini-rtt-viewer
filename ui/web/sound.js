import { $, lsGet, lsSet } from './api.js';

// Minimal / Flat:每个反馈只发一只低音量正弦波,不用采样、扫频或和弦。
const tones = {
  connect: { frequency: 660, duration: 0.070, level: 0.030 },
  disconnect: { frequency: 330, duration: 0.070, level: 0.030 },
  send: { frequency: 880, duration: 0.040, level: 0.025 },
  error: { frequency: 220, duration: 0.090, level: 0.030 },
  clear: { frequency: 440, duration: 0.040, level: 0.025 },
  pause: { frequency: 390, duration: 0.045, level: 0.025 },
  resume: { frequency: 590, duration: 0.045, level: 0.025 },
  mark: { frequency: 740, duration: 0.040, level: 0.025 },
  reset: { frequency: 500, duration: 0.080, level: 0.030 },
  export: { frequency: 760, duration: 0.055, level: 0.025 },
  open: { frequency: 620, duration: 0.045, level: 0.025 },
  copy: { frequency: 820, duration: 0.030, level: 0.020 },
  setting: { frequency: 520, duration: 0.035, level: 0.025 },
  adjust: { frequency: 700, duration: 0.030, level: 0.020 },
  enabled: { frequency: 520, duration: 0.035, level: 0.025 },
};

let context = null;
let enabled = true;
let initialized = false;

// AudioContext 只能在用户手势里创建/恢复;页面加载和日志流绝不触碰音频。
export function unlockSound() {
  if (!enabled) return Promise.resolve(false);
  const AudioContextClass = window.AudioContext || window.webkitAudioContext;
  if (!AudioContextClass) return Promise.resolve(false);
  try {
    if (!context) context = new AudioContextClass();
    if (context.state === 'running') return Promise.resolve(true);
    return Promise.resolve(context.resume())
      .then(() => context.state === 'running')
      .catch(() => false);
  } catch {
    return Promise.resolve(false);
  }
}

export function playSound(name) {
  const tone = tones[name];
  if (!enabled || !tone || !context || context.state !== 'running') return;
  const start = context.currentTime;
  const oscillator = context.createOscillator();
  const gain = context.createGain();
  oscillator.type = 'sine';
  oscillator.frequency.setValueAtTime(tone.frequency, start);
  // 4ms 起音 + 指数释音,避免方波式咔哒声;末尾保留 5ms 静音余量。
  gain.gain.setValueAtTime(0.0001, start);
  gain.gain.exponentialRampToValueAtTime(tone.level, start + 0.004);
  gain.gain.exponentialRampToValueAtTime(0.0001, start + tone.duration);
  oscillator.connect(gain);
  gain.connect(context.destination);
  oscillator.onended = () => {
    oscillator.disconnect();
    gain.disconnect();
  };
  oscillator.start(start);
  oscillator.stop(start + tone.duration + 0.005);
}

export function initSound() {
  if (initialized) return;
  initialized = true;
  enabled = lsGet('sound') !== '0';
  const toggle = $('sound-enabled');
  toggle.checked = enabled;
  toggle.addEventListener('change', () => {
    enabled = toggle.checked;
    lsSet('sound', enabled ? '1' : '0');
    if (enabled) unlockSound().then(ok => { if (ok) playSound('enabled'); });
  });
  document.addEventListener('pointerdown', unlockSound, { passive: true });
  document.addEventListener('keydown', event => {
    if (event.key === 'Enter' || event.key === ' ') unlockSound();
  });
}
