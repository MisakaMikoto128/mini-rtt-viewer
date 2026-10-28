import { $, getJson, postJson, request } from './api.js';
import { playSound } from './sound.js';

let toastEl = null;
let toastTimer = 0;

export function showToast(message, kind) {
  if (kind !== 'ok') playSound('error');
  if (!toastEl) {
    toastEl = document.createElement('div');
    toastEl.id = 'toast';
    toastEl.setAttribute('role', 'alert');
    document.body.appendChild(toastEl);
  }
  toastEl.textContent = message;
  toastEl.classList.toggle('ok', kind === 'ok');
  toastEl.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toastEl?.classList.remove('show'), 3000);
}

function downloadBlob(blob, name) {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = name;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 5000);
}

function initExport() {
  $('export-btn').onclick = () => {
    request('/api/export')
      .then(async response => {
        const match = (response.headers.get('Content-Disposition') || '').match(/filename="([^"]+)"/);
        return { blob: await response.blob(), name: match ? match[1] : 'rtt.log' };
      })
      .then(({ blob, name }) => {
        downloadBlob(blob, name);
        showToast('已导出 ' + name, 'ok');
      })
      .catch(() => showToast('导出失败:服务未响应'));
  };
}

function initContextMenu(log, selectionInLog) {
  const menu = $('ctx-menu');
  const copyText = text => navigator.clipboard.writeText(text)
    .then(() => showToast('已复制 ' + text.length + ' 字符', 'ok'))
    .catch(() => {});
  let selectedText = '';

  log.addEventListener('contextmenu', event => {
    event.preventDefault();
    if (!menu.matches(':popover-open')) menu.showPopover();
    // Chromium 会在菜单项 mousedown 时塌缩选区，打开菜单时先保存文本。
    selectedText = selectionInLog();
    $('ctx-copy').classList.toggle('disabled', !selectedText);
    menu.style.left = Math.min(event.clientX, window.innerWidth - menu.offsetWidth - 8) + 'px';
    menu.style.top = Math.min(event.clientY, window.innerHeight - menu.offsetHeight - 8) + 'px';
  });
  $('ctx-copy').addEventListener('click', () => {
    if (selectedText) copyText(selectedText);
    menu.hidePopover();
  });
  $('ctx-selectall').addEventListener('click', () => {
    const range = document.createRange();
    range.selectNodeContents(log);
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
    menu.hidePopover();
  });
  log.addEventListener('mouseup', event => {
    if (event.button !== 0) return;
    setTimeout(() => {
      const text = selectionInLog();
      if (text) copyText(text);
    }, 0);
  });
}

function initShellAndAbout() {
  if (window.__MINI_SHELL) {
    const button = $('open-browser');
    button.hidden = false;
    button.onclick = () => request('/api/open-browser', { method: 'POST' })
      .then(() => showToast('已在系统浏览器打开', 'ok'))
      .catch(() => showToast('打开浏览器失败'));
  }

  const about = $('about');
  $('version').onclick = () => {
    $('about-ver').textContent = $('version').textContent;
    about.showModal();
  };
  $('about-close').onclick = () => about.close();
  about.addEventListener('click', event => {
    if (event.target === about) about.close();
  });
  about.querySelectorAll('.about-links button').forEach(button => {
    button.onclick = () => {
      const url = button.dataset.url;
      if (window.__MINI_SHELL) postJson('/api/open-browser', { url }).catch(() => {});
      else window.open(url, '_blank', 'noopener');
    };
  });
}

function initRemoteDialog() {
  const dialog = $('remote');
  const cards = [...dialog.querySelectorAll('.remote-card')];
  $('remote-btn').onclick = () => {
    getJson('/api/addr').then(address => {
      const local = 'http://127.0.0.1:' + address.port;
      $('remote-url-local').textContent = local;
      $('remote-card-local').dataset.url = local;
      const lan = address.open && address.lanIp ? `http://${address.lanIp}:${address.port}` : '';
      $('remote-lan-row').hidden = !lan;
      $('remote-hint').hidden = address.open;
      if (lan) {
        $('remote-url-lan').textContent = lan;
        $('remote-lan-row').dataset.url = lan;
      }
      dialog.showModal();
    }).catch(error => showToast(error.message));
  };
  $('remote-close').onclick = () => dialog.close();
  dialog.addEventListener('click', event => {
    if (event.target === dialog) dialog.close();
  });
  cards.forEach(card => {
    card.onclick = () => navigator.clipboard.writeText(card.dataset.url).then(() => {
      const state = card.querySelector('.remote-copy-state');
      state.classList.add('done');
      clearTimeout(card._copyTimer);
      card._copyTimer = setTimeout(() => state.classList.remove('done'), 1400);
    });
  });
}

export function initDialogs({ log, selectionInLog }) {
  initExport();
  initContextMenu(log, selectionInLog);
  initShellAndAbout();
  initRemoteDialog();
}
