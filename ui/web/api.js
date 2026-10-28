export const $ = id => document.getElementById(id);
export const API = location.origin;

// 仅显示偏好使用 localStorage；业务设置由服务端 /api/prefs 持久化。
export const lsGet = key => localStorage.getItem('web-' + key);
export const lsSet = (key, value) => localStorage.setItem('web-' + key, value);

export async function request(path, { method = 'GET', body } = {}) {
  const response = await fetch(API + path, {
    method,
    headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    const message = await response.text();
    throw new Error(message || `HTTP ${response.status}`);
  }
  return response;
}

export const getJson = path => request(path).then(response => response.json());
export const postJson = (path, body) => request(path, { method: 'POST', body });
