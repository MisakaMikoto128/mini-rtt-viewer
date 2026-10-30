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

function initExport() {
  $('export-btn').onclick = () => {
    request('/api/logs/save', { method: 'POST' })
      .then(response => response.json())
      .then(({ name }) => {
        playSound('export');
        showToast('已保存到日志文件夹 ' + name, 'ok');
      })
      .catch(() => showToast('导出失败:服务未响应'));
  };
  $('open-logs').onclick = () => {
    request('/api/logs/open', { method: 'POST' })
      .then(() => {
        playSound('open');
        showToast('已打开日志文件夹', 'ok');
      })
      .catch(() => showToast('打开日志文件夹失败'));
  };
}

function initContextMenu(log, selectionInLog) {
  const menu = $('ctx-menu');
  const copyText = text => navigator.clipboard.writeText(text)
    .then(() => {
      playSound('copy');
      showToast('已复制 ' + text.length + ' 字符', 'ok');
    })
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
  $('open-browser').onclick = () => request('/api/open-browser', { method: 'POST' })
    .then(() => {
      playSound('open');
      showToast('已在系统浏览器打开', 'ok');
    })
    .catch(() => showToast('打开浏览器失败'));

  const about = $('about');
  let updateState = null;
  let updateBusy = false;
  let updateOperation = '';
  let updateError = '';
  let restartRequested = false;
  let updateStale = false;
  let updateRequestSeq = 0;

  function renderUpdate(status) {
    updateState = status;
    const phase = status.phase;
    const latest = status.latestVersion;
    const desktop = !!window.__MINI_SHELL;
    const canInstall = phase === 'available';
    const ready = phase === 'readyToRestart';

    $('about-ver').textContent = 'v' + status.currentVersion;
    $('version').textContent = 'v' + status.currentVersion;
    $('version').classList.toggle('update-available', phase === 'available');
    $('version').title = phase === 'available'
      ? '发现新版本 ' + latest
      : '关于 Mini RTT Viewer';

    $('update-check').disabled = updateBusy || updateStale || ready
      || phase === 'checking' || phase === 'installing';
    $('update-install').hidden = !(canInstall || phase === 'installing');
    $('update-install').disabled = updateBusy || updateStale || !canInstall;
    $('update-restart').hidden = !ready || !desktop;
    $('update-restart').disabled = updateBusy || updateStale || !ready || restartRequested;
    $('update-notes').hidden = !status.notesUrl;
    $('update-notes').disabled = updateBusy || !status.notesUrl;
    $('update-notes').dataset.url = status.notesUrl || '';

    let message;
    if (updateBusy) {
      message = updateOperation === 'check' ? '正在检查…'
        : updateOperation === 'install' ? '正在下载并安装…'
        : '正在请求重启…';
    } else if (restartRequested) {
      message = '重启请求已发送';
    } else if (updateError) {
      message = updateError;
    } else if (phase === 'checking') {
      message = '正在检查…';
    } else if (phase === 'available') {
      message = '发现新版本 ' + latest;
    } else if (phase === 'upToDate') {
      message = '当前已是最新版本';
    } else if (phase === 'installing') {
      message = '正在下载并安装…';
    } else if (ready) {
      message = desktop ? '更新已安装，请重启完成更新' : '请手动重启应用';
    } else if (phase === 'error') {
      message = status.message || '更新失败，请重试';
    } else {
      message = '尚未检查更新';
    }
    $('update-status').textContent = message;
    $('update-status').classList.toggle('error', !!updateError || phase === 'error');
    $('update-status').setAttribute('aria-busy', String(updateBusy));
  }

  function fetchUpdateStatus(preserveError = false) {
    const requestSeq = ++updateRequestSeq;
    return getJson('/api/update/status').then(status => {
      // 只应用最后发起的状态读取；显式操作也会使此前在途轮询失效。
      if (requestSeq !== updateRequestSeq) return;
      updateStale = false;
      if (!preserveError || status.phase === 'error') updateError = '';
      if (status.phase !== 'readyToRestart') restartRequested = false;
      renderUpdate(status);
    });
  }

  function refreshUpdate() {
    if (updateBusy) return;
    fetchUpdateStatus().catch(() => {});
  }

  function runUpdate(operation) {
    if (updateBusy || updateStale || !updateState
      || (operation === 'restart' && restartRequested)) return;
    ++updateRequestSeq;
    updateBusy = true;
    updateOperation = operation;
    updateError = '';
    if (operation !== 'restart') restartRequested = false;
    renderUpdate(updateState);

    postJson('/api/update/' + operation).then(response => (
      operation === 'restart' ? null : response.json()
    )).then(status => {
      updateStale = false;
      if (operation === 'restart') restartRequested = true;
      else renderUpdate(status);
    }).catch(error => {
      updateStale = true;
      updateError = error.message || '更新请求失败';
      showToast(updateError);
      return fetchUpdateStatus(true).catch(() => {});
    }).finally(() => {
      updateBusy = false;
      updateOperation = '';
      renderUpdate(updateState);
    });
  }

  $('update-check').onclick = () => runUpdate('check');
  $('update-install').onclick = () => runUpdate('install');
  $('update-restart').onclick = () => runUpdate('restart');
  $('version').onclick = () => {
    if (!updateState) $('about-ver').textContent = $('version').textContent;
    about.showModal();
    playSound('open');
  };
  $('about-close').onclick = () => about.close();
  about.addEventListener('click', event => {
    if (event.target === about) about.close();
  });
  about.querySelectorAll('.about-links button').forEach(button => {
    button.onclick = () => {
      const url = button.dataset.url;
      if (window.__MINI_SHELL) {
        postJson('/api/open-browser', { url })
          .then(() => playSound('open'))
          .catch(() => showToast('打开链接失败'));
      } else {
        window.open(url, '_blank', 'noopener');
        playSound('open');
      }
    };
  });
  refreshUpdate();
  setInterval(refreshUpdate, 2000);
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
      playSound('open');
    }).catch(error => showToast(error.message));
  };
  $('remote-close').onclick = () => dialog.close();
  dialog.addEventListener('click', event => {
    if (event.target === dialog) dialog.close();
  });
  cards.forEach(card => {
    card.onclick = () => navigator.clipboard.writeText(card.dataset.url).then(() => {
      playSound('copy');
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
