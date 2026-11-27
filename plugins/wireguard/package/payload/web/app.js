import { pluginUI } from './plugin-ui.mjs';

const ui = pluginUI();
const $ = (id) => document.getElementById(id);

let settingsDirty = false;
let currentConfig = '';
let currentPeerName = '';
let toastTimer = 0;
let firstLoad = true;

async function call(name, args = {}, timeoutMs = 30000) {
  const text = await ui.invoke(name, args, { timeoutMs });
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

function toast(message, kind = 'ok') {
  const el = $('toast');
  el.textContent = message;
  el.dataset.kind = kind;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    el.hidden = true;
  }, kind === 'error' ? 6000 : 3000);
}

function setBusy(busy) {
  for (const id of ['save', 'add']) $(id).disabled = busy;
}

function implLabel(impl) {
  if (impl === 'kernel') return '内核模块';
  if (impl === 'userspace') return 'gotatun';
  return '后端未知';
}

function statusPill(status) {
  const dot = $('status-dot');
  const text = $('status-text');
  if (!status || !status.interface_up) {
    dot.className = 'dot bad';
    text.textContent = '接口未运行';
    return;
  }
  if (!status.guardian) {
    dot.className = 'dot warn';
    text.textContent = '接口在，看守循环未运行';
    return;
  }
  dot.className = 'dot ok';
  text.textContent = `运行中 · ${status.active_peers} 个活动 peer · ${implLabel(status.impl)}`;
}

function allowedMode(value) {
  const normalized = String(value || '').replace(/\s+/g, '');
  if (normalized === '0.0.0.0/0,::/0' || normalized === '0.0.0.0/0,0:0:0:0:0:0:0:0/0') {
    return 'all';
  }
  return 'custom';
}

function syncAllowedMode() {
  const mode = $('allowed_ips_mode').value;
  const input = $('allowed_ips');
  if (mode === 'all') {
    input.value = '0.0.0.0/0,::/0';
    input.disabled = true;
  } else {
    input.disabled = false;
  }
}

function fillSettings(s) {
  $('lan_interface').value = s.lan_interface || '';
  $('port').value = s.port ?? '';
  $('network').value = s.network || '';
  $('address').value = s.address || '';
  $('mtu').value = s.mtu ?? '';
  $('table').value = s.table ?? '';
  $('endpoint').value = s.endpoint || '';
  $('dns').value = s.dns || '';
  $('keepalive').value = s.keepalive ?? 25;
  $('allowed_ips_mode').value = allowedMode(s.allowed_ips);
  $('allowed_ips').value = s.allowed_ips || '';
  syncAllowedMode();
}

function readSettings() {
  const number = (id) => {
    const value = Number.parseInt($(id).value, 10);
    return Number.isFinite(value) ? value : NaN;
  };
  const body = {
    lan_interface: $('lan_interface').value.trim(),
    port: number('port'),
    network: $('network').value.trim(),
    address: $('address').value.trim(),
    mtu: number('mtu'),
    table: number('table'),
    endpoint: $('endpoint').value.trim(),
    dns: $('dns').value.trim(),
    allowed_ips: $('allowed_ips_mode').value === 'all'
      ? '0.0.0.0/0,::/0'
      : $('allowed_ips').value.trim().replace(/\s+/g, ''),
    keepalive: number('keepalive'),
  };
  for (const [key, value] of Object.entries(body)) {
    if (value === '' && ['dns', 'endpoint'].includes(key)) continue;
    if (typeof value === 'number' && Number.isNaN(value)) {
      toast(`${key} 需要填数字`, 'error');
      return null;
    }
    if (typeof value === 'string' && !value) {
      toast(`${key} 不能为空`, 'error');
      return null;
    }
  }
  if (!body.allowed_ips) {
    toast('AllowedIPs 不能为空', 'error');
    return null;
  }
  return body;
}

function endpointWarning(s) {
  const warn = $('endpoint-warning');
  const endpoint = s.settings?.endpoint || '';
  warn.hidden = Boolean(endpoint);
}

function renderPeers(peers) {
  const body = $('peers');
  body.replaceChildren();
  $('peer-count').textContent = peers.length ? `（${peers.length}）` : '';
  if (!peers.length) {
    const row = document.createElement('tr');
    const cell = document.createElement('td');
    cell.colSpan = 4;
    cell.className = 'empty';
    cell.textContent = '还没有 peer，先在上面添加一个。';
    row.append(cell);
    body.append(row);
    return;
  }
  for (const peer of peers) {
    const row = document.createElement('tr');

    const name = document.createElement('td');
    name.textContent = peer.name || `peer ${peer.id}`;
    row.append(name);

    const address = document.createElement('td');
    address.className = 'mono';
    address.textContent = peer.address;
    row.append(address);

    const pub = document.createElement('td');
    pub.className = 'pub mono clip';
    pub.textContent = peer.public_key;
    pub.title = peer.public_key;
    row.append(pub);

    const actions = document.createElement('td');
    actions.style.whiteSpace = 'nowrap';
    const show = document.createElement('button');
    show.type = 'button';
    show.textContent = '二维码';
    show.dataset.action = 'config';
    show.dataset.id = String(peer.id);
    show.dataset.name = peer.name || `peer-${peer.id}`;
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.className = 'danger';
    remove.textContent = '删除';
    remove.style.marginLeft = '6px';
    remove.dataset.action = 'remove';
    remove.dataset.id = String(peer.id);
    remove.dataset.name = peer.name || `peer-${peer.id}`;
    actions.append(show, remove);
    row.append(actions);

    body.append(row);
  }
}

function showConfig(name, config) {
  currentConfig = config;
  currentPeerName = name;
  $('dialog-title').textContent = `${name} 的客户端配置`;
  $('dialog-sub').textContent = '用 WireGuard 客户端扫下面的二维码，或复制 / 下载配置。';
  $('config-text').textContent = config;

  const box = $('qr');
  box.replaceChildren();
  try {
    const qr = qrcode(0, 'M');
    qr.addData(config);
    qr.make();
    const holder = document.createElement('div');
    holder.innerHTML = qr.createSvgTag(4, 4);
    box.append(...holder.childNodes);
  } catch (error) {
    const p = document.createElement('p');
    p.className = 'muted';
    p.textContent = `二维码生成失败：${error.message}`;
    box.append(p);
  }
  const dialog = $('peer-dialog');
  if (dialog.open) dialog.close();
  dialog.showModal();
}

async function openPeer(id, name) {
  try {
    const config = await call('peer_config', { peer_id: Number(id) });
    if (typeof config !== 'string' || !config.trim()) {
      throw new Error('配置为空');
    }
    showConfig(name, config);
  } catch (error) {
    toast(`读取配置失败：${error.message}`, 'error');
  }
}

async function removePeer(id, name) {
  if (!confirm(`删除 peer “${name}”？客户端会立即断开。`)) return;
  setBusy(true);
  try {
    await call('remove_peer', { peer_id: Number(id) });
    toast('已删除');
    await refresh();
  } catch (error) {
    toast(`删除失败：${error.message}`, 'error');
  } finally {
    setBusy(false);
  }
}

async function refresh() {
  try {
    const state = await call('state');
    if (!state || typeof state !== 'object') throw new Error('返回格式不对');
    statusPill(state.status);
    $('stat-iface').textContent = state.status?.interface_up ? 'wg0' : '—';
    $('stat-port').textContent = state.settings?.port ?? '—';
    $('stat-peers').textContent = state.status?.active_peers ?? 0;
    $('stat-pub').textContent = state.server_public_key || '尚未生成';
    $('stat-pub').title = state.server_public_key || '';
    endpointWarning(state);
    if (!settingsDirty) {
      fillSettings(state.settings || {});
      if (firstLoad) settingsDirty = false;
    }
    renderPeers(state.peers || []);
    firstLoad = false;
  } catch (error) {
    $('status-dot').className = 'dot bad';
    $('status-text').textContent = '读取失败';
    if (firstLoad) toast(`读取设置失败：${error.message}`, 'error');
    firstLoad = false;
  }
}

$('settings-form').addEventListener('input', () => {
  settingsDirty = true;
});
$('allowed_ips_mode').addEventListener('change', () => {
  settingsDirty = true;
  if ($('allowed_ips_mode').value === 'custom' && $('allowed_ips').value === '0.0.0.0/0,::/0') {
    $('allowed_ips').value = '';
  }
  syncAllowedMode();
});

$('settings-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  const body = readSettings();
  if (!body) return;
  setBusy(true);
  try {
    const result = await call('save_settings', body, 90000);
    settingsDirty = false;
    toast(result?.restarted ? '已保存，隧道已重建' : '已保存');
    await refresh();
  } catch (error) {
    toast(`保存失败：${error.message}`, 'error');
  } finally {
    setBusy(false);
  }
});

$('add-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  const name = $('peer-name').value.trim();
  if (!name) return;
  setBusy(true);
  try {
    const result = await call('add_peer', { peer_name: name }, 30000);
    if (typeof result === 'string') throw new Error(result);
    toast('已添加 peer');
    $('peer-name').value = '';
    await refresh();
    showConfig(result.peer.name, result.config);
  } catch (error) {
    toast(`添加失败：${error.message}`, 'error');
  } finally {
    setBusy(false);
  }
});

$('peers').addEventListener('click', (event) => {
  const button = event.target.closest('button[data-action]');
  if (!button) return;
  const { action, id, name } = button.dataset;
  if (action === 'config') openPeer(id, name);
  if (action === 'remove') removePeer(id, name);
});

$('close-dialog').addEventListener('click', () => $('peer-dialog').close());
$('peer-dialog').addEventListener('click', (event) => {
  if (event.target === $('peer-dialog')) $('peer-dialog').close();
});

$('copy-config').addEventListener('click', async () => {
  try {
    await navigator.clipboard.writeText(currentConfig);
    toast('配置已复制');
  } catch {
    const area = document.createElement('textarea');
    area.value = currentConfig;
    document.body.append(area);
    area.select();
    document.execCommand('copy');
    area.remove();
    toast('配置已复制');
  }
});

$('download-config').addEventListener('click', () => {
  const safe = (currentPeerName || 'peer').replace(/[^\w.-]+/g, '_');
  const blob = new Blob([currentConfig], { type: 'text/plain' });
  const href = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = href;
  anchor.download = `${safe}.conf`;
  anchor.click();
  setTimeout(() => URL.revokeObjectURL(href), 1000);
});

$('stat-pub').addEventListener('click', async () => {
  const pub = $('stat-pub').textContent;
  if (!pub || pub === '尚未生成') return;
  try {
    await navigator.clipboard.writeText(pub);
    toast('服务端公钥已复制');
  } catch {
    toast('复制失败', 'error');
  }
});

syncAllowedMode();
refresh();
setInterval(() => {
  if (document.hidden) return;
  refresh();
}, 8000);
